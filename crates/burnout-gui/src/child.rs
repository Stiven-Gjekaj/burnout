//! Start `burnout write --json` with the privilege to write a drive.
//!
//! Each host asks for the privilege in its own way:
//! - macOS: `burnout` starts with no privilege and no terminal, and asks
//!   macOS for the drive. macOS shows its password dialog.
//! - Linux: `pkexec` starts `burnout` as root after the dialog of polkit.
//! - Windows: UAC starts `burnout`, which gets no streams from this program,
//!   so the events and the answer go through two named pipes.
//!
//! In each case the word `stop` on the input stops the write, because this
//! program cannot send Ctrl+C to a process that runs as root.

use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::path::Path;

/// The host, for the choice of how to start `burnout`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Host {
    Macos,
    Linux,
    Windows,
}

impl Host {
    /// The host that this program runs on.
    pub fn this() -> Self {
        if cfg!(target_os = "macos") {
            Host::Macos
        } else if cfg!(windows) {
            Host::Windows
        } else {
            Host::Linux
        }
    }
}

/// The program to start, and its arguments.
#[derive(Debug, PartialEq, Eq)]
pub struct Launch {
    pub program: OsString,
    pub args: Vec<OsString>,
}

/// How to start a write of `image` to the drive `node` on `host`. On Windows,
/// `pipe` names the two pipes.
pub fn launch(host: Host, burnout: &Path, image: &Path, node: &str, pipe: &str) -> Launch {
    let mut args: Vec<OsString> = vec![
        "write".into(),
        image.into(),
        "--device".into(),
        node.into(),
        "--json".into(),
    ];
    match host {
        Host::Windows => args.extend(["--pipe".into(), pipe.into()]),
        Host::Macos | Host::Linux => args.push("--stop-on-input".into()),
    }
    match host {
        // pkexec takes the full path of the program, and gives it root.
        Host::Linux => {
            args.insert(0, burnout.into());
            Launch {
                program: "pkexec".into(),
                args,
            }
        }
        Host::Macos | Host::Windows => Launch {
            program: burnout.into(),
            args,
        },
    }
}

/// The command line of Windows for these arguments, with the quotes that the
/// C runtime of the child reads back as the same arguments.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn windows_command_line(args: &[OsString]) -> String {
    let mut line = String::new();
    for (at, arg) in args.iter().enumerate() {
        if at > 0 {
            line.push(' ');
        }
        let arg = arg.to_string_lossy();
        if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
            line.push_str(&arg);
            continue;
        }
        line.push('"');
        let mut slashes = 0;
        for c in arg.chars() {
            match c {
                '\\' => slashes += 1,
                '"' => {
                    // Each backslash before a quote doubles, and the quote
                    // gets one of its own.
                    line.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
                    line.push('"');
                    slashes = 0;
                }
                _ => {
                    line.extend(std::iter::repeat_n('\\', slashes));
                    line.push(c);
                    slashes = 0;
                }
            }
        }
        // Backslashes before the closing quote double too.
        line.extend(std::iter::repeat_n('\\', slashes * 2));
        line.push('"');
    }
    line
}

/// How the child ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    /// The child ran, and ended with this code.
    Code(i32),
    /// The person closed the dialog that asks for the privilege, so `burnout`
    /// never ran.
    Cancelled,
}

/// pkexec ends with 126 when the person closes its dialog.
#[cfg(unix)]
const PKEXEC_DISMISSED: i32 = 126;

/// A write that runs.
pub struct Child {
    /// The lines of the events.
    pub events: Box<dyn BufRead + Send>,
    /// Where the answer and the word `stop` go.
    pub answer: Box<dyn Write + Send>,
    process: Process,
}

impl Child {
    /// Wait for the child to end.
    pub fn wait(self) -> Ending {
        drop(self.events);
        self.process.wait()
    }
}

/// Start the write. This waits while the host asks for the privilege, so it
/// runs away from the thread of the window.
pub fn start(burnout: &Path, image: &Path, node: &str) -> Result<Child, String> {
    let pipe = format!("burnout-gui-{}", std::process::id());
    let launch = launch(Host::this(), burnout, image, node, &pipe);
    start_launch(launch, &pipe)
}

#[cfg(unix)]
enum Process {
    Unix {
        child: std::process::Child,
        through_pkexec: bool,
    },
}

#[cfg(unix)]
impl Process {
    fn wait(self) -> Ending {
        let Process::Unix {
            mut child,
            through_pkexec,
        } = self;
        match child.wait().ok().and_then(|s| s.code()) {
            Some(PKEXEC_DISMISSED) if through_pkexec => Ending::Cancelled,
            Some(code) => Ending::Code(code),
            // A signal ended it.
            None => Ending::Code(128),
        }
    }
}

#[cfg(unix)]
fn start_launch(launch: Launch, _pipe: &str) -> Result<Child, String> {
    use std::process::{Command, Stdio};
    let through_pkexec = launch.program == "pkexec";
    let mut child = Command::new(&launch.program)
        .args(&launch.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{} did not start: {e}", launch.program.to_string_lossy()))?;
    let events = std::io::BufReader::new(child.stdout.take().expect("stdout is piped"));
    let answer = child.stdin.take().expect("stdin is piped");
    Ok(Child {
        events: Box::new(events),
        answer: Box::new(answer),
        process: Process::Unix {
            child,
            through_pkexec,
        },
    })
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_CANCELLED, ERROR_PIPE_CONNECTED, HANDLE,
        INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND};
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, WaitForSingleObject, INFINITE,
    };
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    /// A handle that can go to another thread. Windows lets any thread use
    /// a handle of the process.
    #[derive(Clone, Copy)]
    pub(super) struct Handle(HANDLE);
    unsafe impl Send for Handle {}

    pub(super) enum Process {
        Windows(Handle),
        /// UAC was cancelled, so nothing runs.
        None,
    }

    impl Process {
        pub(super) fn wait(self) -> Ending {
            let Process::Windows(Handle(h)) = self else {
                return Ending::Cancelled;
            };
            let mut code = 0u32;
            // SAFETY: the handle is of a process that this program started,
            // and it closes once, here.
            unsafe {
                WaitForSingleObject(h, INFINITE);
                GetExitCodeProcess(h, &mut code);
                CloseHandle(h);
            }
            Ending::Code(code as i32)
        }
    }

    fn pipe(path: &str, access: u32) -> Result<HANDLE, String> {
        // SAFETY: the name is a string that ends in zero, and the other
        // arguments are plain numbers.
        let h = unsafe {
            CreateNamedPipeW(
                wide(path).as_ptr(),
                access,
                PIPE_TYPE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                65536,
                65536,
                0,
                std::ptr::null(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(format!(
                "Windows did not make the pipe {path}: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(h)
    }

    fn connect(h: HANDLE) -> Result<(), String> {
        // SAFETY: the handle is a pipe that this program made.
        let ok = unsafe { ConnectNamedPipe(h, std::ptr::null_mut()) };
        let err = unsafe { GetLastError() };
        if ok == 0 && err != ERROR_PIPE_CONNECTED {
            return Err(format!("burnout did not open its pipe (error {err})"));
        }
        Ok(())
    }

    pub(super) fn start_launch(launch: Launch, name: &str) -> Result<Child, String> {
        let (out_path, in_path) = (
            format!(r"\\.\pipe\{name}.out"),
            format!(r"\\.\pipe\{name}.in"),
        );
        let out = pipe(&out_path, PIPE_ACCESS_INBOUND)?;
        let input = pipe(&in_path, PIPE_ACCESS_OUTBOUND)?;
        let (verb, file, params) = (
            wide("runas"),
            wide(&launch.program.to_string_lossy()),
            wide(&windows_command_line(&launch.args)),
        );
        // SAFETY: the structure is plain data, and zero is a valid start.
        let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = params.as_ptr();
        info.nShow = SW_HIDE;
        // SAFETY: each string lives until the call returns.
        if unsafe { ShellExecuteExW(&mut info) } == 0 {
            let err = unsafe { GetLastError() };
            // SAFETY: the pipes are this program's, and nothing uses them.
            unsafe {
                CloseHandle(out);
                CloseHandle(input);
            }
            if err == ERROR_CANCELLED {
                return Ok(Child {
                    events: Box::new(std::io::empty()),
                    answer: Box::new(std::io::sink()),
                    process: super::Process::Windows(Process::None),
                });
            }
            return Err(format!("Windows did not start burnout (error {err})"));
        }
        let process = Handle(info.hProcess);
        // A burnout that ends before it opens the pipes would leave the waits
        // below waiting for ever. So when the process ends, open each pipe
        // here, which ends the wait. A pipe that burnout holds refuses it.
        let (o, i) = (out_path.clone(), in_path.clone());
        std::thread::spawn(move || {
            let p = process;
            // SAFETY: the handle stays open until `wait` closes it, and
            // `wait` waits for the process first.
            unsafe { WaitForSingleObject(p.0, INFINITE) };
            let _ = std::fs::OpenOptions::new().read(true).open(&o);
            let _ = std::fs::OpenOptions::new().write(true).open(&i);
        });
        connect(out)?;
        connect(input)?;
        // SAFETY: each handle is a connected pipe, and each File owns one.
        let (events, answer) = unsafe {
            (
                std::fs::File::from_raw_handle(out as _),
                std::fs::File::from_raw_handle(input as _),
            )
        };
        Ok(Child {
            events: Box::new(std::io::BufReader::new(events)),
            answer: Box::new(answer),
            process: super::Process::Windows(Process::Windows(process)),
        })
    }
}

#[cfg(windows)]
enum Process {
    Windows(windows::Process),
}

#[cfg(windows)]
impl Process {
    fn wait(self) -> Ending {
        let Process::Windows(p) = self;
        p.wait()
    }
}

#[cfg(windows)]
fn start_launch(launch: Launch, pipe: &str) -> Result<Child, String> {
    windows::start_launch(launch, pipe)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn macos_starts_burnout_itself_and_stops_it_through_the_input() {
        let l = launch(
            Host::Macos,
            Path::new("/opt/burnout"),
            Path::new("/i/a b.iso"),
            "/dev/rdisk4",
            "p",
        );
        assert_eq!(l.program, "/opt/burnout");
        assert_eq!(
            strings(&l.args),
            [
                "write",
                "/i/a b.iso",
                "--device",
                "/dev/rdisk4",
                "--json",
                "--stop-on-input"
            ]
        );
    }

    #[test]
    fn linux_starts_burnout_through_pkexec_by_its_full_path() {
        let l = launch(
            Host::Linux,
            Path::new("/usr/bin/burnout"),
            Path::new("/i/u.iso"),
            "/dev/sdb",
            "p",
        );
        assert_eq!(l.program, "pkexec");
        assert_eq!(
            strings(&l.args),
            [
                "/usr/bin/burnout",
                "write",
                "/i/u.iso",
                "--device",
                "/dev/sdb",
                "--json",
                "--stop-on-input"
            ]
        );
    }

    #[test]
    fn windows_names_the_pipes() {
        let l = launch(
            Host::Windows,
            Path::new(r"C:\b\burnout.exe"),
            Path::new(r"C:\i\w.iso"),
            r"\\.\PhysicalDrive2",
            "gui-7",
        );
        assert_eq!(l.program, r"C:\b\burnout.exe");
        assert_eq!(
            strings(&l.args),
            [
                "write",
                r"C:\i\w.iso",
                "--device",
                r"\\.\PhysicalDrive2",
                "--json",
                "--pipe",
                "gui-7"
            ]
        );
    }

    #[test]
    fn the_command_line_of_windows_quotes_what_needs_it() {
        let args: Vec<OsString> = [
            "write",
            r"C:\My ISOs\Win 11.iso",
            r"C:\dir with space\",
            r#"say "hi""#,
            "",
            r"\\.\PhysicalDrive2",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        assert_eq!(
            windows_command_line(&args),
            r#"write "C:\My ISOs\Win 11.iso" "C:\dir with space\\" "say \"hi\"" "" \\.\PhysicalDrive2"#
        );
    }
}
