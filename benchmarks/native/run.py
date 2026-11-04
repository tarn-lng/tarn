#!/usr/bin/env python3
"""Measure complete builds and checked executions with the installed compiler."""
import os
from pathlib import Path
import statistics
import subprocess
import tempfile
import time


def main():
    root = Path(__file__).resolve().parent
    compiler = os.environ.get("TARN", "tarn")
    expected = {
        "arithmetic": b"499999500000\n",
        "calls": b"499999500000\n",
        "slices": b"1000000\n",
        "dynamic": b"42000000\n",
    }
    print("fixture       build_ms    ELF_bytes    runtime_median_ms", flush=True)
    with tempfile.TemporaryDirectory(prefix="tarn-native-bench-") as directory:
        for name, output in expected.items():
            executable = Path(directory) / name
            start = time.perf_counter()
            subprocess.run(
                [compiler, "build", str(root / "fixtures" / f"{name}.tarn"), "-o", str(executable)],
                check=True,
                capture_output=True,
            )
            build_ms = (time.perf_counter() - start) * 1000
            elf_bytes = executable.stat().st_size
            runtimes = []
            for _ in range(5):
                run_start = time.perf_counter()
                result = subprocess.run([str(executable)], capture_output=True, check=True)
                runtimes.append((time.perf_counter() - run_start) * 1000)
                if result.stdout != output:
                    raise RuntimeError(f"Unexpected output for {name}: {result.stdout!r}")
            runtime_median = statistics.median(runtimes)
            print(f"{name:<12} {build_ms:<10.2f} {elf_bytes:<12} {runtime_median:<10.2f}", flush=True)


if __name__ == "__main__":
    main()
