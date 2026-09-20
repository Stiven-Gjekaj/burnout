//! The Windows shim.
//!
//! This file makes system calls and decides nothing. Every answer it gets is
//! a buffer of bytes that [`super::parse`] reads, and that module is tested
//! on all three hosts.
//!
//! **Every call here runs without Administrator.** The device handle asks for
//! no access at all, and each IO control below is `FILE_ANY_ACCESS`:
//!
//! | Control | Code | Access |
//! | --- | --- | --- |
//! | `IOCTL_STORAGE_GET_DEVICE_NUMBER` | `0x002D1080` | `FILE_ANY_ACCESS` |
//! | `IOCTL_STORAGE_QUERY_PROPERTY` | `0x002D1400` | `FILE_ANY_ACCESS` |
//! | `IOCTL_DISK_GET_DRIVE_GEOMETRY_EX` | `0x000700A0` | `FILE_ANY_ACCESS` |
//! | `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS` | `0x00560000` | `FILE_ANY_ACCESS` |
//!
//! `IOCTL_DISK_GET_LENGTH_INFO` would give the size in one call, and it is
//! declared `FILE_READ_ACCESS`. That needs a handle opened with
//! `GENERIC_READ`, which needs Administrator. Do not use it here.

use std::collections::BTreeSet;
use std::ffi::c_void;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};

use windows_sys::core::GUID;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
    SetupDiGetDeviceInterfaceDetailW, SetupDiGetDeviceRegistryPropertyW, DIGCF_DEVICEINTERFACE,
    DIGCF_PRESENT, HDEVINFO, SPDRP_FRIENDLYNAME, SPDRP_REMOVAL_POLICY, SP_DEVICE_INTERFACE_DATA,
    SP_DEVINFO_DATA,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FindFirstVolumeW, FindNextVolumeW, FindVolumeClose, FlushFileBuffers, ReadFile,
    SetFilePointerEx, WriteFile, FILE_BEGIN, FILE_CURRENT, FILE_END, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW;
use windows_sys::Win32::System::IO::DeviceIoControl;

use burnout_core::{BlockTarget, DriveAccess, DriveId, DriveInfo, DriveList, Error, Result};

use super::parse;
use super::raw::{RawDisk, RawVolume};

/// `SetupDiGetClassDevsW` gives back an `isize` here, and not a pointer, so
/// the invalid value is written out rather than taken from `HANDLE`.
const INVALID_DEVICE_SET: HDEVINFO = -1;

const GUID_DEVINTERFACE_DISK: GUID = GUID {
    data1: 0x53f5_6307,
    data2: 0xb6bf,
    data3: 0x11d0,
    data4: [0x94, 0xf2, 0x00, 0xa0, 0xc9, 0x1e, 0xfb, 0x8b],
};

const IOCTL_STORAGE_GET_DEVICE_NUMBER: u32 = 0x002D_1080;
const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
const IOCTL_DISK_GET_DRIVE_GEOMETRY_EX: u32 = 0x0007_00A0;
const IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS: u32 = 0x0056_0000;

const STORAGE_DEVICE_PROPERTY: u32 = 0;
const STORAGE_ACCESS_ALIGNMENT_PROPERTY: u32 = 6;
const PROPERTY_STANDARD_QUERY: u32 = 0;

/// Lists the drives of a running Windows host.
pub struct WindowsDrives;

impl DriveList for WindowsDrives {
    fn drives(&self) -> Result<Vec<DriveInfo>> {
        let disks = enumerate()?;
        let system = system_disk_numbers();
        Ok(parse::list_drives(&disks, &system))
    }
}

fn wide(text: &str) -> Vec<u16> {
    std::ffi::OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Open a device for a question and not for a write.
///
/// The access mask is zero. That is what lets `burnout list` run without
/// Administrator, and it is the reason every control code above is
/// `FILE_ANY_ACCESS`.
fn open_for_query(path: &[u16]) -> Option<Handle> {
    // SAFETY: the path is a zero terminated wide string.
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        None
    } else {
        Some(Handle(handle))
    }
}

/// A device handle that closes itself.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateFileW and is closed once.
        unsafe { CloseHandle(self.0) };
    }
}

/// Send one control code and give back what the driver wrote.
fn control(handle: &Handle, code: u32, input: &[u8], want: usize) -> Option<Vec<u8>> {
    let mut out = vec![0u8; want];
    let mut written: u32 = 0;
    // SAFETY: both buffers are owned here and their lengths are given.
    let ok = unsafe {
        DeviceIoControl(
            handle.0,
            code,
            if input.is_empty() {
                std::ptr::null()
            } else {
                input.as_ptr() as *const c_void
            },
            input.len() as u32,
            out.as_mut_ptr() as *mut c_void,
            out.len() as u32,
            &mut written,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return None;
    }
    out.truncate(written as usize);
    Some(out)
}

/// Ask a storage property, in the two calls that the interface needs.
///
/// The first call fills an eight byte header that says how much there is.
/// The second call reads that much.
fn storage_property(handle: &Handle, property: u32) -> Option<Vec<u8>> {
    // STORAGE_PROPERTY_QUERY: PropertyId, QueryType, AdditionalParameters.
    let mut query = [0u8; 12];
    query[0..4].copy_from_slice(&property.to_le_bytes());
    query[4..8].copy_from_slice(&PROPERTY_STANDARD_QUERY.to_le_bytes());

    let header = control(handle, IOCTL_STORAGE_QUERY_PROPERTY, &query, 8)?;
    if header.len() < 8 {
        return None;
    }
    let size = u32::from_le_bytes(header[4..8].try_into().ok()?) as usize;
    if size == 0 || size > 64 * 1024 {
        return None;
    }
    control(handle, IOCTL_STORAGE_QUERY_PROPERTY, &query, size)
}

/// Walk every disk that the system has now.
fn enumerate() -> Result<Vec<RawDisk>> {
    // SAFETY: the flags are valid and the other arguments are null.
    let set = unsafe {
        SetupDiGetClassDevsW(
            &GUID_DEVINTERFACE_DISK,
            std::ptr::null(),
            std::ptr::null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    };
    if set == INVALID_DEVICE_SET {
        return Err(Error::Host {
            source: "SetupDiGetClassDevsW".to_string(),
            detail: "the device set could not be opened".to_string(),
        });
    }

    let mut disks = Vec::new();
    let mut index = 0u32;
    loop {
        let mut interface: SP_DEVICE_INTERFACE_DATA = unsafe { std::mem::zeroed() };
        interface.cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
        // SAFETY: the set is live and the structure carries its own size.
        let more = unsafe {
            SetupDiEnumDeviceInterfaces(
                set,
                std::ptr::null_mut(),
                &GUID_DEVINTERFACE_DISK,
                index,
                &mut interface,
            )
        };
        if more == 0 {
            break;
        }
        index += 1;
        if let Some(disk) = one_disk(set, &mut interface) {
            disks.push(disk);
        }
    }

    // SAFETY: the set is owned here and is used no more.
    unsafe { SetupDiDestroyDeviceInfoList(set) };
    Ok(disks)
}

/// Ask everything about one device interface.
fn one_disk(set: HDEVINFO, interface: &mut SP_DEVICE_INTERFACE_DATA) -> Option<RawDisk> {
    let mut needed: u32 = 0;
    // SAFETY: a null buffer asks for the size only.
    unsafe {
        SetupDiGetDeviceInterfaceDetailW(
            set,
            interface,
            std::ptr::null_mut(),
            0,
            &mut needed,
            std::ptr::null_mut(),
        )
    };
    if needed == 0 {
        return None;
    }

    let mut detail = vec![0u8; needed as usize];
    // SP_DEVICE_INTERFACE_DETAIL_DATA_W begins with cbSize and then the path.
    // The size field names the structure and not the buffer.
    let cb_size: u32 = if cfg!(target_pointer_width = "64") {
        8
    } else {
        6
    };
    detail[0..4].copy_from_slice(&cb_size.to_le_bytes());

    let mut info: SP_DEVINFO_DATA = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SP_DEVINFO_DATA>() as u32;

    // SAFETY: the buffer is as large as the call asked for.
    let ok = unsafe {
        SetupDiGetDeviceInterfaceDetailW(
            set,
            interface,
            detail.as_mut_ptr() as *mut _,
            needed,
            std::ptr::null_mut(),
            &mut info,
        )
    };
    if ok == 0 {
        return None;
    }

    // The path is a wide string that starts after cbSize.
    let path_bytes = &detail[4..];
    let mut path: Vec<u16> = path_bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    if let Some(end) = path.iter().position(|c| *c == 0) {
        path.truncate(end);
    }
    let interface_path = std::ffi::OsString::from_wide(&path)
        .to_string_lossy()
        .into_owned();
    path.push(0);

    let handle = open_for_query(&path)?;

    let number_bytes = control(&handle, IOCTL_STORAGE_GET_DEVICE_NUMBER, &[], 12)?;
    // STORAGE_DEVICE_NUMBER: DeviceType, DeviceNumber, PartitionNumber.
    let device_number = u32::from_le_bytes(number_bytes.get(4..8)?.try_into().ok()?);

    Some(RawDisk {
        interface_path,
        device_number,
        device_descriptor: storage_property(&handle, STORAGE_DEVICE_PROPERTY)?,
        geometry: control(&handle, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, &[], 32)?,
        alignment: storage_property(&handle, STORAGE_ACCESS_ALIGNMENT_PROPERTY),
        friendly_name: registry_text(set, &mut info, SPDRP_FRIENDLYNAME),
        removal_policy: registry_u32(set, &mut info, SPDRP_REMOVAL_POLICY),
    })
}

fn registry_text(set: HDEVINFO, info: &mut SP_DEVINFO_DATA, property: u32) -> Option<String> {
    let mut buffer = [0u8; 512];
    let mut needed: u32 = 0;
    // SAFETY: the buffer and its length are given together.
    let ok = unsafe {
        SetupDiGetDeviceRegistryPropertyW(
            set,
            info,
            property,
            std::ptr::null_mut(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            &mut needed,
        )
    };
    if ok == 0 {
        return None;
    }
    let wide: Vec<u16> = buffer[..(needed as usize).min(buffer.len())]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|c| *c != 0)
        .collect();
    let text = std::ffi::OsString::from_wide(&wide)
        .to_string_lossy()
        .trim()
        .to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn registry_u32(set: HDEVINFO, info: &mut SP_DEVINFO_DATA, property: u32) -> Option<u32> {
    let mut buffer = [0u8; 4];
    let mut needed: u32 = 0;
    // SAFETY: the buffer and its length are given together.
    let ok = unsafe {
        SetupDiGetDeviceRegistryPropertyW(
            set,
            info,
            property,
            std::ptr::null_mut(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            &mut needed,
        )
    };
    if ok == 0 || needed < 4 {
        return None;
    }
    Some(u32::from_le_bytes(buffer))
}

/// The disks that the running system starts from.
///
/// The Windows directory names the volume, and the volume names its disks.
/// A volume that spans or mirrors sits on more than one, and every one of
/// them carries the running system.
///
/// The EFI partition carries no drive letter, so this route does not reach
/// it. On every machine but a hand built one it sits on the same disk as
/// Windows, which this does reach.
fn system_disk_numbers() -> BTreeSet<u32> {
    let mut out = BTreeSet::new();
    let mut buffer = [0u16; 260];
    // SAFETY: the buffer and its length are given together.
    let written = unsafe { GetWindowsDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) };
    if written == 0 {
        return out;
    }
    let directory = std::ffi::OsString::from_wide(&buffer[..written as usize])
        .to_string_lossy()
        .into_owned();
    let Some(letter) = directory.chars().next() else {
        return out;
    };

    let path = wide(&format!(r"\\.\{letter}:"));
    let Some(handle) = open_for_query(&path) else {
        return out;
    };
    let Some(bytes) = control(
        &handle,
        IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
        &[],
        8 + 24 * 16,
    ) else {
        return out;
    };
    if let Ok(numbers) = parse::disk_extents(&bytes) {
        out.extend(numbers);
    }
    out
}

/// Opens the drives of a running Windows host.
///
/// **This needs Administrator**, and the rest of this file does not. A write
/// handle and the two volume controls below are the reason.
pub struct WindowsAccess;

impl DriveAccess for WindowsAccess {
    type Target = WindowsDisk;

    fn unmount_volumes(&self, _id: &DriveId) -> Result<()> {
        // Nothing here, and that is not an oversight.
        //
        // `FSCTL_LOCK_VOLUME` holds only while a handle holds it. A call that
        // locked a volume and returned would have dropped the lock before the
        // write began, and the file system would come back during the write.
        // So `open` locks every volume and keeps the handles for as long as
        // the drive is open.
        Ok(())
    }

    fn open(&self, id: &DriveId) -> Result<WindowsDisk> {
        let number: u32 = id.as_str().parse().map_err(|_| Error::NoSuchDrive {
            wanted: id.as_str().to_string(),
        })?;
        let info = one_drive(id)?;

        // Take the volumes first. A write to the disk under a mounted volume
        // gives a file system that disagrees with the bytes beneath it.
        let mut locks = Vec::new();
        for path in parse::volumes_on_disk(&volumes(), number) {
            locks.push(lock_volume(&path)?);
        }

        let handle = open_for_write(&wide(&format!(r"\\.\PhysicalDrive{number}")))?;
        Ok(WindowsDisk {
            handle,
            _locks: locks,
            sector_size: info.logical_sector_size,
            length: info.size_bytes,
        })
    }
}

/// The description of one drive, out of the list that already works.
fn one_drive(id: &DriveId) -> Result<DriveInfo> {
    WindowsDrives
        .drives()?
        .into_iter()
        .find(|d| d.id == *id)
        .ok_or_else(|| Error::NoSuchDrive {
            wanted: id.as_str().to_string(),
        })
}

/// Every volume that the system has now, with the disks each one sits on.
fn volumes() -> Vec<RawVolume> {
    let mut out = Vec::new();
    let mut name = [0u16; 260];
    // SAFETY: the buffer and its length are given together.
    let search = unsafe { FindFirstVolumeW(name.as_mut_ptr(), name.len() as u32) };
    if search == INVALID_HANDLE_VALUE {
        return out;
    }
    loop {
        let text = from_wide(&name);
        let extents = parse::volume_path_to_open(&text)
            .map(|path| wide(&path))
            .and_then(|path| open_for_query(&path))
            .and_then(|handle| {
                control(
                    &handle,
                    IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
                    &[],
                    8 + 24 * 16,
                )
            });
        out.push(RawVolume {
            guid_path: text,
            extents,
        });

        // SAFETY: the search handle is live and the buffer carries its size.
        let more = unsafe { FindNextVolumeW(search, name.as_mut_ptr(), name.len() as u32) };
        if more == 0 {
            break;
        }
    }
    // SAFETY: the search handle is closed once, here.
    unsafe { FindVolumeClose(search) };
    out
}

/// Read a zero terminated wide string out of a fixed buffer.
fn from_wide(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    std::ffi::OsString::from_wide(&buffer[..end])
        .to_string_lossy()
        .into_owned()
}

/// Take one volume away from the file system, and hold it.
///
/// The lock comes first and the dismount second. A dismount with no lock
/// leaves the file system free to mount the volume again during the write.
fn lock_volume(path: &str) -> Result<Handle> {
    let handle = open_for_write(&wide(path))?;
    if control(&handle, FSCTL_LOCK_VOLUME, &[], 0).is_none() {
        return Err(Error::Host {
            source: "FSCTL_LOCK_VOLUME".to_string(),
            detail: format!("{path} is in use, so close what is reading it and try again"),
        });
    }
    if control(&handle, FSCTL_DISMOUNT_VOLUME, &[], 0).is_none() {
        return Err(Error::Host {
            source: "FSCTL_DISMOUNT_VOLUME".to_string(),
            detail: format!("{path} did not dismount"),
        });
    }
    Ok(handle)
}

/// Open a device for a write.
///
/// This is the one call in this file that needs Administrator.
fn open_for_write(path: &[u16]) -> Result<Handle> {
    // SAFETY: the path is a zero terminated wide string.
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
            return Err(Error::NeedsPrivilege {
                remedy: "Start a Command Prompt or PowerShell with \"Run as administrator\", \
                         and run the same command there."
                    .to_string(),
            });
        }
        return Err(Error::Io(error));
    }
    Ok(Handle(handle))
}

/// One open Windows disk, with the volumes of that disk locked.
pub struct WindowsDisk {
    handle: Handle,
    /// The locks. They do nothing but exist, and the drive is writable for
    /// exactly as long as they do.
    _locks: Vec<Handle>,
    sector_size: u32,
    length: u64,
}

impl Read for WindowsDisk {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut read: u32 = 0;
        // SAFETY: the buffer is owned here and its length is given.
        let ok = unsafe {
            ReadFile(
                self.handle.0,
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(read as usize)
    }
}

impl Write for WindowsDisk {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut written: u32 = 0;
        // SAFETY: the buffer is owned here and its length is given.
        let ok = unsafe {
            WriteFile(
                self.handle.0,
                buf.as_ptr(),
                buf.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(written as usize)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        // SAFETY: the handle is live.
        let ok = unsafe { FlushFileBuffers(self.handle.0) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Seek for WindowsDisk {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let (method, distance) = match pos {
            SeekFrom::Start(n) => (FILE_BEGIN, n as i64),
            SeekFrom::End(n) => (FILE_END, n),
            SeekFrom::Current(n) => (FILE_CURRENT, n),
        };
        let mut now: i64 = 0;
        // SAFETY: the handle is live and the output is owned here.
        let ok = unsafe { SetFilePointerEx(self.handle.0, distance, &mut now, method) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(now as u64)
    }
}

impl BlockTarget for WindowsDisk {
    fn logical_sector_size(&self) -> u32 {
        self.sector_size
    }

    fn length(&self) -> u64 {
        self.length
    }

    fn sync(&mut self) -> Result<()> {
        self.flush()?;
        Ok(())
    }
}

const FSCTL_LOCK_VOLUME: u32 = 0x0009_0018;
const FSCTL_DISMOUNT_VOLUME: u32 = 0x0009_0020;

/// Whether this process may open one device for a write.
///
/// The handle is closed at once and nothing is written. This is how the
/// command line finds out that it needs Administrator, and a token that says
/// elevated is not the same answer: the device is the one that decides.
pub fn can_open_for_write(path: &[u16]) -> bool {
    !matches!(open_for_write(path), Err(Error::NeedsPrivilege { .. }))
}
