# Native benchmarks

Run `python3 benchmarks/native/run.py` to use the installed `tarn` from PATH
(override with `TARN=/absolute/path/to/tarn`). This checks each fixture's output
over five executions and reports median runtime, ELF size and total build time.
Build time includes the frontend, code generation, runtime compilation and link;
it cannot be compared directly with the isolated codegen/link columns below.
It does not rebuild the compiler. Timings include process startup and printing.

`volatile-store.tarn` performs ten million volatile u64 stores to a stack
variable and checks the last value. It measures access overhead on RAM, not
MMIO device latency. On x86_64, `cpu-pause.tarn` performs ten million
unprivileged `pause` operations as a proxy for intrinsic call overhead; the
runner omits it on other hosts. Expected outputs are checked like the other
fixtures. ADR 0104 records the before/after method, disassembly and limits;
[raw observations](intrinsics-2026-11-14.json) include every timing sample.

## Historical native baseline (2026-10-05)

Linux x86_64 host, debug Rust compiler and Cranelift default code generation;
C runtime compiled with cc -O0, no optimization changes. Dependency downloads
were cached. Clean means a fresh Cargo target directory, not a cold filesystem
cache. No isolated CPU or load control: numbers are observations, not performance
promises or comparative optimization evidence.

Clean compiler build: **51.10 seconds**, peak RSS **1,400,284 KiB**. Command:

```
/usr/bin/time -f 'clean_compile_seconds=%e peak_rss_kib=%M' cargo build -p tarn --target-dir /tmp/tarn-clean-native-baseline
```

`cargo test -p tarn_backend --test baseline -- --ignored --nocapture` measures
object emission from already-checked post-drop IR (including specialization),
cc runtime compilation plus linking, ELF size and median of five child executions.
Runtime includes process startup and one print; no frontend time is included in
codegen. The clean build ran concurrently with baseline collection, so host
contention may affect these measurements. The baseline test is deliberately
ignored in normal CI and asserts results without timing thresholds.

| Fixture | Codegen ms | Runtime compile + link ms | Object bytes | ELF bytes | Runtime median ms |
|---|---:|---:|---:|---:|---:|
| arithmetic (1M additions) | 6.274 | 86.719 | 1432 | 16976 | 2.642 |
| calls (1M generic calls) | 8.047 | 92.453 | 1632 | 17048 | 4.558 |
| slices (100K 4-element sums) | 29.735 | 90.203 | 3864 | 17048 | 4.943 |

Expected outputs are checked during measurement. Repeat on an idle, controlled
host before drawing conclusions. These baselines do not authorize optimization.

## Phase 9 dynamic dispatch baseline

The collector now also executes `dynamic.tarn`: one million shared-receiver
interface calls through a function parameter, producing 42,000,000. Observation
on the same Linux debug-toolchain host, with other compiler tests running:

| Fixture | Codegen ms | Runtime compile + link ms | Object bytes | ELF bytes | Runtime median ms |
|---|---:|---:|---:|---:|---:|
| dynamic (1M interface calls) | 48.290 | 151.875 | 2936 | 17232 | 35.060 |

Five executions; median includes startup and one print. This is a baseline for
the full existing pair-copy/reborrow/indirect-call path, not an isolated instruction
latency or a controlled comparison with static dispatch. No devirtualization,
inline cache or optimization was used. Command remains the ignored baseline test
above. Repeat on an idle host before attributing cost to a specific operation.
