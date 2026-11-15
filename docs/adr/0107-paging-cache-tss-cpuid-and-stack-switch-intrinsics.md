# ADR 0107: Paging, cache, TSS, cpuid and stack-switch intrinsics

Status: accepted (Cairn T6).

## Problem

Cairn phase 0 found CPU operations Tarn could not express: everything an
aarch64 kernel needs to enable and maintain its MMU (TLB, barriers, cache,
address translation, ID registers), the x86_64 task register, `cpuid`,
`swapgs` and friends, and no way to run code on another stack because Tarn
cannot write `rsp`/`sp`.

## Decision

All new operations are `unsafe` typed functions in `arch_x86_64` /
`arch_aarch64`, implemented with the P5/T3 mechanism (ADR 0101, 0104):
hand-assembled stubs in `compiler/backend/src/arch.rs`, inlined into the call
slot when the body fits (5 bytes on x86_64, one instruction on aarch64).
The disassembly test checks every stub against host `objdump`.

### aarch64

- TLB: `tlbi_vmalle1`, `tlbi_vmalle1is`, `tlbi_vae1(va, asid)`,
  `tlbi_vae1is`, `tlbi_vale1`, `tlbi_aside1(asid)`. The hardware operand is
  `VA[55:12]` in bits [43:0] and the ASID in [63:48]; callers pass the VA and
  ASID separately and Tarn builds the operand (`tlbi_operand`), so the
  encoding cannot be gotten wrong. By-VA operations are page based; pass any
  address in the page.
- Barriers: `dsb_ish/ishst/nsh/nshst`, `dmb_ish/ishld/ishst`.
- Cache: `ic_iallu`, `ic_ialluis`, `ic_ivau`, `dc_civac/cvac/ivac/zva/cvau`.
- Translation: `at_s1e1r`, `at_s1e1w`, `read_par_el1` (the caller issues `isb`).
- Read-only ID registers: `id_aa64mmfr0_el1`, `id_aa64mmfr1_el1`,
  `id_aa64pfr0_el1`, `ctr_el0`, `dczid_el0`, `midr_el1`, `mpidr_el1`;
  `read_sp_el0` / `write_sp_el0` (a new `SYSREGS` entry).
- `ExceptionFrame`: `frame_sp` reads the saved interrupted stack pointer
  (slot 33; informational, not restored). `frame_x` / `set_frame_x` now panic
  for an index above 30 instead of silently reading ELR/SPSR/SP.

Every instruction is a single word and is inlined. Encodings and their
assembly are in `A64_SYS` and `SYSREGS`.

### x86_64

`ltr(selector u16)`, `str() u16`, `cpuid(leaf, subleaf) CpuidResult`,
`ud2`, `xgetbv`, `xsetbv`, `wbinvd`, `clflush(address)`,
`invpcid(kind, descriptor)`, `swapgs`, `rdfsbase/wrfsbase/rdgsbase/wrgsbase`,
`read_rflags`. `CpuidResult` is a struct of four `u32` fields; the stub's
raw ABI is `(leaf, subleaf, out *mut u32)` writing eax, ebx, ecx, edx, with
`rbx` pushed and popped (callee-saved). `read_rflags` is `pushfq; pop rax`
(uses only the stack slot below `rsp`, which no code in Tarn's ABI keeps
live: there is no red zone). `cpuid`, `xgetbv`, `xsetbv` and `call_on_stack`
are longer than the call slot and stay calls; the rest are inlined.
`wrfsbase` etc. need CR4.FSGSBASE, `xgetbv` CR4.OSXSAVE, `invpcid` CPU support.

### `call_on_stack(stack_top u64, entry *u8, arg u64)`

`entry` is `ffi.callback(f)` of an ordinary `fn f(arg u64)`, i.e. a C ABI
function (ADR 0080/0102). It cannot be expressed as straight-line stub code
in the caller because the caller's own frame is addressed through `sp`, so it
is always a real call of a local stub:

```text
x86_64                         aarch64
push rbp                       stp x29, x30, [sp, #-16]!
mov rbp, rsp                   mov x29, sp
mov rsp, rdi                   and x3, x0, #~15
and rsp, -16                   mov sp, x3
mov rdi, rdx                   mov x0, x2
call rsi                       blr x1
mov rsp, rbp                   mov sp, x29
pop rbp                        ldp x29, x30, [sp], #16
ret                            ret
```

- Callee-saved registers: the stub uses only `rbp`/`x29` (saved on the old
  stack and restored) and scratch registers; `entry` is an ordinary C ABI
  function and preserves every other callee-saved register itself. The
  caller's Cranelift code treats the call as an ordinary call, so live values
  are in callee-saved registers or its own frame, which is untouched.
- Alignment: the new `sp` is `stack_top & ~15` at the call; on x86_64 the call
  pushes the return address, so the callee sees the usual `rsp % 16 == 8`;
  on aarch64 `sp` stays 16-aligned.
- Return: the return address is on the new stack (x86_64) or in x30 (aarch64);
  after `entry` returns the old `sp` comes from `rbp`/`x29`, then `ret` uses
  the return address saved on the old stack.
- Nothing is written at or above `stack_top`. `entry` must return normally;
  it must not unwind or switch stacks permanently (that needs a context
  switch, not this primitive). The function stays `unsafe`: the new stack must
  be valid, writable and large enough, and (on x86_64) faults on it need an
  IST entry. Tarn cannot make this safe because stack validity is a hardware
  property.

## Evidence

- `cpu_stubs_disassemble_to_their_documented_assembly` covers every new
  operation on both architectures; `aarch64_maintenance_instructions_replace_their_calls`
  and `oversized_x86_sequences_keep_their_frameless_stub_calls` check that the
  short ones are inlined and only the long ones call.
- `examples/kernel-aarch64` enables a 1 GiB-block identity-mapped MMU with
  MAIR/TCR/TTBR0/SCTLR, `tlbi`, `dsb`, `isb` and `ic iallu`, prints a line, then
  verifies with `at s1e1r`, maps and unmaps a block with `tlbi vae1is`, runs
  `dc zva` and `call_on_stack`, and continues running the heap and timer
  tests with the MMU and caches on.
- `examples/kernel-x86_64` loads a GDT with a TSS using `ltr`, checks `str`,
  prints the `cpuid` vendor, runs `call_on_stack` twice and finally forces a
  double fault by calling on an unmapped stack; the #DF gate uses IST1 and
  the handler verifies it runs on the IST stack.

## Limits

`swapgs`, `wrfsbase` & co., `invpcid`, `xsetbv`, `wbinvd` and `clflush` are
only checked by disassembly (QEMU TCG runs `ltr`, `cpuid`, `str`,
`call_on_stack` and the aarch64 cache and TLB operations). No SVE/PAC
registers or other `tlbi` forms (`vaae1`, `rvae1`) are provided yet.
