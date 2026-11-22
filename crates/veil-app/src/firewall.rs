//! Whether Windows lets other people's calls in, and a one-click way to allow it.
//!
//! Purpose: a host must accept the first packet of each guest (and of its own rendezvous).
//! Windows Defender Firewall drops unasked inbound UDP for a program with no rule, which a
//! guest sees as "could not reach the host". [`Firewall::check`] looks for an enabled inbound
//! Allow rule for this very `veil-app.exe` that covers the kind of network this computer is on
//! now (Private, Public or Domain). [`Firewall::allow`] adds one for that kind only:
//! `netsh advfirewall firewall add rule`, UDP only, this program only, started through Windows'
//! own administrator prompt (UAC), so the person sees and confirms the change.
//!
//! Invariants:
//! - Nothing changes without that prompt; declining it changes nothing.
//! - The installer never allows Public networks (cafés, hotels, and any network Windows was not
//!   told is home). The app asks for Public only while on one, with a warning: such a rule lets
//!   everyone on every Public network try to reach Veil while it hosts.
//! - The rule names this executable's own path and UDP only: it opens nothing else.
//! - Scripts go to PowerShell on standard input, never on the command line, so the path
//!   needs quoting only inside a single-quoted PowerShell string.
//! - Both jobs run on a thread of their own; the interface only reads the state.
//! - Not Windows: there is no Windows firewall to ask, so the state is `Allowed`.

use std::sync::{Arc, Mutex};

use eframe::egui;

use crate::media::lock;

// The script and rule helpers below serve the Windows `sys` only; other platforms keep them
// compiled so their tests run everywhere.

/// The rule's display name in Windows Defender Firewall.
#[cfg_attr(not(windows), allow(dead_code, reason = "Windows firewall only"))]
pub const RULE_NAME: &str = "Veil";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Not checked yet.
    Unknown,
    Checking,
    Allowed,
    /// No rule lets calls in on this kind of network.
    #[cfg_attr(not(windows), allow(dead_code, reason = "Windows firewall only"))]
    Blocked(Profile),
    /// Waiting for the person to answer Windows' prompt.
    Asking,
    /// The check or the change did not work; the text says why.
    Failed(String),
}

/// A Windows firewall profile: the kind of network a rule applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Private,
    Public,
    Domain,
}

#[cfg_attr(not(windows), allow(dead_code, reason = "Windows firewall only"))]
impl Profile {
    /// The name `netsh` and `Get-NetFirewallRule` use.
    fn name(self) -> &'static str {
        match self {
            Profile::Private => "private",
            Profile::Public => "public",
            Profile::Domain => "domain",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "private" => Some(Profile::Private),
            "public" => Some(Profile::Public),
            "domain" => Some(Profile::Domain),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct Firewall {
    state: Arc<Mutex<State>>,
}

impl Firewall {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State::Unknown)),
        }
    }

    pub fn state(&self) -> State {
        lock(&self.state).clone()
    }

    /// Looks for a rule, unless a check or change is already running.
    pub fn check(&self, ctx: &egui::Context) {
        if !self.start(State::Checking) {
            return;
        }
        let (me, ctx) = (self.clone(), ctx.clone());
        std::thread::spawn(move || {
            let state = sys::check();
            me.finish(state, &ctx);
        });
    }

    /// Asks Windows to add the rule for `profile`, then checks again.
    pub fn allow(&self, profile: Profile, ctx: &egui::Context) {
        if !self.start(State::Asking) {
            return;
        }
        let (me, ctx) = (self.clone(), ctx.clone());
        std::thread::spawn(move || {
            let state = match sys::allow(profile) {
                Ok(()) => sys::check(),
                Err(why) => State::Failed(why),
            };
            me.finish(state, &ctx);
        });
    }

    fn start(&self, next: State) -> bool {
        let mut state = lock(&self.state);
        if matches!(*state, State::Checking | State::Asking) {
            return false;
        }
        *state = next;
        true
    }

    fn finish(&self, state: State, ctx: &egui::Context) {
        *lock(&self.state) = state;
        ctx.request_repaint();
    }
}

/// A PowerShell single-quoted string literal holding `text`.
#[cfg_attr(not(windows), allow(dead_code, reason = "Windows firewall only"))]
fn ps_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// Prints the kind of network guests would arrive on (the most exposed of those connected:
/// public, then domain, then private) and how many enabled inbound Allow rules name `exe` and
/// cover it, as `<profile> <count>`.
#[cfg_attr(not(windows), allow(dead_code, reason = "Windows firewall only"))]
fn check_script(exe: &str) -> String {
    format!(
        "$exe = {}\n\
         $cats = @(Get-NetConnectionProfile | ForEach-Object {{ $_.NetworkCategory.ToString() }})\n\
         $need = if ($cats -contains 'Public') {{ 'public' }} \
         elseif ($cats -contains 'DomainAuthenticated') {{ 'domain' }} else {{ 'private' }}\n\
         $n = @(Get-NetFirewallApplicationFilter | Where-Object {{ $_.Program -eq $exe }} | \
         Get-NetFirewallRule | Where-Object {{ $_.Direction -eq 'Inbound' -and \
         $_.Action -eq 'Allow' -and $_.Enabled -eq 'True' -and \
         ($_.Profile -eq 'Any' -or $_.Profile.ToString() -match $need) }}).Count\n\
         Write-Output \"$need $n\"\n",
        ps_quote(exe)
    )
}

/// `netsh` arguments that add the rule for `exe` on `profile` only. `None` if the path cannot
/// be quoted.
#[cfg_attr(not(windows), allow(dead_code, reason = "Windows firewall only"))]
fn netsh_args(exe: &str, profile: Profile) -> Option<String> {
    if exe.contains('"') || exe.contains('\n') || exe.contains('\r') {
        return None;
    }
    Some(format!(
        "advfirewall firewall add rule name=\"{RULE_NAME}\" dir=in action=allow \
         program=\"{exe}\" protocol=udp profile={} enable=yes",
        profile.name()
    ))
}

/// Runs `netsh` (by full path, never a search: it runs as administrator) elevated through
/// Windows' prompt and exits with its exit code.
#[cfg_attr(not(windows), allow(dead_code, reason = "Windows firewall only"))]
fn allow_script(netsh: &str, args: &str) -> String {
    // One line: PowerShell reading standard input runs each line as it comes.
    format!(
        "try {{ $p = Start-Process -FilePath {} -Verb RunAs -Wait -PassThru \
         -WindowStyle Hidden -ArgumentList {}; exit $p.ExitCode }} catch {{ exit 1223 }}\n\n",
        ps_quote(netsh),
        ps_quote(args)
    )
}

/// A program in Windows' own folder, by full path, so nothing earlier on the search path
/// (or beside Veil) can stand in for it.
#[cfg_attr(not(windows), allow(dead_code, reason = "Windows only"))]
pub(crate) fn system_program(system_root: Option<&str>, relative: &str) -> String {
    let root = system_root
        .filter(|r| !r.is_empty())
        .unwrap_or(r"C:\Windows")
        .trim_end_matches('\\');
    format!(r"{root}\System32\{relative}")
}

/// Reads the `<profile> <count>` line `check_script` prints last.
#[cfg_attr(not(windows), allow(dead_code, reason = "Windows firewall only"))]
fn parse_check(output: &str) -> Option<(Profile, usize)> {
    output.lines().rev().find_map(|line| {
        let (profile, count) = line.trim().split_once(' ')?;
        Some((Profile::parse(profile)?, count.trim().parse().ok()?))
    })
}

#[cfg(windows)]
mod sys {
    use std::io::Write;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    use super::{
        Profile, State, allow_script, check_script, netsh_args, parse_check, system_program,
    };

    /// No console window flashes up for PowerShell.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    /// What `allow_script` exits with when the person declines the prompt.
    const CANCELLED: i32 = 1223;

    fn exe() -> Result<String, String> {
        let path = std::env::current_exe().map_err(|e| format!("cannot find Veil: {e}"))?;
        Ok(path.to_string_lossy().into_owned())
    }

    fn system(relative: &str) -> String {
        system_program(std::env::var("SystemRoot").ok().as_deref(), relative)
    }

    fn powershell(script: &str) -> Result<std::process::Output, String> {
        let mut child = Command::new(system(r"WindowsPowerShell\v1.0\powershell.exe"))
            .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| format!("cannot start PowerShell: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(script.as_bytes())
                .map_err(|e| format!("cannot talk to PowerShell: {e}"))?;
        }
        child
            .wait_with_output()
            .map_err(|e| format!("PowerShell failed: {e}"))
    }

    pub fn check() -> State {
        let result = exe().and_then(|exe| powershell(&check_script(&exe)));
        match result {
            Ok(out) => match parse_check(&String::from_utf8_lossy(&out.stdout)) {
                Some((profile, 0)) => State::Blocked(profile),
                Some(_) => State::Allowed,
                None => State::Failed(String::from("could not read the firewall rules")),
            },
            Err(why) => State::Failed(why),
        }
    }

    pub fn allow(profile: Profile) -> Result<(), String> {
        let exe = exe()?;
        let args = netsh_args(&exe, profile).ok_or("Veil's path cannot be used in a rule")?;
        let out = powershell(&allow_script(&system("netsh.exe"), &args))?;
        match out.status.code() {
            Some(0) => Ok(()),
            Some(CANCELLED) => Err(String::from("Windows did not allow the change")),
            code => Err(format!("netsh failed ({code:?})")),
        }
    }
}

#[cfg(not(windows))]
mod sys {
    use super::{Profile, State};

    pub fn check() -> State {
        State::Allowed
    }

    pub fn allow(_profile: Profile) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = r"C:\Users\Jo O'Neil\Veil\veil-app.exe";

    #[test]
    fn paths_are_quoted_for_powershell() {
        assert_eq!(ps_quote("a'b"), "'a''b'");
        let script = check_script(EXE);
        assert!(script.contains(r"$exe = 'C:\Users\Jo O''Neil\Veil\veil-app.exe'"));
    }

    #[test]
    fn the_rule_is_inbound_udp_for_this_program_only() {
        let args = netsh_args(EXE, Profile::Private).unwrap_or_default();
        assert!(args.contains(&format!("program=\"{EXE}\"")));
        for part in ["dir=in", "action=allow", "protocol=udp", "profile=private "] {
            assert!(args.contains(part), "{part}");
        }
        assert!(!args.contains("public"), "a Private rule never covers Public networks");
        let public = netsh_args(EXE, Profile::Public).unwrap_or_default();
        assert!(public.contains("profile=public ") && !public.contains("private"));
        assert_eq!(netsh_args("C:\\a\"b.exe", Profile::Private), None);
        let script = allow_script(r"C:\Windows\System32\netsh.exe", &args);
        assert!(script.contains("-Verb RunAs"));
        assert!(script.contains(r"-FilePath 'C:\Windows\System32\netsh.exe'"));
        assert!(script.contains("Jo O''Neil"), "the argument string is quoted once more");
    }

    #[test]
    fn system_programs_are_named_by_full_path() {
        let netsh = system_program(Some(r"D:\WINDOWS\"), "netsh.exe");
        assert_eq!(netsh, r"D:\WINDOWS\System32\netsh.exe");
        assert_eq!(system_program(None, "netsh.exe"), r"C:\Windows\System32\netsh.exe");
        assert_eq!(system_program(Some(""), "netsh.exe"), r"C:\Windows\System32\netsh.exe");
    }

    /// Reads this machine's rules (read only): `cargo test -j 8 -p veil-app firewall --
    /// --ignored --nocapture`. The test binary has no rule, so this prints `Blocked`.
    #[test]
    #[ignore = "runs PowerShell to read the firewall rules"]
    fn firewall_check_reads_this_machines_rules() {
        let state = sys::check();
        println!("firewall: {state:?}");
        assert!(matches!(state, State::Allowed | State::Blocked(_)), "{state:?}");
    }

    #[test]
    fn the_check_is_the_last_profile_and_count_printed() {
        assert_eq!(parse_check("private 2\r\n"), Some((Profile::Private, 2)));
        assert_eq!(parse_check("warning\npublic 0\n"), Some((Profile::Public, 0)));
        assert_eq!(parse_check("domain 1"), Some((Profile::Domain, 1)));
        assert_eq!(parse_check("0\n"), None);
        assert_eq!(parse_check("everywhere 1"), None);
        assert_eq!(parse_check("nothing"), None);
    }

    #[test]
    fn the_check_asks_about_the_current_network() {
        let script = check_script(EXE);
        assert!(script.contains("Get-NetConnectionProfile"));
        assert!(script.contains("-match $need"));
    }
}
