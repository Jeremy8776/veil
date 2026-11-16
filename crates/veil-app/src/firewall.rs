//! Whether Windows lets other people's calls in, and a one-click way to allow it.
//!
//! Purpose: a host must accept the first packet of each guest (and of its own rendezvous).
//! Windows Defender Firewall drops unasked inbound UDP for a program with no rule, which a
//! guest sees as "could not reach the host". [`Firewall::check`] looks for an enabled inbound
//! Allow rule for this very `veil-app.exe` that covers Public networks (or every profile).
//! [`Firewall::allow`] adds one: `netsh advfirewall firewall add rule`, UDP only, this program
//! only, Private and Public, started through Windows' own administrator prompt (UAC), so the
//! person sees and confirms the change.
//!
//! Invariants:
//! - Nothing changes without that prompt; declining it changes nothing.
//! - The rule names this executable's own path and UDP only: it opens nothing else.
//! - Scripts go to PowerShell on standard input, never on the command line, so the path
//!   needs quoting only inside a single-quoted PowerShell string.
//! - Both jobs run on a thread of their own; the interface only reads the state.
//! - Not Windows: there is no Windows firewall to ask, so the state is `Allowed`.

use std::sync::{Arc, Mutex};

use eframe::egui;

use crate::media::lock;

/// The rule's display name in Windows Defender Firewall.
pub const RULE_NAME: &str = "Veil";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Not checked yet.
    Unknown,
    Checking,
    Allowed,
    /// No rule lets calls in.
    Blocked,
    /// Waiting for the person to answer Windows' prompt.
    Asking,
    /// The check or the change did not work; the text says why.
    Failed(String),
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

    /// Asks Windows to add the rule, then checks again.
    pub fn allow(&self, ctx: &egui::Context) {
        if !self.start(State::Asking) {
            return;
        }
        let (me, ctx) = (self.clone(), ctx.clone());
        std::thread::spawn(move || {
            let state = match sys::allow() {
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
fn ps_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// Prints how many enabled inbound Allow rules name `exe` and cover Public networks.
fn check_script(exe: &str) -> String {
    format!(
        "$exe = {}\n\
         $n = @(Get-NetFirewallApplicationFilter | Where-Object {{ $_.Program -eq $exe }} | \
         Get-NetFirewallRule | Where-Object {{ $_.Direction -eq 'Inbound' -and \
         $_.Action -eq 'Allow' -and $_.Enabled -eq 'True' -and \
         ($_.Profile -eq 'Any' -or $_.Profile.ToString() -match 'Public') }}).Count\n\
         Write-Output $n\n",
        ps_quote(exe)
    )
}

/// `netsh` arguments that add the rule for `exe`. `None` if the path cannot be quoted.
fn netsh_args(exe: &str) -> Option<String> {
    if exe.contains('"') || exe.contains('\n') || exe.contains('\r') {
        return None;
    }
    Some(format!(
        "advfirewall firewall add rule name=\"{RULE_NAME}\" dir=in action=allow \
         program=\"{exe}\" protocol=udp profile=private,public enable=yes"
    ))
}

/// Runs `netsh` (by full path, never a search: it runs as administrator) elevated through
/// Windows' prompt and exits with its exit code.
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
fn system_program(system_root: Option<&str>, relative: &str) -> String {
    let root = system_root
        .filter(|r| !r.is_empty())
        .unwrap_or(r"C:\Windows")
        .trim_end_matches('\\');
    format!(r"{root}\System32\{relative}")
}

/// Reads the rule count `check_script` prints.
fn parse_count(output: &str) -> Option<usize> {
    output.lines().rev().find_map(|line| line.trim().parse().ok())
}

#[cfg(windows)]
mod sys {
    use std::io::Write;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    use super::{State, allow_script, check_script, netsh_args, parse_count, system_program};

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
            Ok(out) => match parse_count(&String::from_utf8_lossy(&out.stdout)) {
                Some(0) => State::Blocked,
                Some(_) => State::Allowed,
                None => State::Failed(String::from("could not read the firewall rules")),
            },
            Err(why) => State::Failed(why),
        }
    }

    pub fn allow() -> Result<(), String> {
        let exe = exe()?;
        let args = netsh_args(&exe).ok_or("Veil's path cannot be used in a rule")?;
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
    use super::State;

    pub fn check() -> State {
        State::Allowed
    }

    pub fn allow() -> Result<(), String> {
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
        let args = netsh_args(EXE).unwrap_or_default();
        assert!(args.contains(&format!("program=\"{EXE}\"")));
        for part in ["dir=in", "action=allow", "protocol=udp", "profile=private,public"] {
            assert!(args.contains(part), "{part}");
        }
        assert_eq!(netsh_args("C:\\a\"b.exe"), None);
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
        assert!(matches!(state, State::Allowed | State::Blocked), "{state:?}");
    }

    #[test]
    fn the_count_is_the_last_number_printed() {
        assert_eq!(parse_count("2\r\n"), Some(2));
        assert_eq!(parse_count("warning\n0\n"), Some(0));
        assert_eq!(parse_count("nothing"), None);
    }
}
