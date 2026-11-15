# Systems readiness A1–A3: execution contexts and protected user access

Implemented by ADR 0109. The public signatures and C ABI context layouts are
specified in [arch.md](arch.md) and [the ADR](adr/0109-execution-contexts-and-user-access.md).
No Cairn files were modified.

## QEMU execution evidence

Both kernels print exactly this task sequence (the tests compare all twelve
lines, not merely the presence of two task names):

```text
coop: task 1 turn 0
coop: task 2 turn 0
coop: task 1 turn 1
coop: task 2 turn 1
coop: task 1 turn 2
coop: task 2 turn 2
switch_stack: entered new stack without return
coop: contexts restored
...
preempt: task 1
preempt: task 2
preempt: task 1
preempt: task 2
preempt: task 1
preempt: task 2
preempt: timer switched both busy tasks
```

The preempted tasks contain no yield. Each timer handler rearms/acknowledges
its device before suspending its own C continuation and trampoline on the
task's stack. The final handler resumes the saved root continuation. Both
busy tasks must have executed their bodies before the demo succeeds.

QEMU `-cpu max` also reports:

```text
smap: stac/clac window ok
pan: protected/open/protected window ok
```

aarch64 baseline cortex-a72 correctly skips unsupported PAN, then the final
SP_EL0 demo reports:

```text
sp_el0: exception returned on task stack
stack overflow: recursive task hit guard page
stack overflow: reported on separate SP_EL1 exception stack
```

The demo first returns from an SP_EL0 SVC through the modified trampoline,
then recursively consumes volatile-backed stack frames on a 16 KiB stack
mapped above an unmapped guard. The fault handler verifies ESR data-abort
class, FAR inside the guard, saved SP in the task stack/guard and current
SP/SPSel on the boot exception stack. This also passes after EL2-to-EL1 entry.

## Validation

The focused backend suites comprise 44 target tests, 10 x86 QEMU tests and
6 aarch64 QEMU tests. They cover exhaustive instruction disassembly, C-layout
context sizes, compile-only emission, unsafe-call rejection, task argument and
stack placement, continuation restoration, preemption and the real overflow.
The broad command is `PATH="$PWD/tools/ci/macos:$PATH" cargo test --no-fail-fast
-p tarn_backend -p tarn -p tarn_types -p tarn_driver`; the final run passed all 361 tests (3 intentionally ignored),
with zero failures and no compiler warnings. No baseline-failure exceptions
were needed. The updated type-dump golden also passes without blessing.

## Exact Cairn follow-up (apply separately)

1. In `kernel/arch/aarch64/mmu.tarn`, replace `enter_high`'s
   `arch_aarch64.call_on_stack(top + layout.KERNEL_BASE, high, arg)` with
   `arch_aarch64.switch_stack(top + layout.KERNEL_BASE, high, arg)`, and declare
   `enter_high(entry *u8, arg u64) never`. This first transfer retains SPSel=1.
   Replace `jump_on_stack` with:

   ```tarn
   pub fn jump_on_stack(entry *u8, stack_top u64) never {
       unsafe { arch_aarch64.switch_stack_el0(stack_top, entry, 0) }
   }
   ```

   Keep IRQs masked through this transfer. The high boot stack becomes the
   SP_EL1 exception stack, so ensure its pages remain mapped after
   `vm.activate()` removes the identity map and budget enough space for
   nested exceptions and logging. The guarded stack from `vm.stack_top(0)`
   becomes SP_EL0; do not write SPSel from an ordinary returning wrapper.

2. `kernel/arch/aarch64/trap.tarn` already installs synchronous slots 0 and 4.
   Retain both, add slots 1/2/3 alongside 5/6/7 for SP_EL0 IRQ/FIQ/SError, and
   route slot 1 to the future timer handler. The existing `frame_sp` report now
   receives the real interrupted SP_EL0. Use `read_sp`/`read_spsel` to assert
   the handler runs on SP_EL1. Keep `faultinfo.describe_address` and stackmap
   guard classification; add `tests/cases/aarch64/stackoverflow.expect` for
   self-test case 16, matching the data-abort/guard classification and failure
   exit instead of accepting a timeout. Update `docs/memory-layout.md` and
   phase-0 reports to remove the old aarch64 limitation after this test passes.

3. In `kernel/arch/x86_64/mmu.tarn`, replace `jump_on_stack`'s call with
   `arch_x86_64.switch_stack(stack_top, entry, 0)` and give the wrapper return
   type `never`. Update the common `kernel/mm/vm.tarn` `enter(next *u8)` wrapper
   to `never`. Retain existing TSS IST1/IST2 overflow reporting. Audit other
   one-way `call_on_stack` sites (including Limine entry) and replace only
   those whose callbacks intentionally never return.

4. For phase 1, store one arch `Context` per task and a root/idle context.
   Allocate guarded stacks with the existing VM/stackmap code, call
   `context_init(ctx, stack_top, ffi.callback(task_entry), arg)` and schedule
   with `context_switch(current, next)` while IRQs are masked. A task entry
   must finish through scheduler teardown/exit and never return. First port
   the cooperative demo, then the busy-task timer test. On x86 ordinary task
   stacks may retain suspended timer trampolines (do not use a shared IST for
   scheduling IRQs). On aarch64, choose task-local SP_EL1 stacks for suspended
   handlers, or implement full exception-frame copying/selection before
   preempting tasks that share the dedicated SP_EL1 stack. The new primitive
   deliberately does not make shared-stack suspension safe.

5. For phase 2 user copies, gate PAN on
   `((read_id_aa64mmfr1_el1() >> 20) & 15) != 0`, and SMAP on
   `(cpuid(7, 0).ebx & (1 << 20)) != 0`. Set CR4.SMAP on x86 and choose
   SCTLR_EL1.SPAN policy on aarch64. Keep protection enabled outside short,
   validated copy windows: `pan_disable`/copy/`pan_enable`, or
   `stac`/copy/`clac`. Mask IRQs for those windows initially; when adding fault
   recovery, close the window on every exceptional exit. Generic x86 entry
   does not clear AC. Add max-CPU and unsupported-feature boot cases.

These changes remove the existing returning-stack workaround and overflow
limitation. They do not include a new Cairn scheduler, user-pointer validator,
shared-stack preemption engine or extended CPU-state policy in Tarn.
