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

#[allow(dead_code)]
pub fn realtime_ok(limit: Option<RtLimit>) -> bool {
    match limit {
        Some(RtLimit::Unlimited) => true,
        Some(RtLimit::Limit(n)) => n > 0,
        None => false,
    }
}

#[allow(dead_code)]
pub fn realtime_limit() -> Option<RtLimit> {
    parse_rtprio(&std::fs::read_to_string("/proc/self/limits").ok()?)
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

#[allow(dead_code)]
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

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: &str = "Limit                     Soft Limit           Hard Limit           Units     \n\
Max nice priority         0                    0                    \n\
Max realtime priority     70                   70                   \n";

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
}
