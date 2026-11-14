//! Linux arm64 Image header and EL1 bootstrap (ADR 0106).
//! The linker supplies the image size, BSS bounds and reserved stack top.
use crate::{Error, Result};
use cranelift_codegen::binemit::Reloc;
use cranelift_module::{FuncId, Linkage, Module, ModuleReloc, ModuleRelocTarget};
use cranelift_object::ObjectModule;

pub(crate) fn emit(module: &mut ObjectModule, adapter: FuncId) -> Result<FuncId> {
    let bug = |e: cranelift_module::ModuleError| Error::bug(e.to_string());
    let sig = module.make_signature();
    let start = module.declare_function("tarn_arm64_start", Linkage::Export, &sig).map_err(bug)?;
    let mut bytes = vec![0; 64];
    bytes[0..4].copy_from_slice(&0x14000010u32.to_le_bytes()); // b +64
    bytes[8..16].copy_from_slice(&0x80000u64.to_le_bytes()); // text_offset
    bytes[56..60].copy_from_slice(b"ARM\x64");
    // Each instruction is listed next to its encoding; only literal addresses
    // need relocations. No external assembler is needed to build Tarn.
    let words: &[u32] = &[
        0xd5034fdf, // msr daifset, #15
        0xd5384241, // mrs x1, CurrentEL
        0xf100203f, // cmp x1, #8
        0x54000241, // b.ne el1
        0xd2b00001, // mov x1, #0x80000000 (HCR.RW)
        0xd51c1101, // msr hcr_el2, x1
        0xd51c115f, // msr cptr_el2, xzr
        0xd2800061, // mov x1, #3
        0xd51ce101, // msr cnthctl_el2, x1
        0xd51ce07f, // msr cntvoff_el2, xzr
        0xd2800121, // mov x1, #9 (SRE, Enable)
        0xd51cc9a1, // msr icc_sre_el2, x1
        0x10000121, // adr x1, el1
        0xd51c4021, // msr elr_el2, x1
        0xd28078a1, // mov x1, #0x3c5 (EL1h, DAIF masked)
        0xd51c4001, // msr spsr_el2, x1
        0xd2810001, // movz x1, #0x800
        0xf2a61a01, // movk x1, #0x30d0, lsl #16 (RES1; M/C/I off)
        0xd5181001, // msr sctlr_el1, x1
        0xd5033fdf, // isb
        0xd69f03e0, // eret
        0xd50041bf, // el1: msr spsel, #1
        0xd2810001, // movz x1, #0x800
        0xf2a61a01, // movk x1, #0x30d0, lsl #16
        0xd5181001, // msr sctlr_el1, x1
        0xd2a00601, // mov x1, #0x300000
        0xd5181041, // msr cpacr_el1, x1 (FPEN=3)
        0xd5033fdf, // isb
        0x580001c1, // ldr x1, stack
        0x9100003f, // mov sp, x1
        0x580001c1, // ldr x1, bss
        0x580001e2, // ldr x2, end
        0xeb02003f, // zero: cmp x1, x2
        0x54000062, // b.hs done
        0xf800843f, // str xzr, [x1], #8
        0x17fffffd, // b zero
        0x58000181, // done: ldr x1, adapter
        0xd63f0020, // blr x1 (x0 remains the DTB physical address)
        0xd5034fdf, // halt: msr daifset, #15
        0xd503207f, // wfi
        0x17fffffe, // b halt
        0xd503201f, // nop (8-byte literal alignment)
    ];
    bytes.extend(words.iter().flat_map(|w| w.to_le_bytes()));
    bytes.extend([0; 32]);
    let mut relocs = Vec::new();
    for (offset, name) in [(16, "__arm64_image_size"), (232, "__arm64_stack_top"), (240, "__bss_start"), (248, "__bss_end")] {
        let data = module.declare_data(name, Linkage::Import, false, false).map_err(bug)?;
        relocs.push(ModuleReloc { offset, kind: Reloc::Abs8, name: ModuleRelocTarget::from(data), addend: 0 });
    }
    relocs.push(ModuleReloc { offset: 256, kind: Reloc::Abs8, name: ModuleRelocTarget::from(adapter), addend: 0 });
    module.define_function_bytes(start, 8, &bytes, &relocs).map_err(bug)?;
    Ok(start)
}
