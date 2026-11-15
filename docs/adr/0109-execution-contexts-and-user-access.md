# ADR 0109: Execution contexts, exception stacks and user access

Status: accepted; systems readiness A1–A3.

## Problem and general value

Stackful fibers, embedded schedulers and kernels need resumable C ABI execution
contexts, one-way task entry and reliable exceptions after a task exhausts its
stack. Protected kernels also need bounded windows for accessing user mappings.
These are architecture capabilities, independent of Cairn. Free-form assembly
and a new scheduler/runtime policy are unnecessary.

## Decision

Extend the typed unsafe stub mechanism of ADRs 0101, 0104 and 0107. Longer
sequences remain real local calls. The public modules expose:

- Both: `switch_stack(stack_top u64, entry *u8, arg u64) never`,
  `context_init(context *mut Context, stack_top u64, entry *u8, arg u64)`,
  `context_switch(from *mut Context, to *Context)`.
- aarch64: `read_sp() u64`, `read_spsel() u64`, `write_spsel(value u64)`,
  `switch_stack_el0(stack_top u64, entry *u8, arg u64) never`,
  `pan_enable()`, `pan_disable()`, `read_pan() u64`,
  `set_frame_sp(frame *mut ExceptionFrame, sp u64)`.
- x86_64: `stac()`, `clac()`.

All operations except the context type require `unsafe`. Entries are ordinary
C callbacks obtained with `ffi.callback(fn(arg u64))`; they must never return.
One-way entry aligns the stack down to 16, calls the entry with its argument
and traps if it returns. It never accesses the abandoned stack again. New
contexts also require nonreturning entries; x86 initializes a null return
sentinel, while aarch64 initializes x30 to the entry address (not a valid
return continuation). No automatic task cleanup or unwinding is supplied.

### Context ABI

Contexts have ordinary C layout; fields are private and initialization is via
`context_init`, which clears saved registers and sets stack, entry and argument.
They are deliberately non-Copy: copying a resumable stack capability is unsafe.
Zero-filled context storage is suitable as a save destination, not a runnable
context. Stacks and context storage must remain mapped, live and exclusive.

| Architecture | Ordered fields and byte offsets | Size |
|---|---|---|
| x86_64 System V | rbx 0, rbp 8, r12–r15 16–40, rsp 48, rip 56, arg 64 | 72 |
| aarch64 AAPCS64 | x19–x30 0–88, sp 96, d8–d15 104–160, arg 168 | 176 |

x86 saves the return address at `[rsp]` as RIP and `rsp+8` as the continuation
SP, restores the destination and jumps to RIP. New contexts enter a C callback
with `rsp % 16 == 8`. aarch64 saves x30 and SP, restores them and returns to
x30; SP remains 16 aligned. AAPCS64 also requires the low 64 bits of v8–v15,
so these are saved even though the initial request named only general registers.
Argument registers are caller-saved and may be overwritten on every resumption.

This is a C call boundary, not a whole-machine snapshot: page tables, TLS,
interrupt masks, FP control/environment and extended vector state belong to
the embedding scheduler. No context may execute simultaneously on two CPUs.
A fiber system needs a compatible policy for signals, TLS and FP environment.
Contexts resume at the same privilege/exception level and active stack bank;
`context_switch` does not change aarch64 SPSel. Context switching through an
intrinsic call lets Cranelift preserve live caller
values using its normal ABI; it does not require recognizing scheduler semantics.

### Preemption and trampoline contract

The examples use task-local exception stacks (SP_EL1 on aarch64, ordinary task
RSP on x86). A timer handler acknowledges/rearms the interrupt **before**
calling `context_switch`. The current handler, adapter and trampoline remain
suspended on that task's own stack. Resuming it later returns through the
handler and trampoline's `eret`/`iretq`. The trampoline preserves all interrupted
caller-saved GPRs and SIMD registers; its ordinary C adapter preserves the
callee-saved state, which the context primitive saves while suspended. aarch64
also saves ELR/SPSR in each trampoline frame, so later interrupts cannot destroy
an earlier continuation. Interrupts stay masked while scheduler state changes;
new task entries explicitly unmask them. The examples use integer tasks and do
not promise switching distinct FP control environments.

A **shared** aarch64 SP_EL1 exception stack must not hold multiple suspended
handlers: use task-local exception stacks or copy the entire saved exception
state to task storage and restore a selected frame on return instead. The
existing x86 InterruptFrame exposes only the hardware frame, not the saved GPRs;
editing RIP/RSP alone is not a full preemptive register switch. This ADR does
not claim otherwise or introduce a misleading partial-frame scheduler API.

### aarch64 stack isolation

Run normal EL1 task code with SPSel=0 and a guarded SP_EL0 stack. Before entry,
install vector slots 0/1 (current EL using SP_EL0), enable FP/SIMD and keep a
mapped, sufficiently large SP_EL1 exception stack. `switch_stack_el0` performs
selection and entry atomically with respect to generated stack accesses; IRQs
must be masked. Hardware switches to SP_EL1 before the first vector instruction.
No wrapper ever returns across a changed stack selection.

`write_spsel` accepts only 0/1 as an unsafe precondition. Changing selection in
ordinary Tarn code is valid only if both banks identify the same live frame;
changing it and then hoping to return to the old caller is invalid. Prefer the
one-way helper when the stacks differ. `read_sp` samples the current generated
frame, not its caller's stack top. SP_EL0 register reads/writes require SPSel=1.

The aarch64 trampoline now inspects saved SPSR.M[0]: an EL1h exception saves its
pre-allocation SP; EL1t/EL0t saves SP_EL0. Return restores SP_EL0 for t-mode
frames, including changes through `set_frame_sp`. EL1h retains its original
exception stack; changing its informational SP slot does not relocate the frame.
This corrects ADR 0107's informational-only SP for t-mode exceptions.

### User-memory access windows

On aarch64, check ID_AA64MMFR1_EL1.PAN before using PAN instructions. `read_pan`
returns the architectural bit in position 22. Set PAN with `pan_enable`, clear
it only around a validated copy with `pan_disable`, then restore protection.
Configure SCTLR_EL1.SPAN and exception entry policy deliberately. On x86 check
CPUID.(7,0).EBX[20], enable CR4.SMAP[21], use `stac` to set RFLAGS.AC and `clac`
to clear it. These instructions require ring 0 and CPU feature support.

Validate user ranges, permissions and wraparound before opening a window.
Mask interrupts for the short copy, or ensure exception entry closes access and
return restores the interrupted access state. In particular x86 hardware does
not clear AC on interrupt entry, and the generic trampoline deliberately does
not execute unsupported `clac` on baseline CPUs. Fault recovery must close the
window too. PAN/SMAP are neither pointer validation nor speculation barriers.

## Alternatives and limits

A frame-only scheduler would need an expanded x86 frame API and selection of a
complete return frame on both architectures; suspending ordinary C continuations
already solves stackful scheduling with less new machinery. A shared exception
stack saves memory but requires explicit frame ownership and migration. A
compiler-special scheduler ABI would duplicate the existing C ABI contract.

The unsafe caller still owns stack lifetime, synchronization, guard mappings,
address spaces and extended CPU state. Hardware stack limits and automatic
preemptive runtime integration are outside this change. Evidence of consumers
needing shared exception stacks or distinct FP environments would justify a
separate complete machine-state API, not silently extending Context layout.

## Evidence

Compile-only fixtures emit every new stub on both targets. The exhaustive
objdump test compares every instruction, including register save/restore offsets.
QEMU kernels show six alternating cooperative lines, successful one-way entry,
then six timer-driven alternations between tasks that never yield. `-cpu max`
exercises PAN and SMAP transitions; baseline aarch64 cortex-a72 skips PAN.
The aarch64 kernel maps a 16 KiB alias stack above an unmapped guard, recursively
consumes real frames and verifies a data abort in that guard while its handler
runs on the separate SP_EL1 stack; it exits successfully instead of looping.
