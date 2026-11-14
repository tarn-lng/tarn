#!/usr/bin/env python3
"""Measure complete builds and checked executions with the installed compiler."""
import os
import platform
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
        "volatile-store": b"9999999\n",
    }
    if platform.machine().lower() in ("x86_64", "amd64"):
        expected["cpu-pause"] = b"10000000\n"
    print("fixture       build_ms    ELF_bytes    runtime_median_ms", flush=True)
    with tempfile.TemporaryDirectory(prefix="tarn-native-bench-") as directory:
        for name, output in expected.items():
            executable = Path(directory) / name
            start = time.perf_counter()
            subprocess.run(
                [compiler, "build", str(root / f"{name}.tarn"), "-o", str(executable)],
                check=True, stdout=subprocess.DEVNULL,
            )
            build_ms = (time.perf_counter() - start) * 1000
            samples = []
            for _ in range(5):
                start = time.perf_counter()
                result = subprocess.run([str(executable)], capture_output=True, check=True)
                samples.append((time.perf_counter() - start) * 1000)
                if result.stdout != output:
                    raise RuntimeError(f"{name}: unexpected output {result.stdout!r}")
            print(
                f"{name:12} {build_ms:10.3f} {executable.stat().st_size:12} "
                f"{statistics.median(samples):20.3f}", flush=True,
            )


if __name__ == "__main__":
    main()
