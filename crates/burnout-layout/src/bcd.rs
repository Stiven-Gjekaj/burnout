//! The BCD store of a drive of Windows To Go: the objects and the elements
//! that start Windows from its NTFS partition through UEFI.
//!
//! The objects and the elements are the ones that bcdboot of Windows 11
//! 25H2 wrote for such a drive, measured on 2026-11-11, without the entries
//! of the Windows Recovery Environment and of the firmware, which Windows
//! adds when it starts. A device element names a partition of a GPT disk by
//! the GUID of the partition and of the disk.

use crate::hive::{Key, Value};

/// The objects that each store of Windows has, by their well-known GUIDs.
const BOOTMGR: &str = "{9dea862c-5cdd-4e70-acc1-f32b344d4795}";
const GLOBAL_SETTINGS: &str = "{7ea2e1ac-2e61-4728-aaa3-896d9d0a9f0e}";
const BOOTLOADER_SETTINGS: &str = "{6efb52bf-1766-41db-a6b3-0ee5eff72bd7}";
const RESUMELOADER_SETTINGS: &str = "{1afa9c49-16ab-4a5c-901b-212802da9460}";
const HYPERVISOR_SETTINGS: &str = "{7ff607e0-4395-11db-b0de-0800200c9a66}";
const DBG_SETTINGS: &str = "{4636856e-540f-4170-a130-a84776f4c654}";
const EMS_SETTINGS: &str = "{0ce4991b-e6b3-4b16-b23c-5e0d9250e5d9}";
const BAD_MEMORY: &str = "{5189b25c-5558-4bf2-bca4-289b11bd29e2}";

/// What the store needs to know about the drive.
#[derive(Clone, Debug)]
pub struct BcdDrive {
    /// The GUID of the GPT disk, and of the EFI and Windows partitions, in
    /// the byte order of the GPT.
    pub disk: [u8; 16],
    pub efi_partition: [u8; 16],
    pub windows_partition: [u8; 16],
    /// The GUIDs of the new loader and resume objects.
    pub loader: [u8; 16],
    pub resume: [u8; 16],
    /// Such as "Windows 11".
    pub description: String,
    /// Such as "en-US".
    pub locale: String,
}

/// A GUID in the form of the registry, from the byte order of the GPT.
pub fn guid_text(g: &[u8; 16]) -> String {
    format!(
        "{{{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}}}",
        u32::from_le_bytes(g[0..4].try_into().unwrap()),
        u16::from_le_bytes(g[4..6].try_into().unwrap()),
        u16::from_le_bytes(g[6..8].try_into().unwrap()),
        g[8],
        g[9],
        g[10],
        g[11],
        g[12],
        g[13],
        g[14],
        g[15]
    )
}

/// The device element of a partition of a GPT disk.
pub fn partition_device(disk: &[u8; 16], partition: &[u8; 16]) -> Vec<u8> {
    let mut d = vec![0u8; 88];
    d[16..20].copy_from_slice(&6u32.to_le_bytes());
    d[24..28].copy_from_slice(&0x48u32.to_le_bytes());
    d[32..48].copy_from_slice(partition);
    d[56..72].copy_from_slice(disk);
    d
}

/// A string. bcdboot writes some with one more null character at the end
/// and some without, so the null that the registry needs is enough.
fn text(s: &str) -> Value {
    Value::string(s)
}

/// A list of objects, which ends with two null characters as the registry
/// needs.
fn list(items: &[&str]) -> Value {
    Value::strings(items)
}

fn integer(v: u64) -> Value {
    Value::binary(&v.to_le_bytes())
}

fn boolean(v: bool) -> Value {
    Value::binary(&[v as u8])
}

/// Add an object with its type and its elements.
fn object(root: &mut Key, id: &str, kind: u32, elements: Vec<(&str, Value)>) {
    let o = root.key(&["Objects", id]);
    o.key(&["Description"]).set("Type", Value::dword(kind));
    o.key(&["Elements"]);
    for (element, value) in elements {
        o.key(&["Elements", element]).set("Element", value);
    }
}

/// The store for `drive`.
pub fn bcd_store(drive: &BcdDrive) -> Key {
    let mut root = Key::default();
    let d = root.key(&["Description"]);
    d.set("KeyName", text("BCD00000000"));
    d.set("System", Value::dword(1));
    d.set("TreatAsSystem", Value::dword(1));

    let loader = guid_text(&drive.loader);
    let resume = guid_text(&drive.resume);
    let efi = partition_device(&drive.disk, &drive.efi_partition);
    let windows = partition_device(&drive.disk, &drive.windows_partition);
    let locale = text(&drive.locale);

    object(
        &mut root,
        BOOTMGR,
        0x1010_0002,
        vec![
            ("11000001", Value::binary(&efi)),
            ("12000002", text("\\EFI\\Microsoft\\Boot\\bootmgfw.efi")),
            ("12000004", text("Windows Boot Manager")),
            ("12000005", locale.clone()),
            ("14000006", list(&[GLOBAL_SETTINGS])),
            ("23000003", text(&loader)),
            ("23000006", text(&resume)),
            ("24000001", list(&[&loader])),
            ("25000004", integer(30)),
        ],
    );
    object(
        &mut root,
        &loader,
        0x1020_0003,
        vec![
            ("11000001", Value::binary(&windows)),
            ("12000002", text("\\Windows\\system32\\winload.efi")),
            ("12000004", text(&drive.description)),
            ("12000005", locale.clone()),
            ("14000006", list(&[BOOTLOADER_SETTINGS])),
            ("15000066", integer(3)),
            ("16000060", boolean(true)),
            ("17000077", Value::binary(&[0x75, 0, 0, 0x15, 0, 0, 0, 0])),
            ("21000001", Value::binary(&windows)),
            ("22000002", text("\\Windows")),
            ("23000003", text(&resume)),
            ("25000020", integer(0)),
            ("250000c2", integer(1)),
        ],
    );
    object(
        &mut root,
        &resume,
        0x1020_0004,
        vec![
            ("11000001", Value::binary(&windows)),
            ("12000002", text("\\Windows\\system32\\winresume.efi")),
            ("12000004", text("Windows Resume Application")),
            ("12000005", locale),
            ("14000006", list(&[RESUMELOADER_SETTINGS])),
            ("16000060", boolean(true)),
            ("17000077", Value::binary(&[0x75, 0, 0, 0x15, 0, 0, 0, 0])),
            ("21000001", Value::binary(&windows)),
            ("22000002", text("\\hiberfil.sys")),
            ("25000008", integer(1)),
            ("26000006", boolean(false)),
        ],
    );
    object(
        &mut root,
        GLOBAL_SETTINGS,
        0x2010_0000,
        vec![("14000006", list(&[DBG_SETTINGS, EMS_SETTINGS, BAD_MEMORY]))],
    );
    object(
        &mut root,
        BOOTLOADER_SETTINGS,
        0x2020_0003,
        vec![("14000006", list(&[GLOBAL_SETTINGS, HYPERVISOR_SETTINGS]))],
    );
    object(
        &mut root,
        RESUMELOADER_SETTINGS,
        0x2020_0004,
        vec![("14000006", list(&[GLOBAL_SETTINGS]))],
    );
    object(
        &mut root,
        HYPERVISOR_SETTINGS,
        0x2020_0003,
        vec![
            ("250000f3", integer(0)),
            ("250000f4", integer(1)),
            ("250000f5", integer(0x1c200)),
        ],
    );
    object(
        &mut root,
        DBG_SETTINGS,
        0x2010_0000,
        vec![("15000011", integer(4))],
    );
    object(
        &mut root,
        EMS_SETTINGS,
        0x2010_0000,
        vec![("16000020", boolean(false))],
    );
    object(&mut root, BAD_MEMORY, 0x2010_0000, Vec::new());
    root
}

/// The bytes of the store for `drive`, as the file `\\EFI\\Microsoft\\Boot\\BCD`
/// holds them. The keys get a descriptor that gives SYSTEM and the
/// administrators full control.
pub fn store_bytes(drive: &BcdDrive, time: u64) -> Result<Vec<u8>, String> {
    // KEY_ALL_ACCESS.
    let descriptor = crate::ntfs::descriptor_for_hive(0x000F_003F);
    crate::hive::write_hive(&bcd_store(drive), "\\BCD", &descriptor, time)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hive::read::read_hive;
    use crate::hive::write_hive;

    fn hex(s: &str) -> [u8; 16] {
        let b: Vec<u8> = (0..32)
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect();
        b.try_into().unwrap()
    }

    /// The drive of the store that bcdboot wrote in the test.
    fn drive() -> BcdDrive {
        BcdDrive {
            disk: hex("4c77374cd3ccbf41939d83f4e4a04d67"),
            efi_partition: hex("8f92cd7c9a535e47a873580cf9c83043"),
            windows_partition: hex("8a7ef740982b2649a1325aca253ce7ef"),
            loader: hex("0b93ffd5daddf111be6cc8cd87dcce89"),
            resume: hex("0a93ffd5daddf111be6cc8cd87dcce89"),
            description: "Windows 11".to_string(),
            locale: "en-US".to_string(),
        }
    }

    #[test]
    fn a_guid_reads_as_the_registry_writes_it() {
        assert_eq!(
            guid_text(&hex("0b93ffd5daddf111be6cc8cd87dcce89")),
            "{d5ff930b-ddda-11f1-be6c-c8cd87dcce89}"
        );
    }

    #[test]
    fn a_partition_device_is_the_one_that_bcdboot_writes() {
        let d = partition_device(
            &hex("4c77374cd3ccbf41939d83f4e4a04d67"),
            &hex("8a7ef740982b2649a1325aca253ce7ef"),
        );
        let bcdboot = "00000000000000000000000000000000060000000000000048000000000000008a7ef740982b2649a1325aca253ce7ef00000000000000004c77374cd3ccbf41939d83f4e4a04d6700000000000000000000000000000000";
        let want: Vec<u8> = (0..bcdboot.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&bcdboot[i..i + 2], 16).unwrap())
            .collect();
        assert_eq!(d, want);
    }

    #[test]
    fn each_object_matches_the_store_of_bcdboot() {
        // The store that bcdboot wrote, when it is at hand.
        let Ok(path) = std::env::var("BCD_BCDBOOT") else {
            return;
        };
        let (_, theirs) = read_hive(&std::fs::read(path).unwrap()).unwrap();
        let ours = bcd_store(&drive());
        for (id, object) in &ours.keys["Objects"].keys {
            let other = &theirs.keys["Objects"].keys[id];
            assert_eq!(
                object.keys["Description"].values["Type"], other.keys["Description"].values["Type"],
                "{id}"
            );
            for (element, value) in &object.keys["Elements"].keys {
                let trim = |k: &Key| {
                    let mut v = k.values["Element"].clone();
                    if v.kind == crate::hive::REG_SZ {
                        while v.data.ends_with(&[0, 0]) {
                            v.data.truncate(v.data.len() - 2);
                        }
                    }
                    v
                };
                assert_eq!(
                    trim(value),
                    trim(&other.keys["Elements"].keys[element]),
                    "{id} {element}"
                );
            }
        }
    }

    #[test]
    fn the_store_writes_and_reads_back() {
        let store = bcd_store(&drive());
        let hive = write_hive(
            &store,
            "\\BCD",
            &[
                1, 0, 4, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
            1,
        )
        .unwrap();
        let (_, back) = read_hive(&hive).unwrap();
        assert_eq!(back, store);
        assert_eq!(back.keys["Objects"].keys.len(), 10);
    }
}
