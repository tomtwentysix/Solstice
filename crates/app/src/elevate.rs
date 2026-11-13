use gtk::gio;

pub enum Outcome {
    Done,
    Cancelled,
    Failed(String),
}

fn valid_name(name: &str, allow_dollar: bool) -> bool {
    let name = if allow_dollar { name.strip_suffix('$').unwrap_or(name) } else { name };
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z' | '_'))
        && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_' | '-'))
}

pub fn group_argv(group: &str, user: &str) -> Option<Vec<String>> {
    if !valid_name(group, false) || !valid_name(user, true) {
        return None;
    }
    Some(["pkexec", "/usr/sbin/usermod", "-aG", group, user].map(String::from).to_vec())
}

pub fn group_command(group: &str) -> String {
    format!("sudo usermod -aG {group} $USER")
}

pub fn wine_argv() -> Vec<String> {
    ["pkcon", "install", "--noninteractive", "wine"].map(String::from).to_vec()
}

pub fn wine_command(os_release: &str) -> String {
    let field = |key: &str| {
        os_release
            .lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
            .map(|v| v.trim().trim_matches('"').to_string())
            .unwrap_or_default()
    };
    let ids = field("ID");
    let like = field("ID_LIKE");
    for id in ids.split_whitespace().chain(like.split_whitespace()) {
        match id {
            "fedora" => return "sudo dnf install wine".into(),
            "debian" | "ubuntu" => return "sudo apt install wine".into(),
            "arch" => return "sudo pacman -S wine".into(),
            id if id.starts_with("opensuse") => return "sudo zypper install wine".into(),
            _ => {}
        }
    }
    "Install the wine package with your distribution's package manager.".into()
}

pub fn outcome(status: Option<i32>, output: &str) -> Outcome {
    if status == Some(0) {
        return Outcome::Done;
    }
    // pkexec exits 126 when the prompt is dismissed or authorisation is refused, 127 when it cannot authenticate
    if matches!(status, Some(126 | 127)) || output.to_lowercase().contains("cancel") {
        return Outcome::Cancelled;
    }
    let last = output.lines().map(str::trim).rfind(|l| !l.is_empty());
    Outcome::Failed(match (last, status) {
        (Some(line), _) => line.to_string(),
        (None, Some(code)) => format!("exited with status {code}"),
        (None, None) => "the program did not exit normally".into(),
    })
}

pub fn run(argv: Vec<String>, done: impl FnOnce(Outcome) + 'static) {
    let args: Vec<&std::ffi::OsStr> = argv.iter().map(|a| a.as_ref()).collect();
    let process = match gio::Subprocess::newv(&args, gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_PIPE) {
        Ok(p) => p,
        Err(e) => return done(Outcome::Failed(e.to_string())),
    };
    let finished = process.clone();
    process.communicate_utf8_async(None, gio::Cancellable::NONE, move |result| match result {
        Ok((stdout, stderr)) => {
            // pkcon reports its errors on stdout, pkexec on stderr
            let output = format!("{}\n{}", stdout.as_deref().unwrap_or(""), stderr.as_deref().unwrap_or(""));
            let status = finished.has_exited().then(|| finished.exit_status());
            done(outcome(status, &output));
        }
        Err(e) => done(Outcome::Failed(e.to_string())),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_safe_commands() {
        assert_eq!(group_argv("pipewire", "tom").unwrap(), ["pkexec", "/usr/sbin/usermod", "-aG", "pipewire", "tom"]);
        assert!(group_argv("pipe wire", "tom").is_none());
        assert!(group_argv("audio", "tom; rm -rf /").is_none());
        assert!(group_argv("audio", "-oops").is_none());
    }

    #[test]
    fn reads_outcomes() {
        assert!(matches!(outcome(Some(0), ""), Outcome::Done));
        assert!(matches!(outcome(Some(126), ""), Outcome::Cancelled));
        assert!(matches!(outcome(Some(127), ""), Outcome::Cancelled));
        assert!(matches!(outcome(Some(1), "Fatal error: The transaction was cancelled"), Outcome::Cancelled));
        assert!(matches!(outcome(Some(1), "No package wine found"), Outcome::Failed(e) if e.contains("wine")));
        assert!(matches!(outcome(None, ""), Outcome::Failed(_)));
        assert!(matches!(outcome(Some(1), "Transaction:\tInstalling\nCommand failed: The install was cancelled\n"), Outcome::Cancelled));
        assert!(matches!(outcome(Some(0), "Nothing was cancelled"), Outcome::Done));
        assert!(matches!(outcome(Some(3), "Command failed: A package name to install is required\n\n"), Outcome::Failed(e) if e == "Command failed: A package name to install is required"));
    }

    #[test]
    fn wine_command_per_distribution() {
        assert_eq!(wine_command("ID=fedora\n"), "sudo dnf install wine");
        assert_eq!(wine_command("ID=linuxmint\nID_LIKE=\"ubuntu debian\"\n"), "sudo apt install wine");
        assert!(wine_command("ID=gentoo\n").contains("package manager"));
    }
}
