//! PVH boot code (ADR 0101, P8). QEMU's `-kernel`, Xen and Cloud Hypervisor
//! start an ELF that carries a `XEN_ELFNOTE_PHYS32_ENTRY` note in 32-bit
//! protected mode, paging off, with `ebx` = address of an `hvm_start_info`.
//! Tarn emits only 64-bit code, so a function marked `#[pvh_entry]` gets a
//! hand-assembled 32-bit stub (generated here, disassembled by a test) that
//! reaches long mode and calls the function, plus the data it needs and the
//! note that names the stub. Nothing here is a blob: every byte is listed in
//! `listing()` next to its assembly.
//!
//! 32-bit part (`tarn_pvh_start`, Intel syntax; `STACK`, `TABLES`, `GDT` are
//! symbols the linker resolves with 32-bit absolute relocations):
//!
//! ```text
//! cli ; cld
//! mov esp, STACK + 16384            16 KiB boot stack, `.bss`
//! mov esi, ebx                      keep the hvm_start_info pointer
//! mov edi, TABLES ; xor eax, eax ; mov ecx, 3072 ; rep stosd     zero 3 pages
//! mov eax, TABLES
//! lea edx, [eax + 0x1003] ; mov [eax], edx                        PML4[0] -> PDPT
//! lea edx, [eax + 0x2003] ; mov [eax + 0x1000], edx               PDPT[0] -> PD
//! lea edi, [eax + 0x2000] ; xor ecx, ecx
//! loop: mov edx, ecx ; shl edx, 21 ; or edx, 0x83                  PD[i] = i * 2 MiB, present+write+PS
//!       mov [edi + ecx*8], edx ; inc ecx ; cmp ecx, 512 ; jb loop
//! mov eax, cr4 ; or eax, 0x620 ; mov cr4, eax                    PAE, OSFXSR, OSXMMEXCPT
//! mov eax, TABLES ; mov cr3, eax
//! mov ecx, 0xc0000080 ; rdmsr ; or eax, 0x100 ; wrmsr             EFER.LME
//! push GDT ; push word 23 ; lgdt [esp] ; add esp, 6
//! mov eax, cr0 ; and eax, 0xfffffff3 ; or eax, 0x80000023 ; mov cr0, eax    PG, NE, MP, PE; EM, TS clear
//! jmp 0x08:long_mode
//! ```
//!
//! 64-bit part: segments from the GDT (selector 0x08 = code, 0x10 = data),
//! stack, `edi` = the saved info pointer, call the Tarn entry, halt if it
//! returns.
//!
//! ```text
//! long_mode: mov ax, 0x10 ; mov ds, ax ; mov es, ax ; mov ss, ax ; xor eax, eax ; mov fs, ax ; mov gs, ax
//! mov esp, STACK + 16384 ; mov edi, esi ; xor ebp, ebp
//! call <C adapter of the entry>
//! cli ; hlt ; jmp -3
//! ```
//!
//! Identity map: the first 1 GiB, 2 MiB pages, write-back. The GDT is
//! `null, code64 (0x00af9b000000ffff), data (0x00cf93000000ffff)`.

use crate::{Error, Result};
use cranelift_codegen::binemit::Reloc;
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module, ModuleReloc, ModuleRelocTarget};
use cranelift_object::ObjectModule;

pub const STACK_SIZE: u32 = 16384;
const TABLES_SIZE: usize = 3 * 4096;
const GDT: [u64; 3] = [0, 0x00af_9b00_0000_ffff, 0x00cf_9300_0000_ffff];

/// What a 32-bit absolute relocation in the stub refers to.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Symbol {
    Stack,
    Tables,
    Gdt,
    /// The stub itself (the 64-bit part).
    Stub,
}

pub(crate) struct Stub {
    pub code: Vec<u8>,
    pub asm: Vec<String>,
    /// `(offset, symbol, addend)` of each 32-bit absolute field.
    pub abs4: Vec<(usize, Symbol, i64)>,
    /// Offset of the `call` operand, a PC-relative field.
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

pub(crate) fn stub() -> Stub {
    let mut b = Builder::default();
    b.op(&[0xfa], "cli");
    b.op(&[0xfc], "cld");
    b.with_symbol(&[0xbc], Symbol::Stack, i64::from(STACK_SIZE), "mov esp,");
    b.op(&[0x89, 0xde], "mov esi,ebx");
    b.with_symbol(&[0xbf], Symbol::Tables, 0, "mov edi,");
    b.op(&[0x31, 0xc0], "xor eax,eax");
    b.op(&[0xb9, 0x00, 0x0c, 0x00, 0x00], "mov ecx,0xc00");
    b.op(&[0xf3, 0xab], "rep stos DWORD PTR es:[edi],eax");
    b.with_symbol(&[0xb8], Symbol::Tables, 0, "mov eax,");
    b.op(&[0x8d, 0x90, 0x03, 0x10, 0x00, 0x00], "lea edx,[eax+0x1003]");
    b.op(&[0x89, 0x10], "mov DWORD PTR [eax],edx");
    b.op(&[0x8d, 0x90, 0x03, 0x20, 0x00, 0x00], "lea edx,[eax+0x2003]");
    b.op(&[0x89, 0x90, 0x00, 0x10, 0x00, 0x00], "mov DWORD PTR [eax+0x1000],edx");
    b.op(&[0x8d, 0xb8, 0x00, 0x20, 0x00, 0x00], "lea edi,[eax+0x2000]");
    b.op(&[0x31, 0xc9], "xor ecx,ecx");
    b.op(&[0x89, 0xca], "mov edx,ecx");
    b.op(&[0xc1, 0xe2, 0x15], "shl edx,0x15");
    b.op(&[0x81, 0xca, 0x83, 0x00, 0x00, 0x00], "or edx,0x83");
    b.op(&[0x89, 0x14, 0xcf], "mov DWORD PTR [edi+ecx*8],edx");
    b.op(&[0x41], "inc ecx");
    b.op(&[0x81, 0xf9, 0x00, 0x02, 0x00, 0x00], "cmp ecx,0x200");
    b.op(&[0x72, 0xe9], "jb");
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
    b.code.push(0xe8);
    let call_at = b.code.len();
    b.code.extend([0; 4]);
    b.asm.push("call".into());
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
    stub().asm
}

/// Emit the stub, its tables, stack, GDT and the PVH note. `adapter` is the
/// C-ABI adapter of the `#[pvh_entry]` function.
pub(crate) fn emit(module: &mut ObjectModule, adapter: FuncId) -> Result<()> {
    let bug = |e: cranelift_module::ModuleError| Error::bug(e.to_string());
    let bss = |module: &mut ObjectModule, name: &str, size: usize| -> Result<DataId> {
        let id = module.declare_data(name, Linkage::Local, true, false).map_err(bug)?;
        let mut d = DataDescription::new();
        d.define_zeroinit(size);
        d.set_align(4096);
        module.define_data(id, &d).map_err(bug)?;
        Ok(id)
    };
    let stack = bss(module, "tarn_pvh_stack", STACK_SIZE as usize)?;
    let tables = bss(module, "tarn_pvh_tables", TABLES_SIZE)?;
    let gdt = module.declare_data("tarn_pvh_gdt", Linkage::Local, false, false).map_err(bug)?;
    let mut d = DataDescription::new();
    d.define(GDT.iter().flat_map(|g| g.to_le_bytes()).collect::<Vec<u8>>().into_boxed_slice());
    d.set_align(8);
    module.define_data(gdt, &d).map_err(bug)?;

    let sig = module.make_signature();
    let start = module.declare_function("tarn_pvh_start", Linkage::Export, &sig).map_err(bug)?;
    let stub = stub();
    let target = |symbol: Symbol| match symbol {
        Symbol::Stack => ModuleRelocTarget::from(stack),
        Symbol::Tables => ModuleRelocTarget::from(tables),
        Symbol::Gdt => ModuleRelocTarget::from(gdt),
        Symbol::Stub => ModuleRelocTarget::from(start),
    };
    let mut relocs: Vec<ModuleReloc> = stub.abs4.iter().map(|(offset, symbol, addend)| ModuleReloc { offset: *offset as u32, kind: Reloc::Abs4, name: target(*symbol), addend: *addend }).collect();
    relocs.push(ModuleReloc { offset: stub.call_at as u32, kind: Reloc::X86CallPCRel4, name: ModuleRelocTarget::from(adapter), addend: -4 });
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_covers_every_instruction_and_jumps_land_on_instruction_starts() {
        let s = stub();
        assert_eq!(s.asm.len(), listing().len());
        // `jb` back to the first instruction of the page directory loop (`mov edx, ecx`).
        let jb = s.code.windows(2).position(|w| w == [0x72, 0xe9]).expect("loop branch");
        let target = (jb as i64 + 2 - 23) as usize;
        assert_eq!(&s.code[target..target + 2], &[0x89, 0xca], "the loop starts at `mov edx, ecx`");
        // The far jump's offset field targets the 64-bit part: `mov ax, 0x10` right after the 7-byte `ljmp`.
        let (at, symbol, addend) = s.abs4.iter().copied().find(|(_, s, _)| *s == Symbol::Stub).expect("far jump relocation");
        assert_eq!(symbol, Symbol::Stub);
        assert_eq!(&s.code[at - 1..at], &[0xea]);
        assert_eq!(addend as usize, at + 4 + 2);
        assert_eq!(&s.code[addend as usize..addend as usize + 4], &[0x66, 0xb8, 0x10, 0x00]);
        // The call is the last instruction with an operand before `cli; hlt; jmp -3`.
        assert_eq!(s.code[s.call_at - 1], 0xe8);
        assert_eq!(&s.code[s.call_at + 4..], &[0xfa, 0xf4, 0xeb, 0xfd]);
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
