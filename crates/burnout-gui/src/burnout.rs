//! How the program finds `burnout`, checks its version, and reads its drives.
//!
//! The program writes nothing itself. Each thing that it knows about a drive
//! comes from `burnout list --json`, so this module only reads text.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// The name of the binary of `burnout` on this host.
const BINARY: &str = if cfg!(windows) {
    "burnout.exe"
} else {
    "burnout"
};

/// One drive, as `burnout list --json` gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drive {
    /// The number that `burnout` prints beside the drive.
    pub number: u64,
    /// The name of the drive, as the host gives it.
    pub name: String,
    /// The size in bytes.
    pub size_bytes: u64,
    /// The bus, such as USB.
    pub bus: String,
    /// Whether the host says that the drive is removable.
    pub removable: bool,
    /// Whether the system starts from this drive.
    pub system: bool,
}

impl Drive {
    /// What the drive is, in words for a person.
    pub fn kind(&self) -> &'static str {
        if self.system {
            "system disk"
        } else if self.removable {
            "removable"
        } else {
            "fixed"
        }
    }
}

/// The drives in the output of `burnout list --json`.
pub fn parse_list(text: &str) -> Result<Vec<Drive>, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|e| format!("burnout gave no JSON: {e}"))?;
    let list = value["drives"]
        .as_array()
        .ok_or("the JSON of burnout holds no list of drives")?;
    list.iter()
        .map(|d| {
            let field = |name: &str| format!("a drive has no field {name}");
            Ok(Drive {
                number: d["number"].as_u64().ok_or_else(|| field("number"))?,
                name: d["name"].as_str().ok_or_else(|| field("name"))?.to_string(),
                size_bytes: d["size_bytes"]
                    .as_u64()
                    .ok_or_else(|| field("size_bytes"))?,
                bus: d["bus"].as_str().unwrap_or("").to_string(),
                removable: d["removable"].as_bool().unwrap_or(false),
                system: d["system"].as_bool().unwrap_or(false),
            })
        })
        .collect()
}

/// The version in the output of `burnout --version`, such as `3.3.0` from
/// `burnout 3.3.0`.
pub fn version_of(output: &str) -> Option<&str> {
    let mut words = output.split_whitespace();
    match (words.next(), words.next()) {
        (Some("burnout"), Some(version)) => Some(version),
        _ => None,
    }
}

/// A size in bytes, in words for a person, as `burnout` prints it.
pub fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "kB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} bytes")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Find `burnout`: first beside this program, then on the path.
///
/// A package installs the two side by side, so the one beside the program
/// is the one of the same release.
pub fn find(beside: Option<&Path>, path: Option<&OsStr>) -> Option<PathBuf> {
    let here = beside.map(|dir| dir.join(BINARY));
    let on_path = path
        .into_iter()
        .flat_map(std::env::split_paths)
        .map(|dir| dir.join(BINARY));
    here.into_iter().chain(on_path).find(|p| p.is_file())
}

/// A `burnout` that this program can use.
pub struct Burnout {
    path: PathBuf,
}

impl Burnout {
    /// Find `burnout` for this process, and refuse one of another version.
    ///
    /// The events of `--json` are the contract between the two programs, so
    /// a `burnout` of another release can mean something else by them.
    pub fn locate() -> Result<Self, String> {
        let exe = std::env::current_exe().ok();
        let beside = exe.as_deref().and_then(Path::parent);
        let path = find(beside, std::env::var_os("PATH").as_deref())
            .ok_or("Burnout cannot find the program burnout. Install it beside this program, or on the path")?;
        let burnout = Self { path };
        let output = burnout.run(&["--version"])?;
        let wanted = env!("CARGO_PKG_VERSION");
        match version_of(&output) {
            Some(v) if v == wanted => Ok(burnout),
            Some(v) => Err(format!(
                "{} is version {v}, and this program needs version {wanted}. Install the two from the same release",
                burnout.path.display()
            )),
            None => Err(format!(
                "{} did not say its version",
                burnout.path.display()
            )),
        }
    }

    /// The drives that `burnout list --json` gives.
    pub fn list(&self) -> Result<Vec<Drive>, String> {
        parse_list(&self.run(&["list", "--json"])?)
    }

    /// Run `burnout` with no privilege, and give back its output.
    fn run(&self, args: &[&str]) -> Result<String, String> {
        let mut command = Command::new(&self.path);
        command.args(args);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW: no console window flashes open for a child.
            command.creation_flags(0x0800_0000);
        }
        let output = command
            .output()
            .map_err(|e| format!("{} did not start: {e}", self.path.display()))?;
        if !output.status.success() {
            return Err(format!(
                "burnout {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        String::from_utf8(output.stdout)
            .map_err(|e| format!("burnout gave text that is not UTF-8: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_gives_each_drive_and_what_it_is() {
        let text = r#"{"drives":[
            {"number":1,"id":"disk0","node":"/dev/rdisk0","name":"APPLE SSD","size_bytes":500277790720,"bus":"Apple Fabric","removable":false,"system":true},
            {"number":2,"id":"disk4","node":"/dev/rdisk4","name":"SanDisk Ultra","size_bytes":30752636928,"bus":"USB","removable":true,"system":false},
            {"number":3,"id":"disk5","node":"/dev/rdisk5","name":"Big Disk","size_bytes":4000787030016,"bus":"USB","removable":false,"system":false}
        ],"system_disk_known":true}"#;
        let drives = parse_list(text).unwrap();
        assert_eq!(drives.len(), 3);
        assert_eq!(drives[1].name, "SanDisk Ultra");
        assert_eq!(drives[1].number, 2);
        let kinds: Vec<_> = drives.iter().map(Drive::kind).collect();
        assert_eq!(kinds, ["system disk", "removable", "fixed"]);
    }

    #[test]
    fn a_list_with_no_drives_field_is_refused() {
        assert!(parse_list(r#"{"event":"error"}"#).is_err());
        assert!(parse_list("not json").is_err());
        assert!(parse_list(r#"{"drives":[{"number":1}]}"#).is_err());
    }

    #[test]
    fn the_version_comes_from_the_second_word() {
        assert_eq!(version_of("burnout 3.3.0\n"), Some("3.3.0"));
        assert_eq!(version_of("other 3.3.0"), None);
        assert_eq!(version_of(""), None);
    }

    #[test]
    fn a_size_has_one_decimal_in_units_of_1000() {
        assert_eq!(size_text(512), "512 bytes");
        assert_eq!(size_text(30_752_636_928), "30.8 GB");
        assert_eq!(size_text(419_430_400), "419.4 MB");
    }

    #[test]
    fn burnout_beside_the_program_comes_before_the_one_on_the_path() {
        let root = std::env::temp_dir().join(format!("burnout-gui-find-{}", std::process::id()));
        let (here, there) = (root.join("here"), root.join("there"));
        std::fs::create_dir_all(&here).unwrap();
        std::fs::create_dir_all(&there).unwrap();
        std::fs::write(there.join(BINARY), b"").unwrap();
        let path = std::env::join_paths([&there]).unwrap();

        assert_eq!(find(Some(&here), Some(&path)), Some(there.join(BINARY)));
        std::fs::write(here.join(BINARY), b"").unwrap();
        assert_eq!(find(Some(&here), Some(&path)), Some(here.join(BINARY)));
        assert_eq!(find(Some(&root), None), None);

        std::fs::remove_dir_all(&root).unwrap();
    }
}
