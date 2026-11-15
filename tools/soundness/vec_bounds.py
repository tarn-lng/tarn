# Differential check of native checked arithmetic / bounds against a Python oracle
# (docs/soundness-audit.md). Usage: python3 tools/soundness/vec_bounds.py SEED COUNT
# from the repository root, after `cargo build -p tarn`; TARN selects the binary.
import random, subprocess, sys, os
random.seed(int(sys.argv[1])); N = int(sys.argv[2])
import tempfile
tarn = os.environ.get("TARN", "target/debug/tarn")
WORK = tempfile.mkdtemp(prefix="tarn-soundness-")
bad = 0
for n in range(N):
    v = []; lines = ["fn main() {", "    var v: Vec<u64> = Vec.new()"]; out = []; panic = False
    for k in range(random.randint(3, 14)):
        L = len(v)
        def idx(): return random.choice([0, 0, 1, L - 1 if L else 0, L, L + 1, max(L - 2, 0), random.randint(0, L + 2), 2**64 - 1])
        op = random.choice(["push", "push", "pop", "get", "at", "replace", "swap", "swap_remove", "truncate", "len", "slice_idx", "slice_range", "extend", "get_mut", "take"])
        val = random.randint(0, 99)
        if op == "push": lines.append(f"    v.push({val})"); v.append(val)
        elif op == "pop":
            lines.append("    match v.pop() {\n        Some(x) => print(x)\n        None => print(999)\n    }"); out.append(str(v.pop()) if v else "999")
        elif op in ("get", "at", "get_mut"):
            i = idx(); lines.append(f"    print(*v.{op}({i}))" if op != "at" else f"    print(v.at({i}))")
            if i >= L: panic = True; break
            out.append(str(v[i]))
        elif op == "replace":
            i = idx(); lines.append(f"    print(v.replace({i}, {val}))")
            if i >= L: panic = True; break
            out.append(str(v[i])); v[i] = val
        elif op == "swap":
            a, b = idx(), idx(); lines.append(f"    v.swap({a}, {b})")
            if a >= L or b >= L: panic = True; break
            v[a], v[b] = v[b], v[a]
        elif op == "swap_remove":
            i = idx(); lines.append(f"    print(v.swap_remove({i}))")
            if i >= L: panic = True; break
            out.append(str(v[i])); v[i] = v[-1]; v.pop()
        elif op == "truncate":
            t = random.randint(0, L + 2); lines.append(f"    v.truncate({t})"); del v[t:]
        elif op == "len": lines.append("    print(v.len())"); out.append(str(L))
        elif op == "slice_idx":
            i = idx(); lines.append(f"    s{k} := v.as_slice()\n    print(s{k}[{i}])")
            if i >= L: panic = True; break
            out.append(str(v[i]))
        elif op == "slice_range":
            a, b = idx(), idx(); lines.append(f"    s{k} := &v.as_slice()[{a}..{b}]\n    print(s{k}.len())")
            if a > b or b > L: panic = True; break
            out.append(str(b - a))
        elif op == "extend":
            lines.append(f"    arr{k} := [2]u64{{{val}, {val+1}}}\n    v.extend_from_slice(&arr{k}[0..2])"); v += [val, val + 1]
        elif op == "take":
            i = idx(); lines.append(f"    match v.take({i}) {{\n        Some(x) => print(x)\n        None => print(998)\n    }}")
            if i < L: out.append(str(v[i])); v[i] = v[-1]; v.pop()
            else: out.append("998")
    lines.append("}")
    src = "\n".join(lines) + "\n"; f = f"{WORK}/p{n}.tarn"; open(f, "w").write(src)
    r = subprocess.run(["nice", "-n", "10", tarn, "run", f], capture_output=True, text=True, timeout=120)
    if "error[" in r.stderr + r.stdout: print("COMPILE", f, (r.stderr + r.stdout)[:300].replace("\n", " ")); continue
    got = r.stdout.split()
    ok = got == out and ((r.returncode != 0) == panic)
    if panic: ok = ok and "panic:" in r.stderr
    if not ok: bad += 1; print("MISMATCH", f, "panic" if panic else "ok", r.returncode, "exp", out, "got", got, r.stderr[:80])
print("done", N, "bad", bad)
