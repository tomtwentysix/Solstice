//! Notices when a yabridge host process exits, so its plug-ins can stop being called.
//! yabridge's Linux half waits for each reply from the host with no time limit.

use std::os::fd::RawFd;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
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
            let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
            Some(parse_child(pid, &cmdline, &stat))
        })
        .collect()
}

/// A process that has exited but not been reaped has an empty command line; its name, cut to
/// 15 characters, is still in `stat` between the parentheses.
fn parse_child(pid: i32, cmdline: &[u8], stat: &str) -> Child {
    let mut args: Vec<String> =
        cmdline.split(|b| *b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
    if args.is_empty()
        && let (Some(a), Some(b)) = (stat.find('('), stat.rfind(')'))
    {
        args.push(stat[a + 1..b].to_string());
    }
    Child { pid, args }
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

/// yabridge starts one host when it loads a plug-in's module, and every instance from that
/// module shares it. Runs `open` and returns the hosts serving `module`: those that started
/// during `open`, or else the running host whose socket is named after the module.
pub fn hosts_for<T>(module: &Path, open: impl FnOnce() -> T) -> (T, Vec<i32>) {
    let (out, new) = opening(open);
    if !new.is_empty() {
        return (out, new);
    }
    (out, serving(module, &children()))
}

/// Bridge hosts among `kids` whose socket, `yabridge-<module name>-<random>`, names `module`.
pub fn serving(module: &Path, kids: &[Child]) -> Vec<i32> {
    let Some(stem) = module.file_stem().map(|s| format!("yabridge-{}-", s.to_string_lossy())) else { return Vec::new() };
    kids.iter()
        .filter(|c| is_bridge_host(c))
        .filter(|c| {
            c.args.iter().any(|a| {
                let name = Path::new(a).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                name.strip_prefix(&stem).is_some_and(|rest| !rest.is_empty() && rest.chars().all(|ch| ch.is_ascii_alphanumeric()))
            })
        })
        .map(|c| c.pid)
        .collect()
}

/// Copies a plug-in bundle to a new folder under `under`, keeping its symlinks. yabridge keeps
/// one bridge per loaded module path, and after its host dies that bridge throws on its own
/// threads if the module is opened again, so a reopen loads a copy instead.
pub fn fresh_copy(bundle: &Path, under: &Path) -> std::io::Result<PathBuf> {
    fn copy(from: &Path, to: &Path) -> std::io::Result<()> {
        let meta = std::fs::symlink_metadata(from)?;
        if meta.file_type().is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(from)?, to)
        } else if meta.is_dir() {
            std::fs::create_dir_all(to)?;
            for e in std::fs::read_dir(from)? {
                let e = e?;
                copy(&e.path(), &to.join(e.file_name()))?;
            }
            Ok(())
        } else {
            std::fs::copy(from, to).map(|_| ())
        }
    }
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let name = bundle.file_name().ok_or_else(|| std::io::Error::other("not a bundle"))?;
    let dir = under.join(NEXT.fetch_add(1, Relaxed).to_string());
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let to = dir.join(name);
    copy(bundle, &to)?;
    Ok(to)
}

/// Which module paths lost their host, and the copy each is opened from now.
#[derive(Default)]
pub struct Bridges {
    dead: HashSet<PathBuf>,
    copies: HashMap<PathBuf, PathBuf>,
}

impl Bridges {
    pub fn died(&mut self, path: &Path) {
        self.dead.insert(path.to_path_buf());
    }

    pub fn has_died(&self, path: &Path) -> bool {
        self.dead.contains(path)
    }

    /// The path to load `module` from: itself until its host dies, then a live copy.
    pub fn load_path(&mut self, module: &Path, under: &Path) -> std::io::Result<PathBuf> {
        if !self.dead.contains(module) {
            return Ok(module.to_path_buf());
        }
        if let Some(c) = self.copies.get(module).filter(|c| !self.dead.contains(*c)) {
            return Ok(c.clone());
        }
        let c = fresh_copy(module, under)?;
        self.copies.insert(module.to_path_buf(), c.clone());
        Ok(c)
    }
}

/// Where this process keeps its bundle copies.
pub fn copies_dir() -> PathBuf {
    std::env::temp_dir().join(format!("solstice-bridges-{}", std::process::id()))
}

/// Removes bundle copies left by Solstice processes that have exited.
pub fn clean_copies() {
    let Ok(rd) = std::fs::read_dir(std::env::temp_dir()) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(pid) = name.strip_prefix("solstice-bridges-").and_then(|p| p.parse::<u32>().ok()) else { continue };
        if !Path::new(&format!("/proc/{pid}")).exists() {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
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
    fn a_running_host_is_found_by_its_socket_name() {
        let host = |pid, plugin: &str| Child {
            pid,
            args: vec![
                "/u/.local/share/yabridge/yabridge-host.exe.so".into(),
                "VST3".into(),
                format!("C:/VST3/{plugin}.vst3"),
                format!("/run/user/1000/yabridge-{plugin}-AbC123xy"),
                "100".into(),
            ],
        };
        let kids = [host(20, "FabFilter Pro-Q 4"), host(21, "FabFilter Pro-DS"), child(22, "/usr/bin/solstice-scan")];
        let module = Path::new("/u/.vst3/yabridge/FabFilter/FabFilter Pro-DS.vst3");
        assert_eq!(serving(module, &kids), vec![21]);
        assert!(serving(Path::new("/u/.vst3/yabridge/FabFilter/FabFilter Pro.vst3"), &kids).is_empty());
    }

    #[test]
    fn a_fresh_copy_keeps_the_bundle_and_its_links() {
        let root = std::env::temp_dir().join(format!("solstice-test-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bundle = root.join("Pro-DS.vst3");
        std::fs::create_dir_all(bundle.join("Contents/x86_64-linux")).unwrap();
        std::fs::create_dir_all(bundle.join("Contents/x86_64-win")).unwrap();
        std::fs::write(bundle.join("Contents/x86_64-linux/Pro-DS.so"), b"elf").unwrap();
        std::os::unix::fs::symlink("/wine/Pro-DS.vst3", bundle.join("Contents/x86_64-win/Pro-DS.vst3")).unwrap();
        let copy = fresh_copy(&bundle, &root.join("copies")).unwrap();
        assert_ne!(copy, bundle);
        assert_eq!(copy.file_name(), bundle.file_name());
        assert_eq!(std::fs::read(copy.join("Contents/x86_64-linux/Pro-DS.so")).unwrap(), b"elf");
        assert_eq!(std::fs::read_link(copy.join("Contents/x86_64-win/Pro-DS.vst3")).unwrap(), Path::new("/wine/Pro-DS.vst3"));
        assert_ne!(fresh_copy(&bundle, &root.join("copies")).unwrap(), copy, "each copy is new");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_module_whose_host_died_opens_from_a_copy() {
        let root = std::env::temp_dir().join(format!("solstice-test-load-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bundle = root.join("Saturn.vst3");
        std::fs::create_dir_all(bundle.join("Contents")).unwrap();
        let mut b = Bridges::default();
        assert_eq!(b.load_path(&bundle, &root).unwrap(), bundle);
        b.died(&bundle);
        let copy = b.load_path(&bundle, &root).unwrap();
        assert_ne!(copy, bundle);
        assert_eq!(b.load_path(&bundle, &root).unwrap(), copy, "the copy is reused while it lives");
        b.died(&copy);
        let again = b.load_path(&bundle, &root).unwrap();
        assert!(again != copy && again != bundle);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_host_that_already_exited_is_still_recognised() {
        let c = parse_child(30, b"", "30 (yabridge-host.e) Z 1 30 30 0 -1");
        assert!(is_bridge_host(&c));
        let c = parse_child(31, b"", "31 (sleep) Z 1 31 31 0 -1");
        assert!(!is_bridge_host(&c));
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
