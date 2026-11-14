//! Notices when a yabridge host process exits, so its plug-ins can stop being called.
//! yabridge's Linux half waits for each reply from the host with no time limit.

use std::os::fd::RawFd;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock};

pub struct Child {
    pub pid: i32,
    pub args: Vec<String>,
}

pub fn children() -> Vec<Child> {
    let mut pids: Vec<i32> = Vec::new();
    if let Ok(tasks) = std::fs::read_dir("/proc/self/task") {
        for t in tasks.flatten() {
            if let Ok(s) = std::fs::read_to_string(t.path().join("children")) {
                pids.extend(s.split_whitespace().filter_map(|p| p.parse::<i32>().ok()));
            }
        }
    }
    pids.sort();
    pids.dedup();
    pids.into_iter()
        .filter_map(|pid| {
            let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
            let args = raw.split(|b| *b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
            Some(Child { pid, args })
        })
        .collect()
}

pub fn is_bridge_host(c: &Child) -> bool {
    let Some(exe) = c.args.first() else { return false };
    let name = Path::new(exe).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    name.starts_with("yabridge-host") || name.starts_with("yabridge-group")
}

pub fn new_hosts(before: &[Child], after: &[Child]) -> Vec<i32> {
    after.iter().filter(|c| is_bridge_host(c) && !before.iter().any(|b| b.pid == c.pid)).map(|c| c.pid).collect()
}

/// Runs `open` and returns its result with the bridge hosts that appeared while it ran.
pub fn opening<T>(open: impl FnOnce() -> T) -> (T, Vec<i32>) {
    let before = children();
    let out = open();
    (out, new_hosts(&before, &children()))
}

struct Entry {
    pidfd: RawFd,
    pid: i32,
    flags: Vec<Arc<AtomicBool>>,
}

static ENTRIES: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
static WAKE: OnceLock<RawFd> = OnceLock::new();

fn wake_fd() -> RawFd {
    *WAKE.get_or_init(|| {
        let mut fds = [-1; 2];
        unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
        let _ = std::thread::Builder::new().name("solstice-hostwatch".into()).spawn(move || run(fds[0]));
        fds[1]
    })
}

/// Sets `dead` once `pid` exits. A pid already watched, such as a yabridge group host, gains
/// the flag alongside the others.
pub fn watch(pid: i32, dead: Arc<AtomicBool>) {
    let wake = wake_fd();
    let mut entries = ENTRIES.lock().unwrap();
    if let Some(e) = entries.iter_mut().find(|e| e.pid == pid) {
        e.flags.push(dead);
        return;
    }
    let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, libc::PIDFD_NONBLOCK) } as RawFd;
    if pidfd < 0 {
        dead.store(true, Relaxed);
        return;
    }
    entries.push(Entry { pidfd, pid, flags: vec![dead] });
    drop(entries);
    unsafe { libc::write(wake, [0u8].as_ptr().cast(), 1) };
}

fn run(wake: RawFd) {
    let mut buf = [0u8; 64];
    loop {
        let mut fds = vec![libc::pollfd { fd: wake, events: libc::POLLIN, revents: 0 }];
        fds.extend(ENTRIES.lock().unwrap().iter().map(|e| libc::pollfd { fd: e.pidfd, events: libc::POLLIN, revents: 0 }));
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) } < 0 {
            continue;
        }
        if fds[0].revents != 0 {
            while unsafe { libc::read(wake, buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
        }
        let exited: Vec<RawFd> = fds[1..].iter().filter(|p| p.revents != 0).map(|p| p.fd).collect();
        if exited.is_empty() {
            continue;
        }
        ENTRIES.lock().unwrap().retain(|e| {
            if !exited.contains(&e.pidfd) {
                return true;
            }
            e.flags.iter().for_each(|f| f.store(true, Relaxed));
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // Reaps a host Solstice started; fails harmlessly for one it did not.
            unsafe {
                libc::waitid(libc::P_PIDFD, e.pidfd as libc::id_t, &mut info, libc::WEXITED | libc::WNOHANG);
                libc::close(e.pidfd);
            }
            false
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
    use std::time::{Duration, Instant};

    fn child(pid: i32, exe: &str) -> Child {
        Child { pid, args: vec![exe.to_string()] }
    }

    fn within(flags: &[&AtomicBool], limit: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < limit {
            if flags.iter().all(|f| f.load(Relaxed)) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn new_hosts_picks_only_new_bridge_hosts() {
        let before = [child(10, "yabridge-host.exe.so")];
        let after = [
            child(10, "yabridge-host.exe.so"),
            child(11, "/usr/bin/solstice-scan"),
            child(12, "/home/u/.local/share/yabridge/yabridge-host.exe.so"),
        ];
        assert_eq!(new_hosts(&before, &after), vec![12]);
    }

    #[test]
    fn group_hosts_are_bridge_hosts() {
        assert!(is_bridge_host(&child(1, "/x/yabridge-group.exe.so")));
        assert!(!is_bridge_host(&child(1, "/usr/bin/wine")));
    }

    #[test]
    fn watched_child_exit_sets_the_flag_and_is_reaped() {
        let c = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let pid = c.id() as i32;
        let (a, b) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        watch(pid, a.clone());
        watch(pid, b.clone());
        unsafe { libc::kill(pid, libc::SIGKILL) };
        assert!(within(&[&a, &b], Duration::from_secs(2)));
        let start = Instant::now();
        while std::path::Path::new(&format!("/proc/{pid}")).exists() && start.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    }

    #[test]
    fn watching_a_dead_pid_sets_the_flag_at_once() {
        let c = std::process::Command::new("true").spawn().unwrap();
        let pid = c.id() as i32;
        unsafe { libc::waitpid(pid, std::ptr::null_mut(), 0) };
        let f = Arc::new(AtomicBool::new(false));
        watch(pid, f.clone());
        assert!(within(&[&f], Duration::from_secs(2)));
    }
}
