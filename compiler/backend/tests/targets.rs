//! Explicit targets (ADR 0101, P1): one host emits objects for every
//! supported target, and each object names that target in its header.
use tarn_backend::{Arch, Os, Target, TARGETS};

/// (format, machine) read straight from the object header.
fn header(bytes: &[u8]) -> (&'static str, u32) {
    if bytes.starts_with(b"\x7fELF") {
        assert_eq!(bytes[4], 2, "ELF64");
        assert_eq!(bytes[5], 1, "little endian");
        assert_eq!(u16::from_le_bytes([bytes[16], bytes[17]]), 1, "ET_REL: relocatable, nothing linked");
        ("elf", u16::from_le_bytes([bytes[18], bytes[19]]) as u32)
    } else if bytes.starts_with(&0xfeedfacf_u32.to_le_bytes()) {
        ("macho", u32::from_le_bytes(bytes[4..8].try_into().unwrap()))
    } else {
        panic!("unknown object format {:02x?}", &bytes[..4]);
    }
}

#[test]
fn every_target_emits_its_own_object() {
    let res = tarn_driver::check(std::path::Path::new("../../benchmarks/native/arithmetic.tarn")).unwrap();
    assert!(!res.has_errors(), "{:?}", res.diagnostics);
    for name in TARGETS {
        let target: Target = name.parse().unwrap();
        let bytes = tarn_backend::emit_object_for(res.drops.as_ref().unwrap(), res.typed.as_ref().unwrap(), target).unwrap_or_else(|e| panic!("{name}: {e}"));
        let (format, machine) = header(&bytes);
        let expected = match (target.os, target.arch) {
            (Os::Macos, Arch::X86_64) => ("macho", 0x0100_0007),
            (Os::Macos, Arch::Aarch64) => ("macho", 0x0100_000c),
            (_, Arch::X86_64) => ("elf", 62),
            (_, Arch::Aarch64) => ("elf", 183),
        };
        assert_eq!((format, machine), expected, "{name}");
    }
}

#[test]
fn host_target_matches_default_emission_format() {
    let Some(host) = Target::host() else { return };
    let res = tarn_driver::check(std::path::Path::new("../../benchmarks/native/arithmetic.tarn")).unwrap();
    let (p, t) = (res.drops.as_ref().unwrap(), res.typed.as_ref().unwrap());
    let default = tarn_backend::emit_object(p, t).unwrap();
    let explicit = tarn_backend::emit_object_for(p, t, host).unwrap();
    assert_eq!(header(&default), header(&explicit));
}
