# ADR 0111 — Atomic x86 idle enable and wait

Status: accepted (Cairn phase 1E).

## Evidence

Separate `arch_x86_64.sti()` and `hlt()` calls lower through ADR 0104 to
`FB 90 90 90 90 F4`: the STI shadow ends at the first NOP. An IRQ can be
handled before HLT and its wakeup lost. Cairn requires race-free idle and
cannot repair instruction adjacency in kernel code.

## Decision

Add unsafe `arch_x86_64.sti_hlt()`, a single template `FB F4 C3`. Normal
inline substitution copies `FB F4` into one call slot and pads after HLT;
the fallback call stub also retains adjacency. The caller must be ring 0,
mask IRQs and check work before entry. Return has IF set. No scheduler,
queue or ownership policy enters the compiler.

ARM already supports the required mask/check/WFI/unmask sequence: a pending
IRQ wakes WFI even while DAIF.I masks exception delivery. No ARM helper is
needed. Physical power savings and deep sleep remain kernel/platform policy.

## Validation

The minimal freestanding reproducer becomes a passing code-generation test
on ELF and Mach-O x86 targets. It checks the inlined FB F4 bytes and immediate
STI/HLT adjacency in caller disassembly; the full template-disassembly test
checks the fallback stub. Existing CPU/boot tests cover both architectures.
