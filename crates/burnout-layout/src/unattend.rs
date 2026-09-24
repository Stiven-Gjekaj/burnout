//! The `autounattend.xml` that goes on every Windows drive.
//!
//! Windows Setup reads this file from the root of the boot partition. The
//! install image is on the other partition, and Setup looks for it only
//! beside `setup.exe`, so the file names the image. [The spike] measured
//! that, and the rest of what the file holds:
//!
//! - A search that finds the volume which holds the install image and gives
//!   it the letter `W:`. Windows PE gives letters itself, and a second disk in
//!   the machine moves them.
//! - The `InstallFrom` path, on `W:`.
//!
//! Each option adds its entries, and nothing else goes in. The rest of Setup
//! stays for the person to answer.
//!
//! [The spike]: https://github.com/Stiven-Gjekaj/burnout/blob/main/docs/spike-layout.md

use std::fmt::Write;

/// The architecture of the image. Setup ignores a component of another
/// architecture, so each component names this one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Architecture {
    X86,
    Amd64,
    Arm64,
}

impl Architecture {
    fn name(self) -> &'static str {
        match self {
            Architecture::X86 => "x86",
            Architecture::Amd64 => "amd64",
            Architecture::Arm64 => "arm64",
        }
    }
}

/// The install image of a Windows ISO, in `sources`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallImage {
    Wim,
    Esd,
}

impl InstallImage {
    /// The name of the file in `sources`.
    pub fn file_name(self) -> &'static str {
        match self {
            InstallImage::Wim => "install.wim",
            InstallImage::Esd => "install.esd",
        }
    }
}

/// What the file holds besides what every Windows drive needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unattend {
    pub architecture: Architecture,
    pub image: InstallImage,
    /// The edition to install, by its index in the image.
    pub edition: Option<u32>,
    /// Turn off the checks of Windows 11 for TPM, Secure Boot, RAM, CPU and
    /// storage.
    pub skip_hardware_checks: bool,
    /// Take away the step that asks for a Microsoft account, and the network
    /// page before it, so Setup asks for a local account.
    pub no_microsoft_account: bool,
}

/// The five checks of Windows 11 that `LabConfig` turns off.
const CHECKS: [&str; 5] = [
    "BypassTPMCheck",
    "BypassSecureBootCheck",
    "BypassRAMCheck",
    "BypassCPUCheck",
    "BypassStorageCheck",
];

/// The key that lets the first-run setup go on with no network. Without it,
/// Windows 11 25H2 stops at "Let's connect you to a network" on a machine
/// with no network, whatever `HideOnlineAccountScreens` says. Measured on
/// build 26200.8037.
const BYPASS_NRO: &str =
    "reg add HKLM\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\OOBE /v BypassNRO /t REG_DWORD /d 1 /f";

/// The letters that the search tries. `W` is the letter that it gives, and
/// `X` is the RAM disk of Windows PE.
const LETTERS: &str = "C D E F G H I J K L M N O P Q R S T U V Y Z";

/// The attributes that each component of Windows Setup carries.
fn component(name: &str, architecture: Architecture) -> String {
    format!(
        "<component name=\"{name}\" processorArchitecture=\"{}\" \
         publicKeyToken=\"31bf3856ad364e35\" language=\"neutral\" versionScope=\"nonSxS\">",
        architecture.name()
    )
}

/// Characters that XML gives a meaning to.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The text of `autounattend.xml`.
pub fn unattend_xml(u: &Unattend) -> String {
    let image = u.image.file_name();
    let mut commands = vec![format!(
        "cmd /c for %d in ({LETTERS}) do @if exist %d:\\sources\\{image} \
         ((echo select volume=%d&echo assign letter=W)>X:\\bo.txt&diskpart /s X:\\bo.txt)"
    )];
    if u.skip_hardware_checks {
        for check in CHECKS {
            commands.push(format!(
                "reg add HKLM\\SYSTEM\\Setup\\LabConfig /v {check} /t REG_DWORD /d 1 /f"
            ));
        }
    }

    let mut x = String::new();
    x.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    x.push_str(
        "<unattend xmlns=\"urn:schemas-microsoft-com:unattend\" \
         xmlns:wcm=\"http://schemas.microsoft.com/WMIConfig/2002/State\">\n",
    );
    x.push_str(
        "  <!-- Burnout wrote this file. The install image is on the second \
         partition, and Setup finds it only through the path below. -->\n",
    );
    x.push_str("  <settings pass=\"windowsPE\">\n");
    let _ = writeln!(
        x,
        "    {}",
        component("Microsoft-Windows-Setup", u.architecture)
    );
    x.push_str("      <RunSynchronous>\n");
    for (order, command) in commands.iter().enumerate() {
        x.push_str("        <RunSynchronousCommand wcm:action=\"add\">\n");
        let _ = writeln!(x, "          <Order>{}</Order>", order + 1);
        let _ = writeln!(x, "          <Path>{}</Path>", escape(command));
        x.push_str("        </RunSynchronousCommand>\n");
    }
    x.push_str("      </RunSynchronous>\n");
    x.push_str("      <ImageInstall>\n        <OSImage>\n          <InstallFrom>\n");
    let _ = writeln!(x, "            <Path>W:\\sources\\{image}</Path>");
    if let Some(index) = u.edition {
        x.push_str("            <MetaData wcm:action=\"add\">\n");
        x.push_str("              <Key>/IMAGE/INDEX</Key>\n");
        let _ = writeln!(x, "              <Value>{index}</Value>");
        x.push_str("            </MetaData>\n");
    }
    x.push_str("          </InstallFrom>\n        </OSImage>\n      </ImageInstall>\n");
    x.push_str("    </component>\n  </settings>\n");
    if u.no_microsoft_account {
        // The key goes in before the first-run setup starts, in the pass
        // that sets up the system that Setup installed.
        x.push_str("  <settings pass=\"specialize\">\n");
        let _ = writeln!(
            x,
            "    {}",
            component("Microsoft-Windows-Deployment", u.architecture)
        );
        x.push_str("      <RunSynchronous>\n");
        x.push_str("        <RunSynchronousCommand wcm:action=\"add\">\n");
        x.push_str("          <Order>1</Order>\n");
        let _ = writeln!(x, "          <Path>{}</Path>", escape(BYPASS_NRO));
        x.push_str("        </RunSynchronousCommand>\n");
        x.push_str("      </RunSynchronous>\n");
        x.push_str("    </component>\n  </settings>\n");
        x.push_str("  <settings pass=\"oobeSystem\">\n");
        let _ = writeln!(
            x,
            "    {}",
            component("Microsoft-Windows-Shell-Setup", u.architecture)
        );
        x.push_str("      <OOBE>\n");
        x.push_str("        <HideOnlineAccountScreens>true</HideOnlineAccountScreens>\n");
        x.push_str("      </OOBE>\n");
        x.push_str("    </component>\n  </settings>\n");
    }
    x.push_str("</unattend>\n");
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() -> Unattend {
        Unattend {
            architecture: Architecture::Amd64,
            image: InstallImage::Wim,
            edition: None,
            skip_hardware_checks: false,
            no_microsoft_account: false,
        }
    }

    /// The names of the elements in the order that they open and close. A
    /// file whose elements do not nest is refused.
    fn elements(xml: &str) -> Vec<String> {
        let mut open: Vec<String> = Vec::new();
        let mut seen = Vec::new();
        let mut rest = xml;
        while let Some(at) = rest.find('<') {
            let end = rest[at..].find('>').expect("a tag ends") + at;
            let tag = &rest[at + 1..end];
            rest = &rest[end + 1..];
            if tag.starts_with('?') || tag.starts_with("!--") {
                continue;
            }
            let name = tag
                .trim_start_matches('/')
                .split_whitespace()
                .next()
                .unwrap()
                .to_string();
            if tag.starts_with('/') {
                assert_eq!(open.pop().as_deref(), Some(name.as_str()), "{xml}");
            } else if !tag.ends_with('/') {
                seen.push(name.clone());
                open.push(name);
            }
        }
        assert!(open.is_empty(), "{open:?} stay open");
        seen
    }

    #[test]
    fn every_drive_gets_the_search_and_the_path() {
        let xml = unattend_xml(&plain());
        assert!(xml.contains(
            "<Path>cmd /c for %d in (C D E F G H I J K L M N O P Q R S T U V Y Z) do @if exist \
             %d:\\sources\\install.wim ((echo select volume=%d&amp;echo assign letter=W)&gt;X:\\bo.txt\
             &amp;diskpart /s X:\\bo.txt)</Path>"
        ));
        assert!(xml.contains("<Path>W:\\sources\\install.wim</Path>"));
        assert!(!xml.contains("MetaData"));
        assert!(!xml.contains("LabConfig"));
        assert!(!xml.contains("oobeSystem"));
        assert!(!xml.contains("ProductKey"), "Setup asks for the key");
        let names = elements(&xml);
        assert_eq!(
            names
                .iter()
                .filter(|n| *n == "RunSynchronousCommand")
                .count(),
            1
        );
    }

    #[test]
    fn the_search_and_the_path_name_an_esd_image_too() {
        let esd = Unattend {
            image: InstallImage::Esd,
            ..plain()
        };
        let xml = unattend_xml(&esd);
        assert!(xml.contains("@if exist %d:\\sources\\install.esd "));
        assert!(xml.contains("<Path>W:\\sources\\install.esd</Path>"));
    }

    #[test]
    fn the_search_never_tries_w_or_the_ram_disk() {
        let letters: Vec<&str> = LETTERS.split(' ').collect();
        assert_eq!(letters.len(), 22);
        assert!(!letters.contains(&"W") && !letters.contains(&"X"));
        assert!(!letters.contains(&"A") && !letters.contains(&"B"));
    }

    #[test]
    fn the_hardware_checks_add_five_commands_after_the_search() {
        let xml = unattend_xml(&Unattend {
            skip_hardware_checks: true,
            ..plain()
        });
        let names = elements(&xml);
        assert_eq!(
            names
                .iter()
                .filter(|n| *n == "RunSynchronousCommand")
                .count(),
            6
        );
        for (n, check) in CHECKS.iter().enumerate() {
            let command = format!(
                "<Order>{}</Order>\n          <Path>reg add HKLM\\SYSTEM\\Setup\\LabConfig /v {check} /t REG_DWORD /d 1 /f</Path>",
                n + 2
            );
            assert!(xml.contains(&command), "{check}");
        }
    }

    #[test]
    fn an_edition_adds_its_index() {
        let xml = unattend_xml(&Unattend {
            edition: Some(6),
            ..plain()
        });
        assert!(xml.contains("<Key>/IMAGE/INDEX</Key>\n              <Value>6</Value>"));
        elements(&xml);
    }

    #[test]
    fn no_microsoft_account_lets_the_first_run_go_on_with_no_network() {
        let xml = unattend_xml(&Unattend {
            no_microsoft_account: true,
            ..plain()
        });
        assert!(xml.contains("<component name=\"Microsoft-Windows-Deployment\""));
        assert!(xml.contains(
            "<Path>reg add HKLM\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\OOBE /v BypassNRO \
             /t REG_DWORD /d 1 /f</Path>"
        ));
        let passes: Vec<&str> = xml
            .match_indices("<settings pass=\"")
            .map(|(at, found)| {
                let rest = &xml[at + found.len()..];
                &rest[..rest.find('"').unwrap()]
            })
            .collect();
        assert_eq!(passes, ["windowsPE", "specialize", "oobeSystem"]);
        assert!(xml.contains("<HideOnlineAccountScreens>true</HideOnlineAccountScreens>"));
        assert!(
            !xml.contains("LocalAccount"),
            "the person makes the account"
        );
        elements(&xml);
    }

    #[test]
    fn each_component_names_the_architecture_of_the_image() {
        for (architecture, name) in [
            (Architecture::X86, "x86"),
            (Architecture::Amd64, "amd64"),
            (Architecture::Arm64, "arm64"),
        ] {
            let xml = unattend_xml(&Unattend {
                architecture,
                no_microsoft_account: true,
                ..plain()
            });
            let wanted = format!("processorArchitecture=\"{name}\"");
            assert_eq!(xml.matches("<component ").count(), 3);
            assert_eq!(xml.matches(&wanted).count(), 3, "{name}");
        }
    }

    #[test]
    fn the_file_nests_with_every_option() {
        let names = elements(&unattend_xml(&Unattend {
            edition: Some(3),
            skip_hardware_checks: true,
            no_microsoft_account: true,
            ..plain()
        }));
        assert_eq!(names[0], "unattend");
        assert_eq!(names.iter().filter(|n| *n == "settings").count(), 3);
    }
}
