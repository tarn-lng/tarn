//! Explicit compilation targets (ADR 0101, P1). The backend never asks the
//! compiling machine what it is generating code for; it asks a `Target`.

use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Arch {
    X86_64,
    Aarch64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Os {
    Linux,
    Macos,
    /// No operating system: a relocatable ELF object for bare metal.
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Target {
    pub arch: Arch,
    pub os: Os,
}

/// Every accepted `--target` spelling, in documentation order.
pub const TARGETS: [&str; 6] = ["x86_64-linux", "aarch64-linux", "x86_64-macos", "aarch64-macos", "x86_64-none", "aarch64-none"];

impl Target {
    /// The machine running the compiler, if Tarn supports it.
    pub fn host() -> Option<Target> {
        let arch = match std::env::consts::ARCH {
            "x86_64" => Arch::X86_64,
            "aarch64" => Arch::Aarch64,
            _ => return None,
        };
        let os = match std::env::consts::OS {
            "linux" => Os::Linux,
            "macos" => Os::Macos,
            _ => return None,
        };
        if !cfg!(target_endian = "little") {
            return None;
        }
        Some(Target { arch, os })
    }

    pub fn is_host(&self) -> bool {
        Target::host() == Some(*self)
    }

    pub fn is_macos(&self) -> bool {
        self.os == Os::Macos
    }

    /// Position-independent code: AArch64 non-PIC symbol addresses use
    /// relocations cranelift-object 0.125.3 cannot emit, and Mach-O
    /// executables are always position independent (ADR 0098).
    pub fn is_pic(&self) -> bool {
        self.arch == Arch::Aarch64 || self.os == Os::Macos
    }

    /// Default object-file extension for this target.
    pub fn object_extension(&self) -> &'static str {
        "o"
    }

    pub(crate) fn triple(&self) -> target_lexicon::Triple {
        use target_lexicon::{Aarch64Architecture, Architecture, BinaryFormat, Environment, OperatingSystem, Triple, Vendor};
        let architecture = match self.arch {
            Arch::X86_64 => Architecture::X86_64,
            Arch::Aarch64 => Architecture::Aarch64(Aarch64Architecture::Aarch64),
        };
        let (vendor, operating_system, environment, binary_format) = match self.os {
            Os::Linux => (Vendor::Unknown, OperatingSystem::Linux, Environment::Gnu, BinaryFormat::Elf),
            Os::Macos => (Vendor::Apple, OperatingSystem::Darwin(None), Environment::Unknown, BinaryFormat::Macho),
            Os::None => (Vendor::Unknown, OperatingSystem::None_, Environment::Unknown, BinaryFormat::Elf),
        };
        Triple { architecture, vendor, operating_system, environment, binary_format }
    }
}

impl FromStr for Target {
    type Err = String;
    fn from_str(s: &str) -> Result<Target, String> {
        let unknown = || format!("unknown target `{s}`; expected one of {}", TARGETS.iter().map(|t| format!("`{t}`")).collect::<Vec<_>>().join(", "));
        let (arch, os) = s.split_once('-').ok_or_else(unknown)?;
        let arch = match arch {
            "x86_64" => Arch::X86_64,
            "aarch64" | "arm64" => Arch::Aarch64,
            _ => return Err(unknown()),
        };
        let os = match os {
            "linux" => Os::Linux,
            "macos" | "darwin" => Os::Macos,
            "none" => Os::None,
            _ => return Err(unknown()),
        };
        Ok(Target { arch, os })
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let arch = match self.arch {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "aarch64",
        };
        let os = match self.os {
            Os::Linux => "linux",
            Os::Macos => "macos",
            Os::None => "none",
        };
        write!(f, "{arch}-{os}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_target_round_trips() {
        for name in TARGETS {
            let target: Target = name.parse().unwrap();
            assert_eq!(target.to_string(), name);
        }
    }

    #[test]
    fn aliases_and_errors() {
        assert_eq!("arm64-darwin".parse::<Target>().unwrap().to_string(), "aarch64-macos");
        let error = "riscv64-none".parse::<Target>().unwrap_err();
        assert!(error.contains("unknown target `riscv64-none`") && error.contains("`x86_64-none`"), "{error}");
        assert!("x86_64".parse::<Target>().is_err());
    }

    #[test]
    fn host_is_listed() {
        if let Some(host) = Target::host() {
            assert!(TARGETS.contains(&host.to_string().as_str()));
            assert!(host.is_host());
        }
    }
}
