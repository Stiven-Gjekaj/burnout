//! The write command.
//!
//! The order of the steps is the point of this file, and
//! [the milestones](../../../../docs/milestones.md) fix it:
//!
//! ```text
//! read the image, choose the mode     no privilege
//! open and measure the image          no privilege
//! list the drives, find the target    no privilege
//! ----------------------------------  check the privilege, start again here
//! confirm the target with the person
//! list again, and refuse a drive that changed
//! unmount, write, flush, unmount again, verify, eject
//! ```
//!
//! The privilege comes after the image check, so nobody gives a password and
//! then learns that the path is wrong. It comes before the confirmation, so
//! the person confirms once and not once for each process.

use std::fs::File;
use std::io::{IsTerminal, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use burnout_core::{
    check_fits, check_same_drive, check_target, describe, force_phrase, has_boot_table, human_size,
    in_list_order, phrase_matches, size_column, verify_image, write_image, BlockTarget,
    DriveAccess, DriveInfo, Error, Force, Progress, ProgressEvent, Result, Stage,
    BOOT_SECTOR_BYTES,
};
use burnout_iso::{check_whole, mode_of_file, IsoSource, Mode};
use burnout_layout::{
    unattend_xml, verify_windows, write_windows, Layout, Serials, Unattend, WindowsDrive,
    WindowsTree, UNATTEND_FILE,
};

use crate::cli::{ModeArg, WriteArgs};
use crate::elevate::{self, Plan, State};
use crate::format::drive_json;
use crate::json::Json;
use crate::platform;
use crate::report::{Bar, JsonProgress, Listener};
use crate::stop::Tracked;

/// Print one line of text for a person.
///
/// With `--json` the output stream carries JSON, so the text for a person
/// goes to the error stream, where a person still reads it.
macro_rules! say {
    ($json:expr) => {
        if $json {
            eprintln!()
        } else {
            println!()
        }
    };
    ($json:expr, $($arg:tt)*) => {
        if $json {
            eprintln!($($arg)*)
        } else {
            println!($($arg)*)
        }
    };
}

/// How many times the check opens the drive before it stops, and the pause
/// between two tries. Ten seconds in all.
const OPEN_TRIES: u32 = 40;
const OPEN_PAUSE: Duration = Duration::from_millis(250);

/// What the write puts onto the drive.
enum Job {
    /// The image, byte for byte.
    Raw {
        image: File,
        bytes: u64,
        boot_table: bool,
    },
    /// The layout of Windows mode, with the files of the ISO on it.
    Windows(Box<WindowsJob>),
}

/// A Windows ISO, read and split onto the two partitions.
struct WindowsJob {
    source: IsoSource<File>,
    tree: WindowsTree,
    unattend: String,
    iso_bytes: u64,
}

/// Write an image to a drive.
///
/// With `json`, each step prints as one JSON object on a line of the output
/// stream, and no progress line is drawn.
pub fn run(args: &WriteArgs, elevated: bool, json: bool) -> Result<i32> {
    crate::stop::on_stop(json);
    let job = prepare(args)?;

    let force = if args.force { Force::Yes } else { Force::No };
    let (drives, chosen) = find_target(args)?;
    // A host that cannot name its own system disk cannot refuse it either.
    // A live USB does this: the root is an overlay, the medium is reached
    // through a loop device, and nothing in between names the drive.
    let system_disk_known = drives.iter().any(|d| d.system);
    check_target(&chosen, force)?;
    let layout = match &job {
        Job::Raw { bytes, .. } => {
            check_fits(*bytes, chosen.size_bytes)?;
            None
        }
        Job::Windows(w) => Some(w.tree.plan(chosen.size_bytes, chosen.logical_sector_size)?),
    };

    let state = State {
        privileged: elevate::can_write(&chosen.node),
        already_elevated: elevated,
        no_elevate: args.no_elevate,
        interactive: std::io::stdin().is_terminal(),
        can_restart: cfg!(unix),
    };
    match elevate::plan(state) {
        Plan::GoAhead => {}
        Plan::Stop => return Err(elevate::refusal(state)),
        Plan::StartAgain => {
            eprintln!("Burnout needs root to write a drive, so it asks sudo for it.");
            elevate::start_again(&std::env::args().skip(1).collect::<Vec<_>>())?;
            unreachable!("the restart replaces this program");
        }
    }

    let number = drives
        .iter()
        .position(|d| d.id == chosen.id)
        .map_or(0, |at| at + 1);
    if !confirm(
        &chosen,
        number,
        force,
        args,
        &job,
        layout.as_ref(),
        system_disk_known,
        json,
    )? {
        return Err(Error::NotConfirmed {
            drive: describe(&chosen),
        });
    }

    // The confirmation takes a person's time, and a drive can leave in it.
    // A number is a position in a list, so the drive at that number now may
    // not be the drive that was named a moment ago.
    let (_, again) = find_target(args)?;
    check_same_drive(&chosen, &again)?;
    check_target(&again, force)?;

    let access = platform::drive_access()?;
    let mut bar = Tracked(match json {
        true => Listener::Json(JsonProgress::new()),
        false => Listener::Bar(Bar::new()),
    });

    // Before the unmount, and until the check ends. A host that mounts the
    // new volumes can write to them before the check reads them.
    let _unmounted = access.keep_unmounted(&again.id)?;
    bar.report(ProgressEvent::Start {
        stage: Stage::Unmount,
        total_bytes: None,
    });
    access.unmount_volumes(&again.id)?;
    bar.report(ProgressEvent::Done {
        stage: Stage::Unmount,
        bytes_done: 0,
    });

    match (job, layout) {
        (Job::Raw { mut image, .. }, _) => write_raw(&access, &again, &mut image, &mut bar, json),
        (Job::Windows(w), Some(layout)) => {
            write_windows_mode(&access, &again, &w, layout, &mut bar, json)
        }
        (Job::Windows(_), None) => unreachable!("Windows mode plans a layout"),
    }
}

/// Read the image and choose the mode, before anything needs privilege.
fn prepare(args: &WriteArgs) -> Result<Job> {
    refuse_incomplete(&args.image)?;
    let mode = match args.mode {
        Some(ModeArg::Raw) => Mode::Raw,
        Some(ModeArg::Windows) => Mode::Windows,
        None => decide(mode_of_file(&args.image), &args.image)?,
    };
    if mode == Mode::Windows {
        return Ok(Job::Windows(Box::new(windows_job(args)?)));
    }
    refuse_windows_options(args)?;

    let mut image = File::open(&args.image).map_err(|e| Error::Image {
        path: args.image.display().to_string(),
        detail: e.to_string(),
    })?;
    let bytes = image.seek(SeekFrom::End(0))?;
    image.seek(SeekFrom::Start(0))?;
    if bytes == 0 {
        return Err(Error::Image {
            path: args.image.display().to_string(),
            detail: "the file holds no bytes".to_string(),
        });
    }
    let boot_table = first_sector_has_boot_table(&mut image)?;
    Ok(Job::Raw {
        image,
        bytes,
        boot_table,
    })
}

/// Refuse an image that ends before its own volume does, in either mode and
/// before the mode check reads its tree.
///
/// A path that is not a file goes on, and the mode check names what it is.
fn refuse_incomplete(image: &Path) -> Result<()> {
    if !image.is_file() {
        return Ok(());
    }
    let name = image.display().to_string();
    let mut file = File::open(image).map_err(|e| Error::Image {
        path: name.clone(),
        detail: e.to_string(),
    })?;
    check_whole(&mut file, &name)
}

/// Refuse an option of Windows mode for an image that goes in raw mode.
fn refuse_windows_options(args: &WriteArgs) -> Result<()> {
    let options = [
        (args.skip_hardware_checks, "--skip-hardware-checks"),
        (args.no_microsoft_account, "--no-microsoft-account"),
    ];
    match options.iter().find(|(on, _)| *on) {
        Some((_, option)) => Err(Error::WindowsOption { option }),
        None => Ok(()),
    }
}

/// Read the tree of a Windows ISO, split it, and write its
/// `autounattend.xml`.
fn windows_job(args: &WriteArgs) -> Result<WindowsJob> {
    let name = args.image.display().to_string();
    let unreadable = |detail: String| Error::Image {
        path: name.clone(),
        detail,
    };
    let file = File::open(&args.image).map_err(|e| unreadable(e.to_string()))?;
    let iso_bytes = file.metadata()?.len();
    let source = IsoSource::new(file, &name)?.ok_or_else(|| {
        unreadable(
            "it holds neither UDF nor ISO 9660, so it holds no tree for Windows mode".to_string(),
        )
    })?;
    let tree = WindowsTree::new(&source).map_err(|e| match e {
        Error::Source { path, detail } => unreadable(format!("{path}: {detail}")),
        other => other,
    })?;
    let unattend = unattend_xml(&Unattend {
        architecture: tree.architecture,
        image: tree.image,
        edition: None,
        skip_hardware_checks: args.skip_hardware_checks,
        no_microsoft_account: args.no_microsoft_account,
    });
    Ok(WindowsJob {
        source,
        tree,
        unattend,
        iso_bytes,
    })
}

/// Copy the image byte for byte, and read the drive back against it.
fn write_raw<A: DriveAccess>(
    access: &A,
    drive: &DriveInfo,
    image: &mut File,
    bar: &mut impl Progress,
    json: bool,
) -> Result<i32> {
    let report = {
        let mut target = access.open(&drive.id)?;
        write_image(image, &mut target, bar)?
    };

    // Open the drive again for the check. A read through the handle that did
    // the writing can come out of the cache of the operating system, and then
    // it proves that the cache holds the image and not that the drive does.
    let proof = {
        let mut target = open_again(access, drive, OPEN_TRIES, OPEN_PAUSE)?;
        verify_image(image, &mut target, bar)?
    };
    let ejected = access.eject(&drive.id);

    say!(json);
    say!(
        json,
        "Wrote {} to {}.",
        size_column(report.image_bytes),
        drive.name
    );
    say!(
        json,
        "Checked {} of the drive against the image, byte for byte.",
        size_column(proof.bytes)
    );
    say!(json, "SHA-256 {}", proof.digest);
    offline_note(json);
    eject_note(&ejected, json);
    if json {
        println!(
            "{}",
            Json::Object(vec![
                ("event", Json::text("result")),
                ("mode", Json::text("raw")),
                ("image_bytes", Json::Number(report.image_bytes)),
                ("written_bytes", Json::Number(report.written_bytes)),
                ("checked_bytes", Json::Number(proof.bytes)),
                ("sha256", Json::text(proof.digest.to_string())),
                ("ejected", Json::Bool(let_go(&ejected))),
                ("offline", Json::Bool(cfg!(windows))),
            ])
        );
    }
    Ok(0)
}

/// Lay out the drive for Windows, copy the files of the ISO onto it, and
/// read each file back.
fn write_windows_mode<A: DriveAccess>(
    access: &A,
    drive: &DriveInfo,
    w: &WindowsJob,
    layout: Layout,
    bar: &mut impl Progress,
    json: bool,
) -> Result<i32> {
    let plan = WindowsDrive {
        layout,
        label: w.source.label(),
        serials: serials(),
        unattend: w.unattend.as_bytes(),
    };
    let written = {
        let mut target = access.open(&drive.id)?;
        same_geometry(&target, &layout, drive)?;
        write_windows(&mut target, &w.tree, &w.source, &plan, bar)?
    };
    // Open the drive again for the check, as raw mode does: a read through
    // the handle that wrote can come out of the cache of the host.
    {
        let mut target = open_again(access, drive, OPEN_TRIES, OPEN_PAUSE)?;
        verify_windows(&mut target, &plan, &written, bar)?;
    }
    let ejected = access.eject(&drive.id);

    let (files, bytes) = written.files();
    say!(json);
    say!(json,
        "Wrote {files} files of {} to {}: {} onto partition 1, FAT32, and {} onto partition 2, exFAT.",
        size_column(bytes),
        drive.name,
        written.boot.files.len(),
        written.install.files.len()
    );
    say!(
        json,
        "Checked each file through a new mount of its volume against the SHA-256 that it went in \
         with, and the partition table against the one that Burnout wrote."
    );
    offline_note(json);
    eject_note(&ejected, json);
    if json {
        println!(
            "{}",
            Json::Object(vec![
                ("event", Json::text("result")),
                ("mode", Json::text("windows")),
                ("files", Json::Number(files as u64)),
                ("bytes", Json::Number(bytes)),
                ("boot_files", Json::Number(written.boot.files.len() as u64)),
                (
                    "install_files",
                    Json::Number(written.install.files.len() as u64)
                ),
                ("ejected", Json::Bool(let_go(&ejected))),
                ("offline", Json::Bool(cfg!(windows))),
            ])
        );
    }
    Ok(0)
}

/// Say that the host let go of the drive, or why it did not.
///
/// The eject comes after the check, so a failed eject leaves a drive that is
/// written and checked. That gives a line in the report, and not an error.
fn eject_note(ejected: &Result<()>, json: bool) {
    match ejected {
        Ok(()) => {
            #[cfg(target_os = "macos")]
            say!(
                json,
                "macOS ejected the drive, so nothing mounts it or writes to it until you connect \
                 it again."
            );
        }
        // A busy drive gets no fix of its own here. The fix of a busy drive
        // is to try again, and the eject does not get another try.
        Err(Error::InUse { .. }) => say!(
            json,
            "The eject failed, because a program uses the drive. The host can mount the drive \
             and write to it, so eject it before you remove it."
        ),
        Err(e) => say!(
            json,
            "The eject failed: {e}. The host can mount the drive and write to it, so eject it \
             before you remove it."
        ),
    }
}

/// Open the drive again for the check.
///
/// When the handle that wrote closes, the host reads the new table and can
/// mount the new volumes. macOS does, and so does a Linux desktop. A mounted
/// volume holds the drive, and the open then fails as busy. So each try takes
/// the volumes off first, and a busy drive gets another try while the host
/// still reads it. A volume that a program of the host already reads stops
/// the unmount as busy, and it gets another try too.
fn open_again<A: DriveAccess>(
    access: &A,
    drive: &DriveInfo,
    tries: u32,
    pause: Duration,
) -> Result<A::Target> {
    let mut left = tries;
    loop {
        let opened = access
            .unmount_volumes(&drive.id)
            .and_then(|()| access.open(&drive.id));
        match opened {
            Err(e) if busy(&e) && left > 1 => {
                left -= 1;
                thread::sleep(pause);
            }
            other => return other,
        }
    }
}

/// A drive that a volume or a probe of the host holds answers busy.
fn busy(e: &Error) -> bool {
    matches!(e, Error::InUse { .. })
}

/// The Windows layer took the drive offline for the write, and a person who
/// looks for it in Explorer needs to know why it is not there.
fn offline_note(json: bool) {
    #[cfg(windows)]
    say!(
        json,
        "Windows now holds the drive offline and read-only, so it changes nothing on it."
    );
    let _ = json;
}

/// Refuse a drive whose size or sector size is not what the list said, and
/// what the confirmed layout is for.
fn same_geometry<T: BlockTarget>(target: &T, layout: &Layout, drive: &DriveInfo) -> Result<()> {
    if target.length() == layout.drive_bytes && target.logical_sector_size() == layout.sector_size {
        return Ok(());
    }
    Err(Error::DriveChanged {
        wanted: describe(drive),
        found: format!(
            "a drive of {} bytes with sectors of {} bytes",
            target.length(),
            target.logical_sector_size()
        ),
    })
}

/// Numbers of this drive alone, from the clock and the process. Windows
/// tells disks apart by the signature of the table, and volumes by their
/// serials.
fn serials() -> Serials {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let mut x = nanos ^ ((std::process::id() as u64) << 32);
    let mut next = || {
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) as u32).max(1)
    };
    Serials {
        disk: next(),
        boot: next(),
        install: next(),
    }
}

/// The mode that the image asks for, or the reason to stop.
///
/// An image that fits neither mode stops, and so does a file whose contents
/// Burnout cannot read, and each error says how to copy it anyway. A path
/// that is not a file gives the fault alone, because no mode copies it.
fn decide(mode: Result<Mode>, image: &Path) -> Result<Mode> {
    let path = image.display().to_string();
    match mode {
        Ok(Mode::Neither) => Err(Error::NoMode { path }),
        Ok(mode) => Ok(mode),
        Err(Error::Image { path, detail }) if image.is_file() => Err(Error::Image {
            path,
            detail: format!("{detail}. To copy it byte for byte anyway, use --mode raw"),
        }),
        Err(e) => Err(e),
    }
}

/// The name of the mode of a job, as JSON gives it.
fn mode_name(job: &Job) -> &'static str {
    match job {
        Job::Raw { .. } => "raw",
        Job::Windows(_) => "windows",
    }
}

/// Whether the host let go of the drive at the end. Only macOS ejects it.
fn let_go(ejected: &Result<()>) -> bool {
    cfg!(target_os = "macos") && ejected.is_ok()
}

/// What the write puts onto the drive, as the confirmation says it.
fn what_goes_on(job: &Job, layout: Option<&Layout>) -> Vec<String> {
    let w = match (job, layout) {
        (Job::Windows(w), Some(layout)) => (w, layout),
        (
            Job::Raw {
                boot_table: true, ..
            },
            _,
        ) => return vec!["The image carries a boot table in its first sector.".to_string()],
        _ => {
            return vec![
            "The image carries no boot table in its first sector, so the drive may start nothing."
                .to_string(),
        ]
        }
    };
    let (w, layout) = w;
    windows_lines(&w.tree, &w.unattend, layout)
}

/// The lines of the confirmation for Windows mode.
fn windows_lines(tree: &WindowsTree, unattend: &str, layout: &Layout) -> Vec<String> {
    let (boot_files, _) = tree.boot_files();
    let mut does = vec!["tells Setup where the install image is".to_string()];
    if unattend.contains("LabConfig") {
        does.push("turns off the hardware checks of Windows 11".to_string());
    }
    if unattend.contains("HideOnlineAccountScreens") {
        does.push("takes away the step of the Microsoft account".to_string());
    }
    let does = match does.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{}, and {last}", rest.join(", ")),
        None => unreachable!("the path is always there"),
    };
    let mut lines = vec![
        "The image is a Windows ISO, so Burnout lays the drive out for Windows:".to_string(),
        format!(
            "    partition 1  FAT32, {}, {boot_files} boot files and {UNATTEND_FILE}",
            human_size(layout.boot.length)
        ),
        format!(
            "    partition 2  exFAT, {}, {}",
            human_size(layout.install.length),
            tree.image_path()
        ),
        format!("{UNATTEND_FILE} {does}."),
    ];
    if tree.replaces_unattend {
        lines.push(format!(
            "The ISO holds an {UNATTEND_FILE} of its own, and Burnout puts its own in its place."
        ));
    }
    if layout.beyond_table > 0 {
        lines.push(format!(
            "An MBR reaches no further, so {} at the end of the drive stay unused.",
            human_size(layout.beyond_table)
        ));
    }
    lines
}

/// Read the first sector and say whether it carries a boot table.
fn first_sector_has_boot_table(image: &mut File) -> Result<bool> {
    let mut first = vec![0u8; BOOT_SECTOR_BYTES];
    let mut filled = 0;
    while filled < first.len() {
        match image.read(&mut first[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    image.seek(SeekFrom::Start(0))?;
    Ok(has_boot_table(&first[..filled]))
}

/// The drive that the person named, out of the list as it is now.
///
/// A path is allowed for a script, and it still has to name a drive that the
/// list reports. Without that, every refusal in this file would have nothing
/// to run against.
fn find_target(args: &WriteArgs) -> Result<(Vec<DriveInfo>, DriveInfo)> {
    let drives = in_list_order(platform::drive_list()?.drives()?);

    if let Some(path) = &args.device {
        let found = drives
            .iter()
            .find(|d| names_drive(d, path))
            .cloned()
            .ok_or_else(|| Error::NoSuchDrive {
                wanted: path.clone(),
            })?;
        return Ok((drives, found));
    }

    // clap asks for one of these, so this is the case where a later change
    // took that rule away.
    let Some(index) = args.target else {
        return Err(Error::NoSuchDrive {
            wanted: "that the command names".to_string(),
        });
    };
    let found = burnout_core::by_index(&drives, index)
        .cloned()
        .ok_or_else(|| Error::NoSuchDrive {
            wanted: index.to_string(),
        })?;
    Ok((drives, found))
}

/// Whether a path from `--device` names this drive.
///
/// It names the drive by the node that Burnout writes, by the name of the
/// drive, or by that name under `/dev`. The last one is for macOS, where every
/// tool of the system prints `/dev/disk5` and Burnout writes `/dev/rdisk5`.
/// Measured: `--device /dev/disk5` answered that no drive carries the name.
fn names_drive(drive: &DriveInfo, path: &str) -> bool {
    drive.node == path
        || drive.id.as_str() == path
        || path.strip_prefix("/dev/") == Some(drive.id.as_str())
}

/// Show the target and wait for the person to agree to it.
///
/// Two prompts, and the harder one is not only for `--force`.
///
/// A drive that Burnout cannot prove is removable asks for the model and the
/// size of the drive, which a person can only produce by looking at the drive
/// in front of them. So does every drive on a host where Burnout could not
/// find the system disk, because there the refusal that protects that disk
/// did not run, and the drive in front of the person may be the one their
/// computer is running from. A live USB is exactly that case.
///
/// An ordinary drive on a host that named its system disk asks for the whole
/// word `yes`, because the person already named the drive by number.
#[allow(clippy::too_many_arguments)]
fn confirm(
    drive: &DriveInfo,
    number: usize,
    force: Force,
    args: &WriteArgs,
    job: &Job,
    layout: Option<&Layout>,
    system_disk_known: bool,
    json: bool,
) -> Result<bool> {
    let image_bytes = match job {
        Job::Raw { bytes, .. } => *bytes,
        Job::Windows(w) => w.iso_bytes,
    };
    say!(json, "This erases the drive. Nothing undoes it.");
    say!(json);
    say!(json, "    image   {}", args.image.display());
    say!(json, "            {}", size_column(image_bytes));
    say!(json, "    drive   {}", drive.name);
    say!(json, "            {}", size_column(drive.size_bytes));
    say!(
        json,
        "            {} {} {}",
        drive.node,
        drive.bus.label(),
        if drive.removable() {
            "removable"
        } else {
            "fixed"
        }
    );
    if let Some(serial) = &drive.serial {
        say!(json, "            serial {serial}");
    }
    say!(json);
    for line in what_goes_on(job, layout) {
        say!(json, "{line}");
    }
    say!(json);

    let wanted = if force == Force::Yes {
        say!(
            json,
            "This drive is not one that Burnout can prove is removable."
        );
        say!(json, "Type the model and the size of the drive to go on:");
        say!(json, "    {}", force_phrase(drive));
        None
    } else if !system_disk_known {
        say!(
            json,
            "Burnout cannot tell which drive this system starts from, so it"
        );
        say!(
            json,
            "cannot refuse that drive. Read the drive above, and type its"
        );
        say!(json, "model and its size to go on:");
        say!(json, "    {}", force_phrase(drive));
        None
    } else {
        say!(json, "Type yes to go on:");
        Some("yes")
    };
    if json {
        // The script reads what the drive is and what to type before it
        // answers, as a person reads the lines above.
        let phrase = force_phrase(drive);
        println!(
            "{}",
            Json::Object(vec![
                ("event", Json::text("confirm")),
                (
                    "image",
                    Json::Object(vec![
                        ("path", Json::text(args.image.display().to_string())),
                        ("size_bytes", Json::Number(image_bytes)),
                    ]),
                ),
                ("mode", Json::text(mode_name(job))),
                ("drive", drive_json(number, drive)),
                ("expects", Json::text(wanted.unwrap_or(&phrase))),
            ])
        );
        std::io::stdout().flush()?;
        eprint!("> ");
        std::io::stderr().flush()?;
    } else {
        print!("> ");
        std::io::stdout().flush()?;
    }

    let mut typed = String::new();
    std::io::stdin().read_line(&mut typed)?;

    Ok(match wanted {
        Some(word) => typed.trim().eq_ignore_ascii_case(word),
        None => phrase_matches(&typed, drive),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use burnout_core::DriveId;

    fn args(image: &str) -> WriteArgs {
        WriteArgs {
            image: image.into(),
            target: Some(1),
            device: None,
            force: false,
            no_elevate: true,
            mode: None,
            skip_hardware_checks: false,
            no_microsoft_account: false,
        }
    }

    #[test]
    fn raw_mode_and_windows_mode_go_on() {
        for mode in [Mode::Raw, Mode::Windows] {
            assert_eq!(decide(Ok(mode), Path::new("image.iso")).unwrap(), mode);
        }
    }

    #[test]
    fn an_option_of_windows_mode_is_refused_for_raw_mode() {
        assert!(refuse_windows_options(&args("ubuntu.iso")).is_ok());
        for (option, flags) in [
            ("--skip-hardware-checks", (true, false)),
            ("--no-microsoft-account", (false, true)),
        ] {
            let a = WriteArgs {
                skip_hardware_checks: flags.0,
                no_microsoft_account: flags.1,
                ..args("ubuntu.iso")
            };
            let e = refuse_windows_options(&a).unwrap_err();
            assert!(e.to_string().starts_with(option), "{e}");
        }
    }

    #[test]
    fn the_confirmation_names_both_partitions_and_what_the_file_does() {
        let mut iso = burnout_core::MemorySource::new();
        iso.add_file("efi/boot/bootaa64.efi", vec![1; 10]).unwrap();
        iso.add_file("sources/boot.wim", vec![2; 10]).unwrap();
        iso.add_file("sources/install.wim", vec![3; 10]).unwrap();
        iso.add_file("autounattend.xml", b"theirs".to_vec())
            .unwrap();
        let tree = WindowsTree::new(&iso).unwrap();
        let layout = tree.plan(16_000_000_000 / 512 * 512, 512).unwrap();
        let unattend = unattend_xml(&Unattend {
            architecture: tree.architecture,
            image: tree.image,
            edition: None,
            skip_hardware_checks: true,
            no_microsoft_account: true,
        });
        let lines = windows_lines(&tree, &unattend, &layout);
        assert!(
            lines[1]
                .starts_with("    partition 1  FAT32, 269.5 MB, 2 boot files and autounattend.xml"),
            "{lines:?}"
        );
        assert!(lines[2].ends_with(", sources/install.wim"), "{lines:?}");
        assert_eq!(
            lines[3],
            "autounattend.xml tells Setup where the install image is, turns off the hardware checks \
             of Windows 11, and takes away the step of the Microsoft account."
        );
        assert!(
            lines[4].contains("an autounattend.xml of its own"),
            "{lines:?}"
        );
        let plain = unattend_xml(&Unattend {
            architecture: tree.architecture,
            image: tree.image,
            edition: None,
            skip_hardware_checks: false,
            no_microsoft_account: false,
        });
        let lines = windows_lines(&tree, &plain, &layout);
        assert_eq!(
            lines[3],
            "autounattend.xml tells Setup where the install image is."
        );
    }

    #[test]
    fn each_drive_gets_serials_that_are_not_zero() {
        let s = serials();
        assert!(s.disk != 0 && s.boot != 0 && s.install != 0);
    }

    #[test]
    fn an_image_that_fits_neither_mode_is_refused_and_the_message_names_the_way_past() {
        let e = decide(Ok(Mode::Neither), Path::new("disk.img.xz")).unwrap_err();
        assert!(matches!(e, Error::NoMode { .. }), "{e}");
        let text = e.to_string();
        assert!(text.contains("expanded first"), "{text}");
        assert!(text.ends_with("use --mode raw"), "{text}");
    }

    fn fault(path: &Path) -> Error {
        Error::Image {
            path: path.display().to_string(),
            detail: "the CRC of a file entry is wrong".to_string(),
        }
    }

    #[test]
    fn a_file_that_cannot_be_read_names_the_way_past() {
        let file = std::env::temp_dir().join(format!("burnout-odd-{}.iso", std::process::id()));
        std::fs::write(&file, b"odd").unwrap();
        let e = decide(Err(fault(&file)), &file).unwrap_err();
        std::fs::remove_file(&file).unwrap();
        let text = e.to_string();
        assert!(
            text.contains(".iso: the CRC of a file entry is wrong."),
            "{text}"
        );
        assert!(text.ends_with("use --mode raw"), "{text}");
    }

    #[test]
    fn a_path_that_is_not_a_file_gives_its_fault_alone() {
        for path in [std::env::temp_dir(), "no-such-image.iso".into()] {
            let text = decide(Err(fault(&path)), &path).unwrap_err().to_string();
            assert!(!text.contains("--mode raw"), "{text}");
        }
    }

    #[test]
    fn macos_media_stays_refused_with_its_own_message() {
        let media = Error::MacosMedia {
            path: "Install.dmg".to_string(),
            what: "a compressed disk image of macOS",
        };
        let e = decide(Err(media), Path::new("Install.dmg")).unwrap_err();
        assert!(matches!(e, Error::MacosMedia { .. }), "{e}");
    }

    #[test]
    fn a_macos_drive_answers_to_its_node_its_name_and_the_name_under_dev() {
        let stick = DriveInfo::new(DriveId::new("disk5"), "/dev/rdisk5", "Flash Drive");
        for path in ["/dev/rdisk5", "disk5", "/dev/disk5"] {
            assert!(names_drive(&stick, path), "{path}");
        }
        for path in ["/dev/disk4", "/dev/rdisk", "disk", "/dev/disk5s1", "rdisk5"] {
            assert!(!names_drive(&stick, path), "{path}");
        }
    }

    #[test]
    fn a_linux_drive_answers_to_its_node_and_its_name() {
        let stick = DriveInfo::new(DriveId::new("sdb"), "/dev/sdb", "Flash Drive");
        assert!(names_drive(&stick, "/dev/sdb"));
        assert!(names_drive(&stick, "sdb"));
        assert!(!names_drive(&stick, "/dev/sdb1"));
    }

    /// What one open of a [`HeldDrive`] answers.
    #[derive(Clone, Copy)]
    enum Answer {
        Opens,
        Busy,
        Refuses,
    }

    /// A drive that answers each open with the next answer of a list, and
    /// counts the calls. An unmount answers from a list of its own, and
    /// takes the volumes off when that list is empty.
    struct HeldDrive {
        answers: std::cell::RefCell<Vec<Answer>>,
        unmount_answers: std::cell::RefCell<Vec<Answer>>,
        unmounts: std::cell::Cell<u32>,
        opens: std::cell::Cell<u32>,
    }

    impl HeldDrive {
        fn new(answers: &[Answer]) -> Self {
            HeldDrive {
                answers: std::cell::RefCell::new(answers.iter().rev().copied().collect()),
                unmount_answers: std::cell::RefCell::new(Vec::new()),
                unmounts: std::cell::Cell::new(0),
                opens: std::cell::Cell::new(0),
            }
        }

        fn with_unmounts(self, answers: &[Answer]) -> Self {
            *self.unmount_answers.borrow_mut() = answers.iter().rev().copied().collect();
            self
        }
    }

    fn fail(answer: Answer) -> Error {
        match answer {
            Answer::Busy => Error::InUse {
                what: "/dev/rdisk5".to_string(),
            },
            _ => Error::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
        }
    }

    impl DriveAccess for HeldDrive {
        type Target = burnout_core::MemoryTarget;

        fn unmount_volumes(&self, _id: &DriveId) -> Result<()> {
            self.unmounts.set(self.unmounts.get() + 1);
            match self.unmount_answers.borrow_mut().pop() {
                Some(Answer::Opens) | None => Ok(()),
                Some(answer) => Err(fail(answer)),
            }
        }

        fn open(&self, _id: &DriveId) -> Result<Self::Target> {
            self.opens.set(self.opens.get() + 1);
            match self.answers.borrow_mut().pop() {
                Some(Answer::Opens) | None => burnout_core::MemoryTarget::new(4096, 512),
                Some(answer) => Err(fail(answer)),
            }
        }
    }

    fn stick() -> DriveInfo {
        DriveInfo::new(DriveId::new("disk5"), "/dev/rdisk5", "Flash Drive")
    }

    #[test]
    fn the_check_takes_the_volumes_off_again_before_it_opens_the_drive() {
        let drive = HeldDrive::new(&[Answer::Opens]);
        open_again(&drive, &stick(), 40, Duration::ZERO).unwrap();
        assert_eq!((drive.unmounts.get(), drive.opens.get()), (1, 1));
    }

    #[test]
    fn a_drive_that_the_host_still_holds_gets_another_try() {
        let busy = Answer::Busy;
        let drive = HeldDrive::new(&[busy, busy, busy, Answer::Opens]);
        open_again(&drive, &stick(), 40, Duration::ZERO).unwrap();
        // Each try takes the volumes off first, because the host can mount
        // them between two tries.
        assert_eq!((drive.unmounts.get(), drive.opens.get()), (4, 4));
    }

    #[test]
    fn a_volume_that_a_program_still_reads_gets_another_try() {
        let busy = Answer::Busy;
        let drive = HeldDrive::new(&[Answer::Opens]).with_unmounts(&[busy, busy]);
        open_again(&drive, &stick(), 40, Duration::ZERO).unwrap();
        // The open waits for the unmount, so it runs once.
        assert_eq!((drive.unmounts.get(), drive.opens.get()), (3, 1));
    }

    #[test]
    fn another_fault_of_the_unmount_gets_no_second_try() {
        let drive = HeldDrive::new(&[Answer::Opens]).with_unmounts(&[Answer::Refuses]);
        let e = open_again(&drive, &stick(), 40, Duration::ZERO).unwrap_err();
        assert!(!busy(&e), "{e}");
        assert_eq!((drive.unmounts.get(), drive.opens.get()), (1, 0));
    }

    #[test]
    fn a_drive_that_stays_busy_gives_its_error_after_the_last_try() {
        let drive = HeldDrive::new(&[Answer::Busy; 10]);
        let e = open_again(&drive, &stick(), 5, Duration::ZERO).unwrap_err();
        assert!(busy(&e), "{e}");
        assert_eq!(drive.opens.get(), 5);
    }

    #[test]
    fn another_fault_of_the_open_gets_no_second_try() {
        let drive = HeldDrive::new(&[Answer::Refuses, Answer::Opens]);
        let e = open_again(&drive, &stick(), 40, Duration::ZERO).unwrap_err();
        assert!(!busy(&e), "{e}");
        assert_eq!(drive.opens.get(), 1);
    }
}
