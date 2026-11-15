# Differential check of native checked arithmetic / bounds against a Python oracle
# (docs/soundness-audit.md). Usage: python3 tools/soundness/loop_arithmetic.py SEED COUNT
# from the repository root, after `cargo build -p tarn`; TARN selects the binary.
import random, subprocess, sys, os
random.seed(int(sys.argv[1]) if len(sys.argv) > 1 else 1)
N = int(sys.argv[2]) if len(sys.argv) > 2 else 30
TYPES = {"usize": (0, 2**64-1), "u8": (0, 255), "u16": (0, 65535), "u32": (0, 2**32-1), "i32": (-2**31, 2**31-1), "i8": (-128, 127), "u64": (0, 2**64-1), "i64": (-2**63, 2**63-1)}
class Panic(Exception): pass
def gen(idx):
    arr = random.choice([0, 0, 4, 8])
    t = "usize" if arr else random.choice([x for x in TYPES if x != "usize"]); lo, hi = TYPES[t]
    big = hi > 2**20
    lim = min(hi, 300)
    iters = random.randint(1, 12)
    def lit():
        c = random.choice([0, 1, 2, 3, 7, 100, 127, 128, 200, 255, 1000, 65535])
        if big and random.random() < .3: c = random.choice([2**31-1, 2**31, 2**32-1, 2**32, hi // 2, hi - 1])
        return min(c, hi) if c <= hi else 1
    # expression grammar over acc, i
    def expr(d=0):
        r = random.random()
        if d > 1 or r < .3: return random.choice([("var", "acc"), ("var", "i"), ("lit", lit())])
        return (random.choice("+-*/%"), expr(d+1), expr(d+1))
    def show(e):
        if e[0] == "var": return e[1]
        if e[0] == "lit": return str(e[1])
        return f"({show(e[1])} {e[0]} {show(e[2])})"
    def ev(e, env):
        if e[0] == "var": return env[e[1]]
        if e[0] == "lit": return e[1]
        a, b = ev(e[1], env), ev(e[2], env)
        if e[0] == "+": v = a + b
        elif e[0] == "-": v = a - b
        elif e[0] == "*": v = a * b
        else:
            if b == 0: raise Panic()
            if e[0] == "/": v = abs(a) // abs(b) * (1 if (a < 0) == (b < 0) or a == 0 else -1)
            else: v = a - b * (abs(a) // abs(b) * (1 if (a < 0) == (b < 0) or a == 0 else -1))
        if v < lo or v > hi: raise Panic()
        return v
    init = lit()
    body = []
    ngroups = random.randint(1, 3)
    for _ in range(ngroups):
        body.append(("assign", expr()))
    cond = None
    if random.random() < .4: cond = (random.choice(["<", ">", "!="]), lit() % (lim + 1))
    idx = None
    if arr: idx = expr()
    lines = [f"fn main() {{", f"    var acc: {t} = {init}", f"    var i: {t} = 0"]
    if arr: lines.append(f"    var a: [{arr}]{t} = [{arr}]{t}{{" + ", ".join(["0"]*arr) + "}")
    lines.append(f"    for i < {min(iters, hi)} {{")
    out = []
    env = {"acc": init, "i": 0}
    try:
        arrv = [0]*arr
        for it in range(min(iters, hi)):
            env["i"] = it
            for _, e in body:
                lines_e = None
                env["acc"] = ev(e, env)
            if arr:
                k = ev(idx, env)
                if k < 0 or k >= arr: raise Panic()
                arrv[k] = env["acc"]
                out.append(arrv[k])
            out.append(env["acc"])
        status = "ok"
    except Panic:
        status = "panic"
    for _, e in body: lines.append(f"        acc = {show(e)}")
    if arr:
        lines.append(f"        a[{show(idx)}] = acc")
        lines.append(f"        print(a[{show(idx)}])")
    lines.append("        print(acc)")
    lines.append("        i = i + 1")
    lines.append("    }")
    lines.append("}")
    return "\n".join(lines) + "\n", out, status
import tempfile
tarn = os.environ.get("TARN", "target/debug/tarn")
WORK = tempfile.mkdtemp(prefix="tarn-soundness-")
bad = 0
for n in range(N):
    src, out, status = gen(n)
    d = WORK
    f = f"{d}/p{n}.tarn"; open(f, "w").write(src)
    r = subprocess.run(["nice", "-n", "10", tarn, "run", f], capture_output=True, text=True, timeout=120)
    got = r.stdout.split()
    if r.returncode != 0 and "error[" in r.stderr + r.stdout:
        print("COMPILE ERROR", f, (r.stderr + r.stdout)[:300]); continue
    exp = [str(x) for x in out]
    okp = (r.returncode == 0) == (status == "ok")
    if not okp or got != exp:
        bad += 1; print("MISMATCH", f, "status", status, "rc", r.returncode, "exp", exp[:6], "got", got[:6])
print("done", N, "bad", bad)
