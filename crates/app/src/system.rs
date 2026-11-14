#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RtLimit {
    Unlimited,
    Limit(u32),
}

pub fn parse_rtprio(proc_limits: &str) -> Option<RtLimit> {
    let rest = proc_limits
        .lines()
        .find_map(|l| l.strip_prefix("Max realtime priority"))?;
    match rest.split_whitespace().next()? {
        "unlimited" => Some(RtLimit::Unlimited),
        n => n.parse().ok().map(RtLimit::Limit),
    }
}

pub fn realtime_ok(limit: Option<RtLimit>) -> bool {
    match limit {
        Some(RtLimit::Unlimited) => true,
        Some(RtLimit::Limit(n)) => n > 0,
        None => false,
    }
}

pub fn realtime_limit() -> Option<RtLimit> {
    parse_rtprio(&std::fs::read_to_string("/proc/self/limits").ok()?)
}

/// Keeps the CPU out of idle states slower to leave than the limit for as long as it is held.
/// Each bridged plug-in wakes another process every period, and the deep states on recent CPUs
/// take up to 700 µs to leave, which is a quarter of a 128-frame period.
pub struct WakeHold(#[allow(dead_code)] std::fs::File);

pub const WAKE_DEVICE: &str = "/dev/cpu_dma_latency";
pub const WAKE_LIMIT_US: i32 = 10;

// The kernel takes the limit as a native-endian s32 and drops the request when the file closes.
pub fn hold_wake_at(path: &std::path::Path, limit_us: i32) -> std::io::Result<WakeHold> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().write(true).open(path)?;
    f.write_all(&limit_us.to_ne_bytes())?;
    Ok(WakeHold(f))
}

pub fn hold_wake() -> std::io::Result<WakeHold> {
    hold_wake_at(std::path::Path::new(WAKE_DEVICE), WAKE_LIMIT_US)
}

// limits.conf lines are `domain type item value`; only `@group` rtprio grants count.
pub fn rt_group(files: &[String]) -> Option<(String, u32)> {
    let mut best: Option<(String, u32)> = None;
    for line in files.iter().flat_map(|f| f.lines()) {
        let f: Vec<&str> = line.split_whitespace().collect();
        let [domain, kind, "rtprio", value] = f[..] else { continue };
        let Some(group) = domain.strip_prefix('@') else { continue };
        if kind != "-" && kind != "soft" {
            continue;
        }
        let Ok(n) = value.parse::<u32>() else { continue };
        if n > 0 && best.as_ref().is_none_or(|(_, b)| n > *b) {
            best = Some((group.to_string(), n));
        }
    }
    best
}

pub fn rt_group_on_system() -> Option<(String, u32)> {
    let mut files = vec![std::fs::read_to_string("/etc/security/limits.conf").unwrap_or_default()];
    let mut dropins: Vec<_> = std::fs::read_dir("/etc/security/limits.d")
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "conf"))
        .collect();
    dropins.sort();
    files.extend(dropins.iter().filter_map(|p| std::fs::read_to_string(p).ok()));
    rt_group(&files)
}

const KNOWN_GOOD: &[&str] = &["ba7022d"];
const POPUPS_BREAK_FROM: (u32, u32) = (9, 22);
const FIXED_RELEASE: (u32, u32, u32) = (5, 2, 0);

pub fn wine_version(output: &str) -> Option<(u32, u32)> {
    let v = output.trim().strip_prefix("wine-")?;
    let mut parts = v.split(|c: char| !c.is_ascii_digit());
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

#[derive(Debug, Clone, PartialEq)]
pub enum YabridgeBuild {
    Release(u32, u32, u32),
    Commit(String),
}

// The library embeds its version as the NUL-terminated string just before "yabridge-host-32.exe":
// `5.1.1` in a release, a short commit ID in a git build.
pub fn yabridge_build(lib: &[u8]) -> Option<YabridgeBuild> {
    const MARKER: &[u8] = b"\0yabridge-host-32.exe\0";
    let end = lib.windows(MARKER.len()).position(|w| w == MARKER)?;
    let start = lib[..end].iter().rposition(|&b| b == 0).map_or(0, |i| i + 1);
    let v = std::str::from_utf8(&lib[start..end]).ok()?;
    let nums: Vec<u32> = v.split('.').map_while(|p| p.parse().ok()).collect();
    match nums[..] {
        [a, b, c] if v.split('.').count() == 3 => Some(YabridgeBuild::Release(a, b, c)),
        _ if !v.is_empty() && v.bytes().all(|b| b.is_ascii_hexdigit()) => Some(YabridgeBuild::Commit(v.to_string())),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    NoWine,
    NoYabridge { branch: bool },
    Unchecked,
    Broken,
    Unknown,
    Working,
}

pub fn verdict(wine: Option<(u32, u32)>, installed: bool, build: Option<&YabridgeBuild>) -> Verdict {
    let Some(wine) = wine else { return Verdict::NoWine };
    let new_wine = wine >= POPUPS_BREAK_FROM;
    if !installed {
        return Verdict::NoYabridge { branch: new_wine };
    }
    let Some(build) = build else { return Verdict::Unchecked };
    if !new_wine {
        return Verdict::Working;
    }
    match build {
        YabridgeBuild::Release(a, b, c) if (*a, *b, *c) >= FIXED_RELEASE => Verdict::Working,
        YabridgeBuild::Release(..) => Verdict::Broken,
        YabridgeBuild::Commit(c) if KNOWN_GOOD.contains(&c.as_str()) => Verdict::Working,
        YabridgeBuild::Commit(_) => Verdict::Unknown,
    }
}

pub fn yabridge_link(wine: (u32, u32)) -> &'static str {
    if wine >= POPUPS_BREAK_FROM {
        "https://github.com/robbert-vdh/yabridge/tree/new-wine10-embedding"
    } else {
        "https://github.com/robbert-vdh/yabridge/releases"
    }
}

pub fn detect() -> (Option<(u32, u32)>, Verdict) {
    let wine = std::process::Command::new("wine")
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| wine_version(&String::from_utf8_lossy(&o.stdout)));
    let lib = crate::yabridge::lib_dir().and_then(|d| std::fs::read(d.join("libyabridge-vst3.so")).ok());
    let installed = crate::yabridge::ctl().is_some();
    let build = lib.as_deref().and_then(yabridge_build);
    (wine, verdict(wine, installed, build.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: &str = "Limit                     Soft Limit           Hard Limit           Units     \n\
Max nice priority         0                    0                    \n\
Max realtime priority     70                   70                   \n";

    #[test]
    fn a_wake_hold_writes_its_limit_and_keeps_the_file_open() {
        let path = std::env::temp_dir().join(format!("solstice-wake-{}", std::process::id()));
        std::fs::write(&path, b"").unwrap();
        let hold = hold_wake_at(&path, 10).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), 10i32.to_ne_bytes());
        drop(hold);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_missing_wake_device_is_an_error() {
        assert!(hold_wake_at(std::path::Path::new("/nonexistent/cpu_dma_latency"), 10).is_err());
    }

    #[test]
    fn reads_rtprio() {
        assert!(matches!(parse_rtprio(LIMITS), Some(RtLimit::Limit(70))));
        assert!(matches!(parse_rtprio(&LIMITS.replace("70                   70", "unlimited            unlimited")), Some(RtLimit::Unlimited)));
        assert!(parse_rtprio("Max nice priority 0 0\n").is_none());
    }

    #[test]
    fn realtime_needs_a_limit_above_zero() {
        assert!(!realtime_ok(Some(RtLimit::Limit(0))));
        assert!(realtime_ok(Some(RtLimit::Limit(1))));
        assert!(realtime_ok(Some(RtLimit::Unlimited)));
        assert!(!realtime_ok(None));
    }

    #[test]
    fn picks_the_highest_group() {
        let fedora = "# comment\n@pipewire   - rtprio  70\n@pipewire   - memlock 4194304\n".to_string();
        let other = "*  - rtprio 99\ntom - rtprio 95\n@audio hard rtprio 98\n@realtime soft rtprio 80\n#@ghost - rtprio 99\n".to_string();
        assert_eq!(rt_group(&[fedora.clone()]), Some(("pipewire".into(), 70)));
        assert_eq!(rt_group(&[fedora, other]), Some(("realtime".into(), 80)));
        assert_eq!(rt_group(&["@audio - rtprio 0\n".into()]), None);
        assert_eq!(rt_group(&[]), None);
    }

    #[test]
    fn reads_wine_versions() {
        assert_eq!(wine_version("wine-11.20 (Staging)\n"), Some((11, 20)));
        assert_eq!(wine_version("wine-9.0"), Some((9, 0)));
        assert_eq!(wine_version("bash: wine: command not found"), None);
    }

    #[test]
    fn finds_the_yabridge_build() {
        let lib = |v: &str| [b"other_proxy\0".as_slice(), v.as_bytes(), b"\0yabridge-host-32.exe\0yabridge-host.exe\0"].concat();
        assert!(matches!(yabridge_build(&lib("5.1.1")), Some(YabridgeBuild::Release(5, 1, 1))));
        assert!(matches!(yabridge_build(&lib("ba7022d")), Some(YabridgeBuild::Commit(c)) if c == "ba7022d"));
        assert!(yabridge_build(b"no marker here").is_none());
    }

    #[test]
    fn judges_yabridge_against_wine() {
        use YabridgeBuild::*;
        let new_wine = Some((11, 20));
        let old_wine = Some((9, 21));
        assert!(matches!(verdict(None, false, None), Verdict::NoWine));
        assert!(matches!(verdict(new_wine, false, None), Verdict::NoYabridge { branch: true }));
        assert!(matches!(verdict(old_wine, false, None), Verdict::NoYabridge { branch: false }));
        assert!(matches!(verdict(new_wine, true, None), Verdict::Unchecked));
        assert!(matches!(verdict(new_wine, true, Some(&Release(5, 1, 1))), Verdict::Broken));
        assert!(matches!(verdict(old_wine, true, Some(&Release(5, 1, 1))), Verdict::Working));
        assert!(matches!(verdict(new_wine, true, Some(&Release(5, 2, 0))), Verdict::Working));
        assert!(matches!(verdict(new_wine, true, Some(&Commit("ba7022d".into()))), Verdict::Working));
        assert!(matches!(verdict(new_wine, true, Some(&Commit("1234abc".into()))), Verdict::Unknown));
        assert!(matches!(verdict(old_wine, true, Some(&Commit("1234abc".into()))), Verdict::Working));
    }
}
