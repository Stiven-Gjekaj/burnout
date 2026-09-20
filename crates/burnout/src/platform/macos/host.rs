//! The macOS shim.
//!
//! This file makes system calls and decides nothing. It walks the IOKit
//! registry and turns what it finds into the plain data of
//! [`super::model`]. Every decision lives in [`super::parse`], whose tests
//! run on all three hosts.
//!
//! Nothing here is covered by a test, and nothing here may grow a rule.

use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::os::unix::fs::MetadataExt;

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::data::CFData;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use io_kit_sys::keys::kIOServicePlane;
use io_kit_sys::types::io_object_t;
use io_kit_sys::{
    IOIteratorNext, IOObjectGetClass, IOObjectRelease, IORegistryEntryCreateCFProperties,
    IORegistryEntryGetParentEntry, IOServiceGetMatchingServices, IOServiceMatching,
};

use burnout_core::{DriveInfo, DriveList, Error, Result};

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
