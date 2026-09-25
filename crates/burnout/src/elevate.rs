//! Starting again with the privilege that a raw device write needs.
//!
//! Burnout never asks for a password itself. On macOS and on Linux it starts
//! itself again through `sudo`, and `sudo` asks. The password goes to a
//! program that already holds that trust, and it never reaches this process.
//!
//! On Windows there is no restart. `ShellExecuteEx` with the `runas` verb
//! would work, and the result is worse than a clean failure: a second process
//! opens in a new console window, this terminal returns at once, the progress
//! goes to a window that nobody watches, that window closes at the end, and
//! the exit code never reaches the shell. So Windows gets a sentence that
//! says what to do.
//!
//! The decision below is pure and the restart under it is not. That is what
//! lets the rule be tested on all three hosts.

use burnout_core::{Error, Result};

/// The argument that says this process is the second one.
///
/// An argument and not an environment variable, because `sudo` clears the
/// environment. [The milestones](../../../docs/milestones.md) hold the
/// measurement.
pub const ELEVATED_FLAG: &str = "--elevated";

/// What the state of the process says to do about privilege.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    /// The process already has what it needs.
    GoAhead,
    /// Start again through `sudo`.
    StartAgain,
    /// Stop, and say why.
    Stop,
}

/// What this process knows about its own privilege.
#[derive(Clone, Copy, Debug)]
pub struct State {
    /// Whether the process can already write a raw device.
    pub privileged: bool,
    /// Whether this process is the one that a restart started.
    pub already_elevated: bool,
    /// Whether the person passed `--no-elevate`.
    pub no_elevate: bool,
    /// Whether a person is there to answer a password prompt.
    pub interactive: bool,
    /// Whether this host can start itself again at all.
    pub can_restart: bool,
}

/// Decide what to do about privilege, and decide it in one place.
///
/// Five inputs and three answers. A rule spread over the command would be a
/// rule that no test can reach.
pub fn plan(state: State) -> Plan {
    if state.privileged {
        return Plan::GoAhead;
    }
    if !state.can_restart {
        // Windows. There is nothing useful to start.
        return Plan::Stop;
    }
    if state.already_elevated {
        // The restart happened and the privilege did not arrive. Asking
        // again would ask for a password for ever.
        return Plan::Stop;
    }
    if state.no_elevate {
        return Plan::Stop;
    }
    if !state.interactive {
        // A password prompt inside a pipeline stops and waits for nobody.
        return Plan::Stop;
    }
    Plan::StartAgain
}

/// Why Burnout stopped, in words that name the fix.
pub fn refusal(state: State) -> Error {
    let remedy = if !state.can_restart {
        "Close this window. Open Command Prompt or PowerShell with \"Run as \
         administrator\", and run the same command there"
            .to_string()
    } else if state.already_elevated {
        "The restart through sudo did not give root. Run the same command \
         under sudo yourself"
            .to_string()
    } else if state.no_elevate {
        "Remove --no-elevate, or run the same command under sudo".to_string()
    } else {
        "There is no terminal to ask a password on. Run the same command \
         under sudo"
            .to_string()
    };
    Error::NeedsPrivilege { remedy }
}

/// Build the command line that starts this process again with privilege.
///
/// Three details, and each one is a way to get this wrong:
///
/// - The program comes from `current_exe()` and never from `argv[0]`. The
///   caller chooses `argv[0]`, so trusting it hands the choice of what runs
///   as root to whoever started this.
/// - `--` goes before the path, or `sudo` reads the Burnout options as its
///   own.
/// - The arguments go across as a list. A command string would need quoting,
///   and quoting is where an injection gets in.
pub fn restart_arguments(program: &str, exe: &str, arguments: &[String]) -> Vec<String> {
    let mut out = vec![program.to_string(), "--".to_string(), exe.to_string()];
    out.extend(arguments.iter().cloned());
    out.push(ELEVATED_FLAG.to_string());
    out
}

/// Whether this process can already write one drive.
///
/// This asks the drive and not the user id. Root is the usual way to hold
/// that permission and it is not the only one: a member of the `disk` group
/// on Linux holds it, and on macOS so does the person who attached a disk
/// image. Asking the drive means Burnout asks for a password when it needs
/// one and not every time.
///
/// The probe opens for a write and writes nothing. Only a refusal about
/// permission counts as no: a drive that answers `busy` is one this process
/// may write once the volumes are unmounted, and a password would not help.
#[cfg(unix)]
pub fn can_write(node: &str) -> bool {
    // SAFETY: geteuid reads one number out of the process and touches
    // nothing.
    if unsafe { libc::geteuid() == 0 } {
        return true;
    }
    match std::fs::OpenOptions::new().write(true).open(node) {
        Ok(_) => true,
        Err(e) => e.kind() != std::io::ErrorKind::PermissionDenied,
    }
}

/// Whether this process can already write one drive.
///
/// Windows answers by opening the drive, in the same way. A token can say
/// elevated while the device still refuses, so the open is the only honest
/// answer.
#[cfg(not(unix))]
pub fn can_write(node: &str) -> bool {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = std::ffi::OsStr::new(node)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    crate::platform::windows::host::can_open_for_write(&wide)
}

/// Start this process again through `sudo`, and never come back.
///
/// `exec` and not `spawn`. A spawned child would give this process two jobs,
/// to copy the output of the child and to pass its exit code on, and it would
/// put a second process between the person and their terminal. `exec`
/// replaces this program with `sudo`, so there is one process, one terminal
/// and one exit code.
#[cfg(unix)]
pub fn start_again(arguments: &[String]) -> Result<std::convert::Infallible> {
    use std::os::unix::process::CommandExt;

    let mut last = None;
    // `doas` where there is no `sudo`. Not `pkexec`, which targets a desktop
    // and changes the environment under the program it starts.
    for program in ["sudo", "doas"] {
        let line = restart_arguments(program, &current_exe()?, arguments);
        let error = std::process::Command::new(&line[0]).args(&line[1..]).exec();
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(Error::Io(error));
        }
        last = Some(error);
    }
    Err(Error::NeedsPrivilege {
        remedy: format!(
            "Neither sudo nor doas is on this system ({}). Run the same command as root",
            last.map(|e| e.to_string()).unwrap_or_default()
        ),
    })
}

/// Start this process again. There is no way to do that on this host.
#[cfg(not(unix))]
pub fn start_again(_arguments: &[String]) -> Result<std::convert::Infallible> {
    Err(refusal(State {
        privileged: false,
        already_elevated: false,
        no_elevate: false,
        interactive: true,
        can_restart: false,
    }))
}

/// The path of this program, as the operating system reports it.
#[cfg(unix)]
fn current_exe() -> Result<String> {
    let path = std::env::current_exe()?;
    path.to_str()
        .map(|p| p.to_string())
        .ok_or_else(|| Error::Host {
            source: "current_exe".to_string(),
            detail: "the path of this program is not text".to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> State {
        State {
            privileged: false,
            already_elevated: false,
            no_elevate: false,
            interactive: true,
            can_restart: true,
        }
    }

    #[test]
    fn a_process_that_already_has_privilege_goes_ahead() {
        let mut s = state();
        s.privileged = true;
        assert_eq!(plan(s), Plan::GoAhead);
    }

    #[test]
    fn a_process_without_privilege_starts_again() {
        assert_eq!(plan(state()), Plan::StartAgain);
    }

    #[test]
    fn a_second_attempt_stops_rather_than_ask_for_ever() {
        // The restart happened and root did not arrive. Asking again is a
        // password prompt with no end.
        let mut s = state();
        s.already_elevated = true;
        assert_eq!(plan(s), Plan::Stop);
    }

    #[test]
    fn a_process_that_already_has_privilege_goes_ahead_even_on_the_second_run() {
        // The ordinary path: the restart worked. The guard must not stop the
        // process it started.
        let mut s = state();
        s.privileged = true;
        s.already_elevated = true;
        assert_eq!(plan(s), Plan::GoAhead);
    }

    #[test]
    fn no_elevate_stops_instead_of_asking() {
        let mut s = state();
        s.no_elevate = true;
        assert_eq!(plan(s), Plan::Stop);
    }

    #[test]
    fn a_pipeline_stops_rather_than_wait_for_a_password() {
        // A sudo prompt with no terminal behind it waits for nobody.
        let mut s = state();
        s.interactive = false;
        assert_eq!(plan(s), Plan::Stop);
    }

    #[test]
    fn a_host_that_cannot_restart_stops_and_says_how_to_run_it_again() {
        let mut s = state();
        s.can_restart = false;
        assert_eq!(plan(s), Plan::Stop);
        let message = refusal(s).to_string();
        assert!(message.contains("administrator"));
    }

    #[test]
    fn each_reason_to_stop_names_its_own_fix() {
        let mut second = state();
        second.already_elevated = true;
        assert!(refusal(second).to_string().contains("sudo"));

        let mut asked_not_to = state();
        asked_not_to.no_elevate = true;
        assert!(refusal(asked_not_to).to_string().contains("--no-elevate"));

        let mut piped = state();
        piped.interactive = false;
        assert!(refusal(piped).to_string().contains("terminal"));
    }

    #[test]
    fn the_restart_puts_two_dashes_before_the_program() {
        // Without them sudo reads the Burnout options as its own.
        let line = restart_arguments("sudo", "/usr/local/bin/burnout", &["list".to_string()]);
        assert_eq!(line[0], "sudo");
        assert_eq!(line[1], "--");
        assert_eq!(line[2], "/usr/local/bin/burnout");
    }

    #[test]
    fn the_restart_carries_the_arguments_and_adds_the_guard() {
        let given: Vec<String> = ["write", "ubuntu.iso", "2", "--force"]
            .iter()
            .map(|a| a.to_string())
            .collect();
        let line = restart_arguments("sudo", "/bin/burnout", &given);
        assert_eq!(&line[3..line.len() - 1], given.as_slice());
        assert_eq!(line.last().unwrap(), ELEVATED_FLAG);
    }

    #[test]
    fn an_argument_with_a_space_stays_one_argument() {
        // The list goes across as a list, so there is no quoting and nothing
        // to get through it.
        let given = vec!["write".to_string(), "/tmp/my image.iso".to_string()];
        let line = restart_arguments("sudo", "/bin/burnout", &given);
        assert!(line.contains(&"/tmp/my image.iso".to_string()));
        assert_eq!(line.len(), 3 + given.len() + 1);
    }
}
