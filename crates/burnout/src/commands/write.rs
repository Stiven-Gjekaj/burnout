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
//! read the partition table            with --partition only
//! confirm the target with the person
//! list again, and refuse a drive that changed
//! read the table again, and refuse a partition that changed
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
    check_fits, check_same_drive, check_target, choose_partition, describe, force_phrase, grouped,
    has_boot_table, human_size, in_list_order, phrase_matches, read_table, size_column,
    verify_image, write_image, BlockTarget, DriveAccess, DriveInfo, Error, Force, Partition,
    Progress, ProgressEvent, Result, Stage, TreePath, Window, BOOT_SECTOR_BYTES, TABLE_BYTES,
};
use burnout_iso::{check_whole, mode_of_file, IsoSource, Mode};
use burnout_layout::{
    unattend_xml, verify_windows, write_windows, InstallImage, Layout, Serials, Unattend,
    WindowsDrive, WindowsTree, UNATTEND_FILE,
};
use burnout_wim::{read_wim, Compression, Edition, Wim};

use crate::cli::{ModeArg, WriteArgs};
use crate::elevate::{self, Plan, State};
use crate::format::drive_json;
use crate::json::Json;
use crate::platform;
use crate::report::{Bar, JsonProgress, Listener};
use crate::stop::Tracked;
use crate::to_go::{self, ToGo};

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
    /// One edition, applied for Windows To Go.
    ToGo(Box<ToGoJob>),
}

/// A Windows ISO or a WIM file, and the edition of it that goes onto the
/// drive.
struct ToGoJob {
    input: WimInput,
    wim: Wim,
    edition: Edition,
    /// The place of the edition in the install image, counted from 1.
    position: usize,
    image_bytes: u64,
    /// The drive, worked out when the drive is known.
    plan: Option<ToGo>,
}

/// Where the install image of `--to-go` is.
enum WimInput {
    /// `sources/install.wim` inside a Windows ISO.
    Iso {
        source: IsoSource<File>,
        path: TreePath,
    },
    /// A WIM file of its own, such as the one that DISM exports from an
    /// `install.esd`.
    File(std::path::PathBuf),
}

/// Something that reads and seeks, for the two kinds of input.
trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

impl WimInput {
    fn open(&self) -> Result<Box<dyn ReadSeek + '_>> {
        match self {
            WimInput::Iso { source, path } => Ok(Box::new(source.open_file(path)?)),
            WimInput::File(path) => Ok(Box::new(File::open(path).map_err(|e| Error::Image {
                path: path.display().to_string(),
                detail: e.to_string(),
            })?)),
        }
    }

    /// The name of the install image in a message.
    fn name(&self) -> String {
        match self {
            WimInput::Iso { path, .. } => path.to_string(),
            WimInput::File(path) => path.display().to_string(),
        }
    }
}

impl ToGoJob {
    fn name(&self) -> String {
        self.input.name()
    }
}

/// A Windows ISO, read and split onto the two partitions.
struct WindowsJob {
    source: IsoSource<File>,
    tree: WindowsTree,
    unattend: String,
    iso_bytes: u64,
    /// The edition that `--edition` chose, which Setup then installs.
    edition: Option<Edition>,
}

/// Write an image to a drive.
///
/// With `json`, each step prints as one JSON object on a line of the output
/// stream, and no progress line is drawn.
pub fn run(args: &WriteArgs, elevated: bool, json: bool) -> Result<i32> {
    crate::stop::on_stop(json);
    let mut job = prepare(args)?;

    let force = if args.force { Force::Yes } else { Force::No };
    let (drives, chosen) = find_target(args)?;
    // A host that cannot name its own system disk cannot refuse it either.
    // A live USB does this: the root is an overlay, the medium is reached
    // through a loop device, and nothing in between names the drive.
    let system_disk_known = drives.iter().any(|d| d.system);
    check_target(&chosen, force)?;
    let layout = match &mut job {
        Job::Raw { bytes, .. } => {
            check_fits(*bytes, chosen.size_bytes)?;
            None
        }
        Job::Windows(w) => Some(w.tree.plan(chosen.size_bytes, chosen.logical_sector_size)?),
        Job::ToGo(t) => {
            let name = t.name();
            let mut wim_file = t.input.open()?;
            t.plan = Some(to_go::plan(
                &mut wim_file,
                &name,
                &t.wim,
                t.position,
                chosen.size_bytes,
                &seed(&chosen),
                if args.bios {
                    to_go::Firmware::Bios
                } else {
                    to_go::Firmware::Uefi
                },
            )?);
            None
        }
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

    // The table comes from the drive, so it needs the privilege, and the
    // person sees the partition before they agree to it.
    let partition = match (&job, args.partition) {
        (Job::Raw { bytes, .. }, Some(number)) => Some(partition_of(&chosen, number, *bytes)?),
        _ => None,
    };

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
        partition.as_ref(),
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
    if let (Some(p), Job::Raw { bytes, .. }) = (&partition, &job) {
        if partition_of(&again, p.number, *bytes)? != *p {
            return Err(Error::PartitionChanged { number: p.number });
        }
    }

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
        (Job::Raw { mut image, .. }, _) => write_raw(
            &access,
            &again,
            partition.as_ref(),
            &mut image,
            &mut bar,
            json,
        ),
        (Job::Windows(w), Some(layout)) => {
            write_windows_mode(&access, &again, &w, layout, &mut bar, json)
        }
        (Job::Windows(_), None) => unreachable!("Windows mode plans a layout"),
        (Job::ToGo(t), _) => write_to_go_mode(&access, &again, &t, &mut bar, json),
    }
}

/// The seed of the GUIDs of a drive of Windows To Go: the drive and the
/// time, so that two drives never share a GUID.
fn seed(drive: &DriveInfo) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "{}/{}/{now}",
        drive.id.as_str(),
        drive.serial.as_deref().unwrap_or("")
    )
}

/// Apply the edition for Windows To Go, and read the drive back.
fn write_to_go_mode<A: DriveAccess>(
    access: &A,
    drive: &DriveInfo,
    t: &ToGoJob,
    bar: &mut impl Progress,
    json: bool,
) -> Result<i32> {
    let plan = t.plan.as_ref().expect("run plans the drive");
    let name = t.name();
    let written = {
        let mut target = access.open(&drive.id)?;
        let mut wim_file = t.input.open()?;
        to_go::write(&mut target, plan, &mut wim_file, &t.wim, &name, bar)?
    };
    {
        let mut target = open_again(access, drive, OPEN_TRIES, OPEN_PAUSE)?;
        to_go::verify(&mut target, plan, &written, bar)?;
    }
    let ejected = access.eject(&drive.id);

    say!(json);
    say!(
        json,
        "Wrote {} to {}: {} files and directories and {} of streams onto partition 2, NTFS, and {} boot files and the BCD store onto partition 1, FAT32.",
        plan.edition,
        drive.name,
        grouped(plan.files as u64),
        size_column(plan.stream_bytes),
        plan.boot_files
    );
    say!(
        json,
        "Checked the {}, each boot file through a new mount of its volume, and each stream of \
         Windows against the SHA-1 that the install image names for it.",
        match plan.firmware {
            to_go::Firmware::Uefi => "GPT",
            to_go::Firmware::Bios => "MBR",
        }
    );
    offline_note(json);
    eject_note(&ejected, json);
    if json {
        println!(
            "{}",
            Json::Object(vec![
                ("event", Json::text("result")),
                ("mode", Json::text("to_go")),
                (
                    "firmware",
                    Json::text(match plan.firmware {
                        to_go::Firmware::Uefi => "uefi",
                        to_go::Firmware::Bios => "bios",
                    }),
                ),
                ("edition", edition_object(&t.edition)),
                ("files", Json::Number(plan.files as u64)),
                ("stream_bytes", Json::Number(plan.stream_bytes)),
                ("boot_files", Json::Number(plan.boot_files as u64)),
                ("ejected", Json::Bool(let_go(&ejected))),
                ("offline", Json::Bool(cfg!(windows))),
            ])
        );
    }
    Ok(0)
}

/// Read the image and choose the mode, before anything needs privilege.
fn prepare(args: &WriteArgs) -> Result<Job> {
    refuse_incomplete(&args.image)?;
    if args.to_go {
        return Ok(Job::ToGo(Box::new(to_go_job(args)?)));
    }
    let mode = match (args.partition, args.mode) {
        (Some(_), Some(ModeArg::Windows)) => return Err(windows_in_partition(&args.image)),
        (Some(_), _) => partition_mode(mode_of_file(&args.image), &args.image)?,
        (None, Some(ModeArg::Raw)) => Mode::Raw,
        (None, Some(ModeArg::Windows)) => Mode::Windows,
        (None, None) => decide(mode_of_file(&args.image), &args.image)?,
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
        (args.edition.is_some(), "--edition"),
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
    let edition = match &args.edition {
        Some(text) => Some(choose_edition(&source, &tree, text, &name)?),
        None => None,
    };
    let unattend = unattend_xml(&Unattend {
        architecture: tree.architecture,
        image: tree.image,
        edition: edition.as_ref().map(|e| e.index),
        skip_hardware_checks: args.skip_hardware_checks,
        no_microsoft_account: args.no_microsoft_account,
    });
    Ok(WindowsJob {
        source,
        tree,
        unattend,
        iso_bytes,
        edition,
    })
}

/// Read the install image of a Windows ISO or a WIM file, and find the
/// edition that `--edition` names.
fn to_go_job(args: &WriteArgs) -> Result<ToGoJob> {
    let name = args.image.display().to_string();
    let unreadable = |detail: String| Error::Image {
        path: name.clone(),
        detail,
    };
    let mut file = File::open(&args.image).map_err(|e| unreadable(e.to_string()))?;
    let image_bytes = file.metadata()?.len();
    let mut magic = [0u8; 8];
    let is_wim = file.read_exact(&mut magic).is_ok() && &magic == WIM_MAGIC;
    file.seek(SeekFrom::Start(0))?;
    let mut esd = false;
    let input = if is_wim {
        WimInput::File(args.image.clone())
    } else {
        let source = IsoSource::new(file, &name)?.ok_or_else(|| {
            unreadable(
                "it holds neither UDF nor ISO 9660 nor a WIM header, so it is neither a \
                 Windows ISO nor a WIM file"
                    .to_string(),
            )
        })?;
        let tree = WindowsTree::new(&source).map_err(|e| match e {
            Error::Source { path, detail } => unreadable(format!("{path}: {detail}")),
            other => other,
        })?;
        esd = tree.image == InstallImage::Esd;
        WimInput::Iso {
            path: tree.image_path().clone(),
            source,
        }
    };
    // The header and the XML of an install.esd are not compressed, so the
    // edition is found before the refusal, and the refusal names its index.
    let wim = read_wim(input.open()?, &input.name())?;
    let text = args.edition.as_deref().unwrap_or_default();
    let edition = wim
        .find_edition(text)
        .cloned()
        .ok_or_else(|| Error::NoSuchEdition {
            path: name.clone(),
            edition: text.to_string(),
            editions: editions_text(&wim.editions),
        })?;
    if esd || wim.header.compression() == Compression::Lzms {
        return Err(Error::LzmsImage {
            path: name.clone(),
            inside_iso: matches!(input, WimInput::Iso { .. }),
            index: edition.index,
        });
    }
    let position = wim
        .editions
        .iter()
        .position(|e| e.index == edition.index)
        .expect("the edition is in the list")
        + 1;
    Ok(ToGoJob {
        input,
        wim,
        edition,
        position,
        image_bytes,
        plan: None,
    })
}

/// The first bytes of a WIM file.
const WIM_MAGIC: &[u8; 8] = b"MSWIM\0\0\0";

/// The edition of the install image that `text` names, by its index or its
/// name, as `burnout info` lists them.
fn choose_edition(
    source: &IsoSource<File>,
    tree: &WindowsTree,
    text: &str,
    iso: &str,
) -> Result<Edition> {
    let path = tree.image_path();
    let wim = read_wim(source.open_file(path)?, &format!("{iso}: {path}"))?;
    wim.find_edition(text)
        .cloned()
        .ok_or_else(|| Error::NoSuchEdition {
            path: iso.to_string(),
            edition: text.to_string(),
            editions: editions_text(&wim.editions),
        })
}

/// The editions as a person reads them: "1 Windows 11 Home and 2 Windows 11
/// Pro".
fn editions_text(editions: &[Edition]) -> String {
    let each: Vec<String> = editions
        .iter()
        .map(|e| format!("{} {}", e.index, e.shown_name()))
        .collect();
    match each.split_last() {
        None => "none".to_string(),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

/// Copy the image byte for byte, and read the drive back against it.
fn write_raw<A: DriveAccess>(
    access: &A,
    drive: &DriveInfo,
    partition: Option<&Partition>,
    image: &mut File,
    bar: &mut impl Progress,
    json: bool,
) -> Result<i32> {
    let report = {
        let mut target = access.open(&drive.id)?;
        match partition {
            Some(p) => write_image(image, &mut inside(&mut target, p)?, bar)?,
            None => write_image(image, &mut target, bar)?,
        }
    };

    // Open the drive again for the check. A read through the handle that did
    // the writing can come out of the cache of the operating system, and then
    // it proves that the cache holds the image and not that the drive does.
    let proof = {
        let mut target = open_again(access, drive, OPEN_TRIES, OPEN_PAUSE)?;
        match partition {
            Some(p) => verify_image(image, &mut inside(&mut target, p)?, bar)?,
            None => verify_image(image, &mut target, bar)?,
        }
    };
    let ejected = access.eject(&drive.id);

    let (into, of) = match partition {
        Some(p) => (
            format!("partition {} of {}", p.number, drive.name),
            "partition",
        ),
        None => (drive.name.clone(), "drive"),
    };
    say!(json);
    say!(json, "Wrote {} to {into}.", size_column(report.image_bytes));
    say!(
        json,
        "Checked {} of the {of} against the image, byte for byte.",
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
                ("partition", partition_number_json(partition)),
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

/// The mode of an image that goes into one partition: raw, whatever the
/// image is, except a Windows ISO.
///
/// A file system image fits neither mode on a whole drive, and it is the usual
/// image for one partition, so it goes on. macOS media and a file that cannot
/// be read stay refused.
fn partition_mode(mode: Result<Mode>, image: &Path) -> Result<Mode> {
    match mode? {
        Mode::Windows => Err(windows_in_partition(image)),
        _ => Ok(Mode::Raw),
    }
}

fn windows_in_partition(image: &Path) -> Error {
    Error::WindowsInPartition {
        path: image.display().to_string(),
    }
}

/// Partition `number` of `drive`, read from the table on the drive, and
/// checked for an image of `image_bytes`.
fn partition_of(drive: &DriveInfo, number: u32, image_bytes: u64) -> Result<Partition> {
    let sector = drive.logical_sector_size as u64;
    let length = (TABLE_BYTES as u64).min(drive.size_bytes) / sector * sector;
    let mut head = vec![0u8; length as usize];
    File::open(&drive.node)?.read_exact(&mut head)?;
    let table = read_table(&head, drive.logical_sector_size, drive.size_bytes)?;
    choose_partition(table.as_ref(), number, &drive.name, image_bytes)
}

/// The part of the drive that the partition holds.
fn inside<'a, T: BlockTarget>(target: &'a mut T, p: &Partition) -> Result<Window<&'a mut T>> {
    Window::new(target, p.first_byte, p.bytes)
}

/// The lines of the confirmation that name the partition.
fn partition_lines(p: &Partition) -> Vec<String> {
    vec![
        format!("    partition {}", p.number),
        format!("            {}", size_column(p.bytes)),
        format!("            from byte {}", grouped(p.first_byte)),
        format!("            {}", p.kind.describe()),
    ]
}

fn partition_json(p: Option<&Partition>) -> Json {
    match p {
        Some(p) => Json::Object(vec![
            ("number", Json::Number(p.number as u64)),
            ("first_byte", Json::Number(p.first_byte)),
            ("size_bytes", Json::Number(p.bytes)),
            ("type", Json::text(p.kind.describe())),
        ]),
        None => Json::Null,
    }
}

fn partition_number_json(p: Option<&Partition>) -> Json {
    match p {
        Some(p) => Json::Number(p.number as u64),
        None => Json::Null,
    }
}

/// The name of the mode of a job, as JSON gives it.
/// The edition that Setup installs, or `null` when Setup asks.
fn edition_json(job: &Job) -> Json {
    match job {
        Job::Windows(w) => match &w.edition {
            Some(e) => edition_object(e),
            None => Json::Null,
        },
        Job::ToGo(t) => edition_object(&t.edition),
        Job::Raw { .. } => Json::Null,
    }
}

fn edition_object(e: &Edition) -> Json {
    Json::Object(vec![
        ("index", Json::Number(e.index as u64)),
        ("name", Json::text(e.shown_name())),
    ])
}

fn mode_name(job: &Job) -> &'static str {
    match job {
        Job::Raw { .. } => "raw",
        Job::Windows(_) => "windows",
        Job::ToGo(_) => "to_go",
    }
}

/// Whether the host let go of the drive at the end. Only macOS ejects it.
fn let_go(ejected: &Result<()>) -> bool {
    cfg!(target_os = "macos") && ejected.is_ok()
}

/// What the write puts onto the drive, as the confirmation says it.
fn what_goes_on(job: &Job, layout: Option<&Layout>) -> Vec<String> {
    if let Job::ToGo(t) = job {
        return to_go_lines(t);
    }
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
    windows_lines(&w.tree, &w.unattend, w.edition.as_ref(), layout)
}

/// The lines of the confirmation for Windows To Go.
fn to_go_lines(t: &ToGoJob) -> Vec<String> {
    let plan = t.plan.as_ref().expect("run plans the drive");
    vec![
        format!(
            "Burnout makes a drive of Windows To Go with edition {}, {}:",
            t.edition.index, plan.edition
        ),
        format!(
            "    partition 1  FAT32, {}, {} boot files and {}",
            human_size(to_go::EFI_SECTORS * 512),
            plan.boot_files,
            if plan.firmware == to_go::Firmware::Bios {
                "two BCD stores, active"
            } else {
                "the BCD store"
            }
        ),
        format!(
            "    partition 2  NTFS, {}, {} files and directories, {} of streams",
            human_size(plan.windows_bytes()),
            grouped(plan.files as u64),
            human_size(plan.stream_bytes)
        ),
        match plan.firmware {
            to_go::Firmware::Uefi => "The drive starts Windows through UEFI, with no Setup.",
            to_go::Firmware::Bios => {
                "The drive has an MBR, and starts Windows through a BIOS or through UEFI, with no Setup."
            }
        }
        .to_string(),
    ]
}

/// The lines of the confirmation for Windows mode.
fn windows_lines(
    tree: &WindowsTree,
    unattend: &str,
    edition: Option<&Edition>,
    layout: &Layout,
) -> Vec<String> {
    let (boot_files, _) = tree.boot_files();
    let mut does = vec!["tells Setup where the install image is".to_string()];
    if let Some(e) = edition {
        does.push(format!(
            "chooses edition {}, {}, so Setup does not ask for one",
            e.index,
            e.shown_name()
        ));
    }
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
    partition: Option<&Partition>,
    system_disk_known: bool,
    json: bool,
) -> Result<bool> {
    let image_bytes = match job {
        Job::Raw { bytes, .. } => *bytes,
        Job::Windows(w) => w.iso_bytes,
        Job::ToGo(t) => t.image_bytes,
    };
    match partition {
        Some(p) => say!(
            json,
            "This erases partition {} of the drive. Nothing undoes it.",
            p.number
        ),
        None => say!(json, "This erases the drive. Nothing undoes it."),
    }
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
    if let Some(p) = partition {
        for line in partition_lines(p) {
            say!(json, "{line}");
        }
    }
    say!(json);
    let lines = match partition {
        Some(_) => vec!["The other partitions of the drive stay as they are.".to_string()],
        None => what_goes_on(job, layout),
    };
    for line in lines {
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
                ("edition", edition_json(job)),
                ("drive", drive_json(number, drive)),
                ("partition", partition_json(partition)),
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
            partition: None,
            skip_hardware_checks: false,
            no_microsoft_account: false,
            edition: None,
            to_go: false,
            bios: false,
        }
    }

    fn wim_fixture(name: &str) -> String {
        format!(
            "{}/../burnout-wim/tests/data/{name}",
            env!("CARGO_MANIFEST_DIR")
        )
    }

    fn to_go_args(image: &str, edition: &str) -> WriteArgs {
        WriteArgs {
            to_go: true,
            edition: Some(edition.to_string()),
            ..args(image)
        }
    }

    #[test]
    fn to_go_takes_a_wim_file_as_its_image() {
        let image = wim_fixture("to-go.wim");
        let job = to_go_job(&to_go_args(&image, "1")).unwrap();
        assert!(matches!(&job.input, WimInput::File(p) if p.display().to_string() == image));
        assert_eq!(job.edition.shown_name(), "Windows Test");
        assert_eq!(job.position, 1);
        assert_eq!(job.image_bytes, std::fs::metadata(&image).unwrap().len());
        // The tree of the edition reads through the input.
        let mut file = job.input.open().unwrap();
        let plan = to_go::plan(
            &mut file,
            &job.name(),
            &job.wim,
            1,
            600 << 20,
            "t",
            to_go::Firmware::Uefi,
        )
        .unwrap();
        assert_eq!(plan.boot_files, 3);
    }

    #[test]
    fn to_go_names_the_editions_of_a_wim_file_that_lacks_the_one_asked_for() {
        let Err(e) = to_go_job(&to_go_args(&wim_fixture("to-go.wim"), "Home")) else {
            panic!("refused")
        };
        assert_eq!(e.kind(), "no_such_edition");
        assert!(e.to_string().contains("1 Windows Test"), "{e}");
    }

    #[test]
    fn to_go_refuses_a_wim_file_that_lzms_compresses_and_tells_how_to_export_it() {
        let image = wim_fixture("lzms-solid.wim");
        let Err(e) = to_go_job(&to_go_args(&image, "Windows Test")) else {
            panic!("refused")
        };
        assert_eq!(e.kind(), "lzms_image");
        let text = e.to_string();
        assert!(
            text.contains(&format!(
                "dism /Export-Image /SourceImageFile:{image} /SourceIndex:1"
            )),
            "{text}"
        );
        assert!(
            text.contains("burnout write install.wim <drive> --to-go --edition 1"),
            "{text}"
        );
        if !cfg!(windows) {
            assert!(
                text.contains(&format!("wimlib-imagex export {image} 1 install.wim")),
                "{text}"
            );
        }
    }

    #[test]
    fn a_partition_takes_any_image_but_a_windows_iso() {
        let image = Path::new("data.img");
        for mode in [Mode::Raw, Mode::Neither] {
            assert_eq!(partition_mode(Ok(mode), image).unwrap(), Mode::Raw);
        }
        let e = partition_mode(Ok(Mode::Windows), Path::new("Win11.iso")).unwrap_err();
        assert_eq!(e.kind(), "windows_in_partition");
        assert!(
            e.to_string().starts_with("Win11.iso is a Windows ISO"),
            "{e}"
        );
        let fault = Error::MacosMedia {
            path: "Install.app".to_string(),
            what: "an installer app",
        };
        assert_eq!(
            partition_mode(Err(fault), image).unwrap_err().kind(),
            "macos_media"
        );
    }

    #[test]
    fn the_confirmation_names_the_partition_with_its_place_and_type() {
        let p = Partition {
            number: 2,
            first_byte: 1_048_576,
            bytes: 2_000_000_000,
            kind: burnout_core::PartitionType::Mbr(0x83),
        };
        assert_eq!(
            partition_lines(&p),
            vec![
                "    partition 2",
                "            2.0 GB (2,000,000,000 bytes)",
                "            from byte 1,048,576",
                "            Linux (0x83)",
            ]
        );
        let json = partition_json(Some(&p)).to_string();
        assert!(json.contains("\"number\":2"), "{json}");
        assert!(json.contains("\"first_byte\":1048576"), "{json}");
        assert_eq!(partition_json(None).to_string(), "null");
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
        let lines = windows_lines(&tree, &unattend, None, &layout);
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
        let lines = windows_lines(&tree, &plain, None, &layout);
        assert_eq!(
            lines[3],
            "autounattend.xml tells Setup where the install image is."
        );
        let pro = Edition {
            index: 6,
            name: "Windows 11 Pro".to_string(),
            ..Edition::default()
        };
        let lines = windows_lines(&tree, &plain, Some(&pro), &layout);
        assert_eq!(
            lines[3],
            "autounattend.xml tells Setup where the install image is, and chooses edition 6, \
             Windows 11 Pro, so Setup does not ask for one."
        );
    }

    #[test]
    fn the_editions_read_as_a_list() {
        let e = |index, name: &str| Edition {
            index,
            name: name.to_string(),
            ..Edition::default()
        };
        assert_eq!(editions_text(&[]), "none");
        assert_eq!(editions_text(&[e(1, "Home")]), "1 Home");
        assert_eq!(
            editions_text(&[e(1, "Home"), e(2, "Home N"), e(3, "Pro")]),
            "1 Home, 2 Home N and 3 Pro"
        );
    }

    #[test]
    fn an_edition_is_refused_for_an_image_that_goes_raw() {
        let mut a = args("ubuntu.iso");
        a.edition = Some("Pro".to_string());
        let e = refuse_windows_options(&a).unwrap_err();
        assert_eq!(e.kind(), "windows_option");
        assert!(e.to_string().starts_with("--edition"), "{e}");
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
