//! PVH boot code (ADR 0101, P8). QEMU's `-kernel`, Xen and Cloud Hypervisor
//! start an ELF that carries a `XEN_ELFNOTE_PHYS32_ENTRY` note in 32-bit
//! protected mode, paging off, with `ebx` = address of an `hvm_start_info`.
//! Tarn emits only 64-bit code, so a function marked `#[pvh_entry]` gets a
//! hand-assembled 32-bit stub (generated here, disassembled by a test) that
//! reaches long mode and calls the function, plus the data it needs and the
//! note that names the stub. Nothing here is a blob: every byte is listed in
//! `listing()` next to its assembly.
//!
//! The low stub, descriptor, GDT, tables and 16 KiB stack stay physical.
//! 2 MiB pages identity-map 1..=512 GiB; an optional upper-half 1 GiB alias
//! maps physical [0, 1 GiB). See ADR 0105 for the linker and hand-off contract.

use crate::{Error, Result};
use cranelift_codegen::binemit::Reloc;
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module, ModuleReloc, ModuleRelocTarget};
use cranelift_object::ObjectModule;
use tarn_types::statics::PvhConfig;

pub const STACK_SIZE: u32 = 16384;

fn tables_size(config: PvhConfig) -> usize {
    (2 + config.identity_gib as usize + usize::from(config.higher_half.is_some())) * 4096
}
const GDT: [u64; 3] = [0, 0x00af_9b00_0000_ffff, 0x00cf_9300_0000_ffff];

/// What a 32-bit absolute relocation in the stub refers to.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Symbol {
    Stack,
    Tables,
    Gdt,
    /// The stub itself (the 64-bit part).
    Stub,
    Descriptor,
}

pub(crate) struct Stub {
    pub code: Vec<u8>,
    pub asm: Vec<String>,
    /// `(offset, symbol, addend)` of each 32-bit absolute field.
    pub abs4: Vec<(usize, Symbol, i64)>,
    /// Offset of the absolute 64-bit adapter address.
    pub call_at: usize,
}

#[derive(Default)]
struct Builder {
    code: Vec<u8>,
    asm: Vec<String>,
    abs4: Vec<(usize, Symbol, i64)>,
}

impl Builder {
    fn op(&mut self, bytes: &[u8], asm: &str) {
        self.code.extend(bytes);
        self.asm.push(asm.to_string());
    }
    /// `prefix` bytes, then a 32-bit absolute field for `symbol + addend`.
    fn with_symbol(&mut self, prefix: &[u8], symbol: Symbol, addend: i64, asm: &str) {
        self.code.extend(prefix);
        self.abs4.push((self.code.len(), symbol, addend));
        self.code.extend([0; 4]);
        self.asm.push(asm.to_string());
    }
}

pub(crate) fn stub(config: PvhConfig) -> Stub {
    let mut b = Builder::default();
    b.op(&[0xfa], "cli");
    b.op(&[0xfc], "cld");
    b.with_symbol(&[0xbc], Symbol::Stack, i64::from(STACK_SIZE), "mov esp,");
    b.op(&[0x89, 0xde], "mov esi,ebx");
    b.with_symbol(&[0xbf], Symbol::Tables, 0, "mov edi,");
    b.op(&[0x31, 0xc0], "xor eax,eax");
    let words = (tables_size(config) / 4) as u32;
    b.code.push(0xb9);
    b.code.extend(words.to_le_bytes());
    b.asm.push(format!("mov ecx,0x{words:x}"));
    b.op(&[0xf3, 0xab], "rep stos DWORD PTR es:[edi],eax");
    b.with_symbol(&[0xb8], Symbol::Tables, 0, "mov eax,");
    b.op(&[0x8d, 0x90, 0x03, 0x10, 0x00, 0x00], "lea edx,[eax+0x1003]");
    b.op(&[0x89, 0x10], "mov DWORD PTR [eax],edx");
    // Every PDPT slot points to its own 2 MiB-page directory. All physical
    // table pointers are below 4 GiB; their upper words were zeroed above.
    for i in 0..config.identity_gib {
        b.with_symbol(&[0xba], Symbol::Tables, i64::from((2 + i) * 4096 + 3), "mov edx,");
        b.code.extend([0x89, 0x90]);
        b.code.extend((4096 + i * 8).to_le_bytes());
        b.asm.push(format!("mov DWORD PTR [eax+0x{:x}],edx", 4096 + i * 8));
    }
    if let Some(base) = config.higher_half {
        let pdpt = (2 + config.identity_gib) * 4096;
        b.with_symbol(&[0xba], Symbol::Tables, i64::from(pdpt + 3), "mov edx,");
        b.code.extend([0x89, 0x90]);
        b.code.extend((((base >> 39) & 511) as u32 * 8).to_le_bytes());
        b.asm.push("mov high PML4 slot,edx".into());
        b.with_symbol(&[0xba], Symbol::Tables, 8195, "mov edx,");
        b.code.extend([0x89, 0x90]);
        b.code.extend((pdpt + ((base >> 30) & 511) as u32 * 8).to_le_bytes());
        b.asm.push("mov high PDPT slot,edx".into());
    }
    b.op(&[0x8d, 0xb8, 0x00, 0x20, 0x00, 0x00], "lea edi,[eax+0x2000]");
    b.op(&[0x31, 0xc9], "xor ecx,ecx");
    let loop_at = b.code.len();
    b.op(&[0x89, 0xca], "mov edx,ecx");
    b.op(&[0xc1, 0xe2, 0x15], "shl edx,0x15");
    b.op(&[0x81, 0xca, 0x83, 0x00, 0x00, 0x00], "or edx,0x83");
    b.op(&[0x89, 0x14, 0xcf], "mov DWORD PTR [edi+ecx*8],edx");
    b.op(&[0x89, 0xcb], "mov ebx,ecx");
    b.op(&[0xc1, 0xeb, 0x0b], "shr ebx,0xb");
    b.op(&[0x89, 0x5c, 0xcf, 0x04], "mov DWORD PTR [edi+ecx*8+0x4],ebx");
    b.op(&[0x41], "inc ecx");
    b.code.extend([0x81, 0xf9]);
    b.code.extend((config.identity_gib * 512).to_le_bytes());
    b.asm.push(format!("cmp ecx,0x{:x}", config.identity_gib * 512));
    let displacement = (loop_at as isize - (b.code.len() + 2) as isize) as i8;
    b.op(&[0x72, displacement as u8], "jb");
    b.op(&[0x0f, 0x20, 0xe0], "mov eax,cr4");
    b.op(&[0x0d, 0x20, 0x06, 0x00, 0x00], "or eax,0x620");
    b.op(&[0x0f, 0x22, 0xe0], "mov cr4,eax");
    b.with_symbol(&[0xb8], Symbol::Tables, 0, "mov eax,");
    b.op(&[0x0f, 0x22, 0xd8], "mov cr3,eax");
    b.op(&[0xb9, 0x80, 0x00, 0x00, 0xc0], "mov ecx,0xc0000080");
    b.op(&[0x0f, 0x32], "rdmsr");
    b.op(&[0x0d, 0x00, 0x01, 0x00, 0x00], "or eax,0x100");
    b.op(&[0x0f, 0x30], "wrmsr");
    b.with_symbol(&[0x68], Symbol::Gdt, 0, "push");
    b.op(&[0x66, 0x6a, 0x17], "pushw 0x17");
    b.op(&[0x0f, 0x01, 0x14, 0x24], "lgdtl [esp]");
    b.op(&[0x83, 0xc4, 0x06], "add esp,0x6");
    b.op(&[0x0f, 0x20, 0xc0], "mov eax,cr0");
    b.op(&[0x25, 0xf3, 0xff, 0xff, 0xff], "and eax,0xfffffff3");
    b.op(&[0x0d, 0x23, 0x00, 0x00, 0x80], "or eax,0x80000023");
    b.op(&[0x0f, 0x22, 0xc0], "mov cr0,eax");
    // ljmp $0x08, $long_mode: the offset is the stub plus where the 64-bit part starts,
    // which is known only after the jump's own 7 bytes.
    let long_mode = b.code.len() as i64 + 7;
    b.with_symbol(&[0xea], Symbol::Stub, long_mode, "ljmp");
    b.code.extend([0x08, 0x00]);
    b.op(&[0x66, 0xb8, 0x10, 0x00], "mov ax,0x10");
    b.op(&[0x8e, 0xd8], "mov ds,eax");
    b.op(&[0x8e, 0xc0], "mov es,eax");
    b.op(&[0x8e, 0xd0], "mov ss,eax");
    b.op(&[0x31, 0xc0], "xor eax,eax");
    b.op(&[0x8e, 0xe0], "mov fs,eax");
    b.op(&[0x8e, 0xe8], "mov gs,eax");
    b.with_symbol(&[0xbc], Symbol::Stack, i64::from(STACK_SIZE), "mov esp,");
    b.op(&[0x89, 0xf7], "mov edi,esi");
    b.op(&[0x31, 0xed], "xor ebp,ebp");
    b.with_symbol(&[0xbe], Symbol::Descriptor, 0, "mov esi,");
    b.code.extend([0x48, 0xb8]);
    let call_at = b.code.len();
    b.code.extend([0; 8]);
    b.asm.push("movabs rax,".into());
    b.op(&[0xff, 0xd0], "call rax");
    b.op(&[0xfa], "cli");
    b.op(&[0xf4], "hlt");
    b.op(&[0xeb, 0xfd], "jmp");
    Stub { code: b.code, asm: b.asm, abs4: b.abs4, call_at }
}

/// The note: `namesz=4, descsz=8, type=18 (XEN_ELFNOTE_PHYS32_ENTRY), "Xen\0"`, then the entry.
fn note() -> Vec<u8> {
    let mut n = Vec::new();
    for word in [4u32, 8, 18] {
        n.extend(word.to_le_bytes());
    }
    n.extend(b"Xen\0");
    n.extend([0u8; 8]);
    n
}

/// Assembly of the stub, for the disassembly test.
#[doc(hidden)]
pub fn listing() -> Vec<String> {
    stub(PvhConfig::default()).asm
}

/// Emit the stub, its tables, stack, GDT and the PVH note. `adapter` is the
/// C-ABI adapter of the `#[pvh_entry]` function.
pub(crate) fn emit(module: &mut ObjectModule, adapter: FuncId, config: PvhConfig) -> Result<FuncId> {
    let bug = |e: cranelift_module::ModuleError| Error::bug(e.to_string());
    let bss = |module: &mut ObjectModule, name: &str, size: usize| -> Result<DataId> {
        let id = module.declare_data(name, Linkage::Local, true, false).map_err(bug)?;
        let mut d = DataDescription::new();
        d.define_zeroinit(size);
        d.set_align(4096);
        // `used` gives zero-init data its own real SHT_NOBITS section.
        // Custom sections in Cranelift 0.125.3 are always PROGBITS.
        d.set_used(true);
        module.define_data(id, &d).map_err(bug)?;
        Ok(id)
    };
    let stack = bss(module, "tarn_pvh_stack", STACK_SIZE as usize)?;
    let tables = bss(module, "tarn_pvh_tables", tables_size(config))?;
    let gdt = module.declare_data("tarn_pvh_gdt", Linkage::Local, false, false).map_err(bug)?;
    let mut d = DataDescription::new();
    d.define(GDT.iter().flat_map(|g| g.to_le_bytes()).collect::<Vec<u8>>().into_boxed_slice());
    d.set_align(8);
    d.set_segment_section("", ".rodata.pvh");
    module.define_data(gdt, &d).map_err(bug)?;

    let sig = module.make_signature();
    let start = module.declare_function("tarn_pvh_start", Linkage::Export, &sig).map_err(bug)?;
    let stub = stub(config);
    let descriptor = module.declare_data("tarn_pvh_boot_info", Linkage::Local, false, false).map_err(bug)?;
    // ABI v1: twelve little-endian u64 words, all pointers are physical.
    let words = [
        1, // version
        0, tables_size(config) as u64, // table address relocation, bytes
        0, u64::from(STACK_SIZE), // stack address relocation, bytes
        0, stub.code.len() as u64, // stub address relocation, bytes
        0, 24, // GDT address relocation, bytes
        96, // descriptor bytes
        u64::from(config.identity_gib) << 30,
        config.higher_half.unwrap_or(0),
    ];
    let mut d = DataDescription::new();
    d.define(words.into_iter().flat_map(u64::to_le_bytes).collect::<Vec<_>>().into_boxed_slice());
    for (offset, id) in [(8, tables), (24, stack), (56, gdt)] {
        let symbol = module.declare_data_in_data(id, &mut d);
        d.write_data_addr(offset, symbol, 0);
    }
    let entry = module.declare_func_in_data(start, &mut d);
    d.write_function_addr(40, entry);
    d.set_align(8);
    d.set_segment_section("", ".rodata.pvh");
    module.define_data(descriptor, &d).map_err(bug)?;
    let target = |symbol: Symbol| match symbol {
        Symbol::Stack => ModuleRelocTarget::from(stack),
        Symbol::Tables => ModuleRelocTarget::from(tables),
        Symbol::Gdt => ModuleRelocTarget::from(gdt),
        Symbol::Stub => ModuleRelocTarget::from(start),
        Symbol::Descriptor => ModuleRelocTarget::from(descriptor),
    };
    let mut relocs: Vec<ModuleReloc> = stub.abs4.iter().map(|(offset, symbol, addend)| ModuleReloc { offset: *offset as u32, kind: Reloc::Abs4, name: target(*symbol), addend: *addend }).collect();
    relocs.push(ModuleReloc { offset: stub.call_at as u32, kind: Reloc::Abs8, name: ModuleRelocTarget::from(adapter), addend: 0 });
    module.define_function_bytes(start, 16, &stub.code, &relocs).map_err(bug)?;

    let note_id = module.declare_data("tarn_pvh_note", Linkage::Local, false, false).map_err(bug)?;
    let mut d = DataDescription::new();
    d.define(note().into_boxed_slice());
    let function = module.declare_func_in_data(start, &mut d);
    d.write_function_addr(16, function);
    d.set_align(4);
    d.set_segment_section("", ".note.Xen");
    d.set_used(true);
    module.define_data(note_id, &d).map_err(bug)?;
    Ok(start)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_covers_every_instruction_and_jumps_land_on_instruction_starts() {
        let s = stub(PvhConfig::default());
        assert_eq!(s.asm.len(), listing().len());
        let jb = s.code.iter().position(|&b| b == 0x72).expect("loop branch");
        let target = (jb as isize + 2 + s.code[jb + 1] as i8 as isize) as usize;
        assert_eq!(&s.code[target..target + 2], &[0x89, 0xca]);
        // The far jump's offset field targets the 64-bit part: `mov ax, 0x10` right after the 7-byte `ljmp`.
        let (at, symbol, addend) = s.abs4.iter().copied().find(|(_, s, _)| *s == Symbol::Stub).expect("far jump relocation");
        assert_eq!(symbol, Symbol::Stub);
        assert_eq!(&s.code[at - 1..at], &[0xea]);
        assert_eq!(addend as usize, at + 4 + 2);
        assert_eq!(&s.code[addend as usize..addend as usize + 4], &[0x66, 0xb8, 0x10, 0x00]);
        // The call is the last instruction with an operand before `cli; hlt; jmp -3`.
        assert_eq!(&s.code[s.call_at - 2..s.call_at], &[0x48, 0xb8]);
        assert_eq!(&s.code[s.call_at + 8..], &[0xff, 0xd0, 0xfa, 0xf4, 0xeb, 0xfd]);
    }

    #[test]
    fn configured_stubs_bound_tables_and_encode_both_upper_half_indices() {
        for gib in [1, 4, 5, 512] {
            for higher_half in [None, Some(0xffff800000000000), Some(0xffffffff80000000)] {
                let config = PvhConfig { identity_gib: gib, higher_half };
                let s = stub(config);
                // Zero the complete allocation, including the optional high PDPT.
                let words = (tables_size(config) / 4) as u32;
                let zero_count = [vec![0xb9], words.to_le_bytes().to_vec(), vec![0xf3, 0xab]].concat();
                assert!(s.code.windows(zero_count.len()).any(|w| w == zero_count));
                let count = [vec![0x81, 0xf9], (gib * 512).to_le_bytes().to_vec(), vec![0x72]].concat();
                let at = s.code.windows(count.len()).position(|w| w == count).unwrap() + 6;
                let target = (at as isize + 2 + s.code[at + 1] as i8 as isize) as usize;
                assert_eq!(&s.code[target..target + 2], &[0x89, 0xca]);
                // The high physical word is written too (needed above 4 GiB).
                assert!(s.code.windows(9).any(|w| w == [0x89, 0xcb, 0xc1, 0xeb, 0x0b, 0x89, 0x5c, 0xcf, 0x04]));
                let (_, _, far) = s.abs4.iter().find(|(_, symbol, _)| *symbol == Symbol::Stub).unwrap();
                assert_eq!(&s.code[*far as usize..*far as usize + 4], &[0x66, 0xb8, 0x10, 0x00]);
                if let Some(base) = higher_half {
                    let pdpt = (2 + gib) * 4096;
                    assert!(s.abs4.iter().any(|(_, symbol, addend)| *symbol == Symbol::Tables && *addend == i64::from(pdpt + 3)));
                    for offset in [((base >> 39) & 511) as u32 * 8, pdpt + ((base >> 30) & 511) as u32 * 8] {
                        let store = [vec![0x89, 0x90], offset.to_le_bytes().to_vec()].concat();
                        assert!(s.code.windows(store.len()).any(|w| w == store));
                    }
                }
                for (offset, _, _) in &s.abs4 { assert!(*offset + 4 <= s.code.len()); }
            }
        }
    }

    #[test]
    fn the_note_is_a_xen_phys32_entry_note() {
        let n = note();
        assert_eq!(u32::from_le_bytes(n[0..4].try_into().unwrap()), 4);
        assert_eq!(u32::from_le_bytes(n[8..12].try_into().unwrap()), 18);
        assert_eq!(&n[12..16], b"Xen\0");
        assert_eq!(n.len() % 4, 0);
    }
}
