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
use std::os::windows::ffi::{OsStrExt, OsStringExt};

use windows_sys::core::GUID;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
    SetupDiGetDeviceInterfaceDetailW, SetupDiGetDeviceRegistryPropertyW, DIGCF_DEVICEINTERFACE,
    DIGCF_PRESENT, HDEVINFO, SPDRP_FRIENDLYNAME, SPDRP_REMOVAL_POLICY, SP_DEVICE_INTERFACE_DATA,
    SP_DEVINFO_DATA,
};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW;
use windows_sys::Win32::System::IO::DeviceIoControl;

use burnout_core::{DriveInfo, DriveList, Error, Result};

use super::parse;
use super::raw::RawDisk;

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
