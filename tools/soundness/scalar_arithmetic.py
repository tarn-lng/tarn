# Differential check of native checked arithmetic / bounds against a Python oracle
# (docs/soundness-audit.md). Usage: python3 tools/soundness/scalar_arithmetic.py SEED COUNT
# from the repository root, after `cargo build -p tarn`; TARN selects the binary.
import random, subprocess, sys, os
random.seed(int(sys.argv[1])); N = int(sys.argv[2])
T = {"u8": (0, 255), "u16": (0, 65535), "u32": (0, 2**32-1), "u64": (0, 2**64-1), "i8": (-128, 127), "i16": (-2**15, 2**15-1), "i32": (-2**31, 2**31-1), "i64": (-2**63, 2**63-1)}
class P(Exception): pass
def tdiv(a, b):
    q = abs(a) // abs(b); return q if (a < 0) == (b < 0) else -q
def pick(lo, hi):
    c = [lo, hi, lo + 1, hi - 1, 0, 1, -1 if lo < 0 else 2, 2, 127, 128, 255, 256, 65535, 65536, 2**31-1, 2**31, 2**32-1, 2**32, 2**63-1, 2**63, -2**31, -2**63, random.randint(lo, hi)]
    c = [x for x in c if lo <= x <= hi]
    return random.choice(c)
def lit(v): return f"({v})" if v < 0 else str(v)
def gen():
    t = random.choice(list(T)); lo, hi = T[t]
    a, b = pick(lo, hi), pick(lo, hi)
    op = random.choice(["+", "-", "*", "/", "%", "&", "|", "^", "<<", ">>", "cmp", "neg", "cast"])
    form = random.choice(["const", "var", "param"])
    if op == "neg":
        if lo == 0: op = "+"
    if op in ("<<", ">>"): b = random.randint(0, 70)
    exp = None
    try:
        if op == "+": r = a + b
        elif op == "-": r = a - b
        elif op == "*": r = a * b
        elif op in "/%":
            if b == 0: raise P()
            q = tdiv(a, b); r = q if op == "/" else a - b * q
        elif op == "&": r = a & b
        elif op == "|": r = a | b
        elif op == "^": r = a ^ b
        elif op == "<<":
            if b >= int(t[1:]): raise P()
            r = a << b
        elif op == ">>":
            if b >= int(t[1:]): raise P()
            r = a >> b
        elif op == "neg": r = -a
        elif op == "cmp": r = "true" if a < b else "false"
        elif op == "cast":
            tt = random.choice(list(T)); l2, h2 = T[tt]
            if not (l2 <= a <= h2): raise P()
            r = a
        if op not in ("cmp",) and not isinstance(r, str):
            if op == "cast":
                if not (l2 <= r <= h2): raise P()
            elif not (lo <= r <= hi):
                if op in ("<<",): r = None  # shl overflow semantics: unknown; skip
                else: raise P()
        exp = str(r).lower() if r is not None else None
    except P: exp = "PANIC"
    if exp is None: return None
    shift_b = f"u32({b})" if False else str(b)
    if form == "const": pre = f"    x: {t} := {lit(a)}\n    y: {t} := {lit(b)}\n"
    elif form == "var": pre = f"    var x: {t} = {lit(a)}\n    var y: {t} = {lit(b)}\n    x = x\n"
    else: pre = ""
    if form == "param":
        fn = f"fn f(x {t}, y {t}) "
    if op == "neg": e = "-x"
    elif op == "cmp": e = "x < y"
    elif op == "cast": e = f"{tt}(x)"
    else: e = f"x {op} y"
    if form == "param":
        rt = "bool" if op == "cmp" else (tt if op == "cast" else t)
        src = f"fn f(x {t}, y {t}) {rt} {{ return {e} }}\nfn main() {{\n    print(f({lit(a)}, {lit(b)}))\n}}\n"
    else:
        src = f"fn main() {{\n{pre}    print({e})\n}}\n"
    return src, exp
import tempfile
tarn = os.environ.get("TARN", "target/debug/tarn")
WORK = tempfile.mkdtemp(prefix="tarn-soundness-")
bad = 0; n = 0
while n < N:
    g = gen()
    if not g: continue
    src, exp = g; f = f"{WORK}/p{n}.tarn"; open(f, "w").write(src); n += 1
    r = subprocess.run(["nice", "-n", "10", tarn, "run", f], capture_output=True, text=True, timeout=120)
    if "error[" in r.stderr + r.stdout: print("COMPILE", f, (r.stderr+r.stdout)[:200].replace("\n", " ")); continue
    if exp == "PANIC": ok = r.returncode != 0 and "panic:" in r.stderr
    else: ok = r.returncode == 0 and r.stdout.strip() == exp
    if not ok: bad += 1; print("MISMATCH", f, exp, r.returncode, r.stdout.strip()[:40], r.stderr.strip()[:60]); print(src)
print("done", N, "bad", bad)
