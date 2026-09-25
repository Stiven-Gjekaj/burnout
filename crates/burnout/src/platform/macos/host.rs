//! The macOS shim.
//!
//! This file makes system calls and decides nothing. It walks the IOKit
//! registry and turns what it finds into the plain data of
//! [`super::model`]. Every decision lives in [`super::parse`], whose tests
//! run on all three hosts.
//!
//! Nothing here is covered by a test, and nothing here may grow a rule.

use std::any::Any;
use std::collections::BTreeMap;
use std::ffi::{c_char, c_void, CStr, CString};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::data::CFData;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use core_foundation_sys::base::{CFAllocatorRef, CFRelease};
use core_foundation_sys::runloop::{
    kCFRunLoopDefaultMode, CFRunLoopGetCurrent, CFRunLoopRef, CFRunLoopRunInMode,
};
use io_kit_sys::keys::kIOServicePlane;
use io_kit_sys::types::io_object_t;
use io_kit_sys::{
    IOIteratorNext, IOObjectGetClass, IOObjectRelease, IORegistryEntryCreateCFProperties,
    IORegistryEntryGetParentEntry, IOServiceGetMatchingServices, IOServiceMatching,
};

use burnout_core::{BlockTarget, DriveAccess, DriveId, DriveInfo, DriveList, Error, Result};

use super::model::{Node, RegistrySnapshot, Value};
use super::parse;

/// Lists the drives of a running macOS host.
pub struct MacosDrives;

impl DriveList for MacosDrives {
    fn drives(&self) -> Result<Vec<DriveInfo>> {
        let snapshot = snapshot()?;
        // The device number of the root file system, without libc and
        // without statfs.
        let root_dev = std::fs::metadata("/").ok().map(|m| m.dev());
        parse::list_drives(&snapshot, root_dev)
    }
}

/// Walk every `IOMedia` in the registry, and the nodes above each one.
///
/// `IOServiceMatching` takes the subclasses too, so an APFS container arrives
/// here as well as a plain disk.
fn snapshot() -> Result<RegistrySnapshot> {
    let mut out = RegistrySnapshot::default();
    let class = CString::new("IOMedia").expect("a literal holds no zero byte");

    // SAFETY: the name is a valid C string. IOServiceGetMatchingServices
    // takes the matching dictionary, so this code does not release it.
    let iterator = unsafe {
        let matching = IOServiceMatching(class.as_ptr());
        if matching.is_null() {
            return Err(Error::Host {
                source: "IOServiceMatching".to_string(),
                detail: "the matching dictionary is null".to_string(),
            });
        }
        let mut iterator: io_object_t = 0;
        let status = IOServiceGetMatchingServices(0, matching, &mut iterator);
        if status != 0 {
            return Err(Error::Host {
                source: "IOServiceGetMatchingServices".to_string(),
                detail: format!("the call returned {status}"),
            });
        }
        iterator
    };

    loop {
        // SAFETY: the iterator came from IOServiceGetMatchingServices.
        let entry = unsafe { IOIteratorNext(iterator) };
        if entry == 0 {
            break;
        }
        let media = chain(&mut out, entry);
        out.media.push(media);
        // SAFETY: IOIteratorNext gives an entry that this code owns.
        unsafe { IOObjectRelease(entry) };
    }
    // SAFETY: the iterator is owned here and is used no more.
    unsafe { IOObjectRelease(iterator) };

    Ok(out)
}

/// Record one entry and every node above it, and give back the index of the
/// entry itself.
///
/// An ancestor that two drives share is recorded twice. That costs a little
/// memory and it keeps this function simple, and nothing above compares one
/// node against another.
fn chain(out: &mut RegistrySnapshot, entry: io_object_t) -> usize {
    let index = out.nodes.len();
    out.nodes.push(node(entry));

    let mut here = entry;
    let mut child = index;
    // The registry is a few levels deep. The count guards against a loop
    // that a future version of the system might introduce.
    for _ in 0..64 {
        let mut parent: io_object_t = 0;
        // SAFETY: `here` is a live entry, and the plane name is a constant
        // C string from IOKit.
        let status = unsafe { IORegistryEntryGetParentEntry(here, kIOServicePlane, &mut parent) };
        if status != 0 || parent == 0 {
            break;
        }
        let at = out.nodes.len();
        out.nodes.push(node(parent));
        out.nodes[child].parent = Some(at);
        child = at;
        if here != entry {
            // SAFETY: every parent after the first is owned here.
            unsafe { IOObjectRelease(here) };
        }
        here = parent;
    }
    if here != entry {
        // SAFETY: the last parent is owned here.
        unsafe { IOObjectRelease(here) };
    }
    index
}

/// Read the class and the properties of one entry.
fn node(entry: io_object_t) -> Node {
    let mut class_name = [0_i8; 128];
    // SAFETY: the buffer is large enough for any IOKit class name.
    unsafe { IOObjectGetClass(entry, class_name.as_mut_ptr()) };
    // SAFETY: IOObjectGetClass writes a zero terminated string.
    let class = unsafe { CStr::from_ptr(class_name.as_ptr()) }
        .to_string_lossy()
        .into_owned();

    let mut node = Node::new(&class);
    let mut properties = std::ptr::null_mut();
    // SAFETY: the entry is live. The call creates the dictionary, so the
    // wrapper below takes it under the create rule and releases it.
    let status = unsafe {
        IORegistryEntryCreateCFProperties(entry, &mut properties, std::ptr::null_mut(), 0)
    };
    if status == 0 && !properties.is_null() {
        // SAFETY: the call above created this dictionary.
        let dict: CFDictionary = unsafe { CFDictionary::wrap_under_create_rule(properties as _) };
        node.properties = dictionary(&dict);
    }
    node
}

/// Turn a Core Foundation dictionary into plain data.
fn dictionary(dict: &CFDictionary) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    let (keys, values) = dict.get_keys_and_values();
    for (key, value) in keys.into_iter().zip(values) {
        if key.is_null() || value.is_null() {
            continue;
        }
        // SAFETY: the keys and the values belong to the dictionary, which
        // outlives this loop. Both are taken under the get rule.
        let (key, value) = unsafe {
            (
                CFString::wrap_under_get_rule(key as CFStringRef),
                CFType::wrap_under_get_rule(value as CFTypeRef),
            )
        };
        if let Some(value) = convert(&value) {
            out.insert(key.to_string(), value);
        }
    }
    out
}

/// Turn one Core Foundation value into plain data.
///
/// A kind that this code does not know is dropped rather than guessed at.
fn convert(value: &CFType) -> Option<Value> {
    if let Some(b) = value.downcast::<CFBoolean>() {
        return Some(Value::Bool(b == CFBoolean::true_value()));
    }
    if let Some(n) = value.downcast::<CFNumber>() {
        return n.to_i64().map(Value::Int);
    }
    if let Some(s) = value.downcast::<CFString>() {
        return Some(Value::Text(s.to_string()));
    }
    if let Some(d) = value.downcast::<CFData>() {
        return Some(Value::Data(d.bytes().to_vec()));
    }
    if let Some(d) = value.downcast::<CFDictionary>() {
        return Some(Value::Dict(dictionary(&d)));
    }
    if let Some(a) = value.downcast::<CFArray>() {
        let mut items = Vec::new();
        for index in 0..a.len() {
            let Some(item) = a.get(index) else { continue };
            // SAFETY: the item belongs to the array, which outlives this
            // loop, and it is taken under the get rule.
            let item = unsafe { CFType::wrap_under_get_rule(*item as CFTypeRef) };
            if let Some(item) = convert(&item) {
                items.push(item);
            }
        }
        return Some(Value::List(items));
    }
    None
}

/// Opens the drives of a running macOS host.
pub struct MacosAccess;

impl DriveAccess for MacosAccess {
    type Target = MacosDisk;

    fn unmount_volumes(&self, id: &DriveId) -> Result<()> {
        unmount_whole(id.as_str())
    }

    fn keep_unmounted(&self, id: &DriveId) -> Result<Box<dyn Any>> {
        Ok(Box::new(MountRefusal::start(id.as_str())?))
    }

    fn eject(&self, id: &DriveId) -> Result<()> {
        eject_whole(id.as_str())
    }

    fn open(&self, id: &DriveId) -> Result<MacosDisk> {
        let info = one_drive(id)?;
        // The raw node, and never /dev/diskN. The raw node skips the buffer
        // cache, so what is written is on the medium and a read back is a
        // read of the medium.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&info.node)
            .map_err(|e| super::super::busy_or(e, &info.node))?;
        Ok(MacosDisk {
            file,
            sector_size: info.logical_sector_size,
            length: info.size_bytes,
        })
    }
}

/// The description of one drive, out of the list that already works.
///
/// The size and the sector size come from the same registry walk that the
/// list command uses, so there is one place where they are read and one place
/// where that reading can be wrong.
fn one_drive(id: &DriveId) -> Result<DriveInfo> {
    MacosDrives
        .drives()?
        .into_iter()
        .find(|d| d.id == *id)
        .ok_or_else(|| Error::NoSuchDrive {
            wanted: id.as_str().to_string(),
        })
}

/// Take every volume of one whole disk off its mount point.
///
/// Disk Arbitration and not `diskutil`. The framework is the interface that
/// `diskutil` itself calls, and a program is a tool of the host.
fn unmount_whole(bsd_name: &str) -> Result<()> {
    settle(bsd_name, "DADiskUnmount", |disk, context| {
        // SAFETY: the disk is live for the length of this call, and the
        // context is the Outcome that settle owns.
        unsafe { DADiskUnmount(disk, UNMOUNT_WHOLE, answered, context) }
    })
}

/// Eject one whole disk, so that macOS lets go of it until it is connected
/// again.
fn eject_whole(bsd_name: &str) -> Result<()> {
    settle(bsd_name, "DADiskEject", |disk, context| {
        // SAFETY: the disk is live for the length of this call, and the
        // context is the Outcome that settle owns.
        unsafe { DADiskEject(disk, EJECT_DEFAULT, answered, context) }
    })
}

/// Send one request about a whole disk to Disk Arbitration, and wait for the
/// answer.
///
/// A request is asynchronous: it goes to a run loop and answers through a
/// callback. So this schedules the session, lets `request` send the request
/// with [`answered`] and the context it gets, runs the loop until the callback
/// arrives, and reads what the callback recorded. `source` names the request
/// in an error.
fn settle(
    bsd_name: &str,
    source: &str,
    request: impl FnOnce(DADiskRef, *mut c_void),
) -> Result<()> {
    let name = CString::new(bsd_name).map_err(|_| Error::Host {
        source: bsd_name.to_string(),
        detail: "the name holds a zero byte".to_string(),
    })?;

    // SAFETY: every pointer below is checked before use, each one is
    // released once, and the run loop belongs to this thread.
    unsafe {
        let session = DASessionCreate(std::ptr::null());
        if session.is_null() {
            return Err(Error::Host {
                source: "DASessionCreate".to_string(),
                detail: "the session is null".to_string(),
            });
        }
        let disk = DADiskCreateFromBSDName(std::ptr::null(), session, name.as_ptr());
        if disk.is_null() {
            CFRelease(session as CFTypeRef);
            return Err(Error::NoSuchDrive {
                wanted: bsd_name.to_string(),
            });
        }

        let run_loop = CFRunLoopGetCurrent();
        DASessionScheduleWithRunLoop(session, run_loop, kCFRunLoopDefaultMode);

        let mut outcome = Outcome {
            answered: false,
            status: 0,
        };
        request(disk, &mut outcome as *mut Outcome as *mut c_void);

        // A deadline, because a file system that will not let go would
        // otherwise hold this here for ever with nothing on the screen.
        let deadline = Instant::now() + Duration::from_secs(30);
        while !outcome.answered && Instant::now() < deadline {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.25, 1);
        }

        DASessionUnscheduleFromRunLoop(session, run_loop, kCFRunLoopDefaultMode);
        CFRelease(disk as CFTypeRef);
        CFRelease(session as CFTypeRef);

        if !outcome.answered {
            return Err(Error::Host {
                source: source.to_string(),
                detail: format!("no answer came for {bsd_name} in thirty seconds"),
            });
        }
        match outcome.status {
            0 => {}
            DA_UNIX_BUSY | DA_RETURN_BUSY | EXCLUSIVE_ACCESS => {
                return Err(Error::InUse {
                    what: bsd_name.to_string(),
                });
            }
            status => {
                return Err(Error::Host {
                    source: source.to_string(),
                    detail: format!("{bsd_name} refused with status {status:#010x}"),
                });
            }
        }
    }
    Ok(())
}

/// What the callback of a request recorded.
struct Outcome {
    answered: bool,
    status: i32,
}

/// The callback that Disk Arbitration calls when a request is settled.
///
/// A null dissenter means the request happened. Anything else carries the
/// reason it did not.
extern "C" fn answered(_disk: DADiskRef, dissenter: DADissenterRef, context: *mut c_void) {
    // SAFETY: the context is the Outcome that settle owns, and that Outcome
    // outlives the run loop that calls this.
    let outcome = unsafe { &mut *(context as *mut Outcome) };
    outcome.answered = true;
    outcome.status = if dissenter.is_null() {
        0
    } else {
        // SAFETY: the dissenter is live for the length of this call.
        unsafe { DADissenterGetStatus(dissenter) }
    };
}

/// Refuses each mount of a volume of one disk, for as long as it lives.
///
/// Disk Arbitration asks each session that registered an approval callback
/// before it mounts a volume, and a dissenter refuses the mount. The session
/// runs on a thread of its own, because the write holds this thread for
/// minutes and Disk Arbitration waits for the answer.
struct MountRefusal {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl MountRefusal {
    /// Start to refuse, and return when Disk Arbitration has the callback.
    fn start(bsd_name: &str) -> Result<Self> {
        let name = CString::new(bsd_name).map_err(|_| Error::Host {
            source: bsd_name.to_string(),
            detail: "the name holds a zero byte".to_string(),
        })?;
        let stop = Arc::new(AtomicBool::new(false));
        let (ready, registered) = mpsc::channel();
        let flag = Arc::clone(&stop);
        let thread = thread::spawn(move || refuse_mounts(name, &flag, &ready));
        match registered.recv() {
            Ok(Ok(())) => Ok(MountRefusal {
                stop,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err(Error::Host {
                source: "DARegisterDiskMountApprovalCallback".to_string(),
                detail: "the thread of the session stopped".to_string(),
            }),
        }
    }
}

impl Drop for MountRefusal {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The thread of a [`MountRefusal`]: register, answer until told to stop,
/// then unregister.
fn refuse_mounts(name: CString, stop: &AtomicBool, ready: &mpsc::Sender<Result<()>>) {
    // SAFETY: the session is checked before use and released once, the run
    // loop belongs to this thread, and the context is the name, which lives
    // until the callback is unregistered.
    unsafe {
        let session = DASessionCreate(std::ptr::null());
        if session.is_null() {
            let _ = ready.send(Err(Error::Host {
                source: "DASessionCreate".to_string(),
                detail: "the session is null".to_string(),
            }));
            return;
        }
        let run_loop = CFRunLoopGetCurrent();
        DASessionScheduleWithRunLoop(session, run_loop, kCFRunLoopDefaultMode);
        let context = name.as_ptr() as *mut c_void;
        DARegisterDiskMountApprovalCallback(session, std::ptr::null(), refuse, context);
        let _ = ready.send(Ok(()));

        while !stop.load(Ordering::SeqCst) {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.25, 1);
        }

        DAUnregisterApprovalCallback(session, refuse as *mut c_void, context);
        DASessionUnscheduleFromRunLoop(session, run_loop, kCFRunLoopDefaultMode);
        CFRelease(session as CFTypeRef);
    }
    drop(name);
}

/// The approval callback: a dissenter for a volume of the disk that the
/// context names, and no answer for any other disk.
extern "C" fn refuse(disk: DADiskRef, context: *mut c_void) -> DADissenterRef {
    // SAFETY: the context is the name that refuse_mounts owns, and the whole
    // disk is checked before use and released once.
    unsafe {
        let wanted = CStr::from_ptr(context as *const c_char);
        let whole = DADiskCopyWholeDisk(disk);
        if whole.is_null() {
            return std::ptr::null();
        }
        let name = DADiskGetBSDName(whole);
        let ours = !name.is_null() && CStr::from_ptr(name) == wanted;
        CFRelease(whole as CFTypeRef);
        if ours {
            DADissenterCreate(std::ptr::null(), EXCLUSIVE_ACCESS, std::ptr::null())
        } else {
            std::ptr::null()
        }
    }
}

/// One open macOS drive.
#[derive(Debug)]
pub struct MacosDisk {
    file: File,
    sector_size: u32,
    length: u64,
}

impl Read for MacosDisk {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.file.read(buf)
    }
}

impl Write for MacosDisk {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Seek for MacosDisk {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.file.seek(pos)
    }
}

impl BlockTarget for MacosDisk {
    fn logical_sector_size(&self) -> u32 {
        self.sector_size
    }

    fn length(&self) -> u64 {
        self.length
    }

    fn sync(&mut self) -> Result<()> {
        self.file.flush()?;
        // `fsync` on the raw node answers ENOTTY, because the raw node has no
        // buffer of the operating system to empty. The drive still has one of
        // its own, and this is the call that empties that.
        //
        // SAFETY: the descriptor is open for the length of this call, and the
        // control takes no argument.
        let code = unsafe { libc::ioctl(self.file.as_raw_fd(), DKIOCSYNCHRONIZECACHE) };
        if code != 0 {
            return Err(Error::Io(io::Error::last_os_error()));
        }
        Ok(())
    }
}

/// `DKIOCSYNCHRONIZECACHE`, which tells the drive to put its own cache on the
/// medium.
///
/// `_IO('d', 22)` out of `<sys/disk.h>`, written out because `libc` does not
/// carry it.
const DKIOCSYNCHRONIZECACHE: libc::c_ulong = 0x2000_6416;

/// `unix_err(EBUSY)` of `<mach/error.h>`, the answer to a request for a
/// volume that a program uses. Measured: the unmount of a disk image with a
/// file open on its volume answers this.
const DA_UNIX_BUSY: i32 = 0x0000_C000 | libc::EBUSY;

/// `kDAReturnBusy` of Disk Arbitration.
const DA_RETURN_BUSY: i32 = 0xF8DA_0002_u32 as i32;

/// `kDADiskUnmountOptionWhole`, which takes every volume of the disk.
const UNMOUNT_WHOLE: u32 = 0x0000_0001;

/// `kDADiskEjectOptionDefault`.
const EJECT_DEFAULT: u32 = 0;

/// `kDAReturnExclusiveAccess`, the reason that a refused mount gives. A
/// request for a disk that another program holds for itself answers it too.
const EXCLUSIVE_ACCESS: i32 = 0xF8DA_0004_u32 as i32;

type DASessionRef = *const c_void;
type DADiskRef = *const c_void;
type DADissenterRef = *const c_void;
type DADiskUnmountCallback = extern "C" fn(DADiskRef, DADissenterRef, *mut c_void);
type DADiskEjectCallback = extern "C" fn(DADiskRef, DADissenterRef, *mut c_void);
type DADiskMountApprovalCallback = extern "C" fn(DADiskRef, *mut c_void) -> DADissenterRef;

// Disk Arbitration has no binding crate that this project would depend on, so
// declare the functions it uses. This is the fallback that the plan named for
// IOKit, and it is the whole of the interface.
#[link(name = "DiskArbitration", kind = "framework")]
extern "C" {
    fn DASessionCreate(allocator: CFAllocatorRef) -> DASessionRef;
    fn DASessionScheduleWithRunLoop(
        session: DASessionRef,
        run_loop: CFRunLoopRef,
        mode: CFStringRef,
    );
    fn DASessionUnscheduleFromRunLoop(
        session: DASessionRef,
        run_loop: CFRunLoopRef,
        mode: CFStringRef,
    );
    fn DADiskCreateFromBSDName(
        allocator: CFAllocatorRef,
        session: DASessionRef,
        name: *const c_char,
    ) -> DADiskRef;
    fn DADiskUnmount(
        disk: DADiskRef,
        options: u32,
        callback: DADiskUnmountCallback,
        context: *mut c_void,
    );
    fn DADiskEject(
        disk: DADiskRef,
        options: u32,
        callback: DADiskEjectCallback,
        context: *mut c_void,
    );
    fn DADissenterGetStatus(dissenter: DADissenterRef) -> i32;
    fn DADissenterCreate(
        allocator: CFAllocatorRef,
        status: i32,
        string: CFStringRef,
    ) -> DADissenterRef;
    fn DARegisterDiskMountApprovalCallback(
        session: DASessionRef,
        matching: CFTypeRef,
        callback: DADiskMountApprovalCallback,
        context: *mut c_void,
    );
    fn DAUnregisterApprovalCallback(
        session: DASessionRef,
        callback: *mut c_void,
        context: *mut c_void,
    );
    fn DADiskCopyWholeDisk(disk: DADiskRef) -> DADiskRef;
    fn DADiskGetBSDName(disk: DADiskRef) -> *const c_char;
}
