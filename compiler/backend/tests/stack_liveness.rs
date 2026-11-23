//! Stack-slot liveness reuse regression tests (ADR 0113).
use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn checked(src: &str, tag: &str) -> (PathBuf, tarn_driver::CheckResult) {
    let dir = std::env::temp_dir().join(format!("tarn-stack-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.tarn");
    std::fs::write(&file, src).unwrap();
    let result = tarn_driver::check(&file).unwrap();
    assert!(
        !result.has_errors(),
        "{}",
        result
            .diagnostics
            .iter()
            .map(|d| d.render(&result.program.sources))
            .collect::<String>()
    );
    (dir, result)
}

fn compile_and_run(src: &str, tag: &str) -> (Output, Vec<u8>) {
    let (dir, result) = checked(src, tag);
    let exe = dir.join("program");
    let drops = result.drops.as_ref().unwrap();
    let typed = result.typed.as_ref().unwrap();
    let obj = tarn_backend::emit_object(drops, typed).unwrap();
    tarn_backend::build(drops, typed, &exe).unwrap();
    let out = Command::new("timeout").arg("30s").arg(&exe).output().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    (out, obj)
}

fn max_x86_frame_bytes(obj: &[u8]) -> u32 {
    // Scan x86_64/Mach-O/ELF machine code for `sub $imm32, %rsp` (`48 81 ec <imm32>`)
    // and `sub $imm8, %rsp` (`48 83 ec <imm8>`).
    let mut best = 0u32;
    for w in obj.windows(7) {
        if w[0] == 0x48 && w[1] == 0x81 && w[2] == 0xec {
            let v = u32::from_le_bytes([w[3], w[4], w[5], w[6]]);
            if v < 0x10_0000 {
                best = best.max(v);
            }
        }
    }
    for w in obj.windows(4) {
        if w[0] == 0x48 && w[1] == 0x83 && w[2] == 0xec {
            best = best.max(u32::from(w[3]));
        }
    }
    best
}

#[test]
fn sequential_scopes_reuse_slots_and_bound_frame() {
    let single = r#"
struct Big {
    a u64
    b u64
    c u64
    d u64
    e u64
    f u64
    g u64
    h u64
}
fn make(x u64) Big {
    return Big{a: x, b: x, c: x, d: x, e: x, f: x, g: x, h: x}
}
fn use_big(b Big) u64 {
    return b.a + b.h
}
fn seq() u64 {
    var acc: u64 = 0
    {
        x1 := make(1)
        acc = acc + use_big(x1)
    }
    return acc
}
fn main() {
    print(seq())
}
"#;
    let eight = r#"
struct Big {
    a u64
    b u64
    c u64
    d u64
    e u64
    f u64
    g u64
    h u64
}
fn make(x u64) Big {
    return Big{a: x, b: x, c: x, d: x, e: x, f: x, g: x, h: x}
}
fn use_big(b Big) u64 {
    return b.a + b.h
}
fn seq() u64 {
    var acc: u64 = 0
    {
        x1 := make(1)
        acc = acc + use_big(x1)
    }
    {
        x2 := make(2)
        acc = acc + use_big(x2)
    }
    {
        x3 := make(3)
        acc = acc + use_big(x3)
    }
    {
        x4 := make(4)
        acc = acc + use_big(x4)
    }
    {
        x5 := make(5)
        acc = acc + use_big(x5)
    }
    {
        x6 := make(6)
        acc = acc + use_big(x6)
    }
    {
        x7 := make(7)
        acc = acc + use_big(x7)
    }
    {
        x8 := make(8)
        acc = acc + use_big(x8)
    }
    return acc
}
fn main() {
    print(seq())
}
"#;
    let (out1, obj1) = compile_and_run(single, "seq1");
    let (out8, obj8) = compile_and_run(eight, "seq8");
    assert_eq!(out1.stdout, b"2\n");
    assert_eq!(out8.stdout, b"72\n");
    let f1 = max_x86_frame_bytes(&obj1);
    let f8 = max_x86_frame_bytes(&obj8);
    // Without slot reuse, 8 sequential scopes grew the frame to ~2064 B.
    // With liveness packing, 8 scopes stay bounded relative to 1 scope.
    if cfg!(target_arch = "x86_64") && f1 > 0 {
        assert!(f8 <= f1 * 2 + 128, "expected bounded frame: single={f1} eight={f8}");
    }
}

#[test]
fn many_sequential_scopes_keep_frame_bounded() {
    let mut src = String::from(r#"
copy struct Big {
    a u64
    b u64
    c u64
    d u64
    e u64
    f u64
    g u64
    h u64
}
fn make(x u64) Big {
    return Big{a: x, b: x, c: x, d: x, e: x, f: x, g: x, h: x}
}
fn sum(x Big) u64 { return x.a + x.h }
fn main() {
    var total: u64 = 0
"#);
    for i in 0..512 {
        src.push_str(&format!("    {{\n        item := make({i})\n        total = total + sum(item)\n    }}\n"));
    }
    src.push_str("    print(total)\n}\n");
    let (out, obj) = compile_and_run(&src, "many-scopes");
    assert_eq!(out.stdout, b"261632\n");
    if cfg!(target_arch = "x86_64") {
        let frame = max_x86_frame_bytes(&obj);
        assert!(frame > 0 && frame < 1024, "512 sequential scopes need a bounded frame, got {frame}");
    }
}

#[test]
fn overlapping_lifetimes_and_long_lived_borrows_do_not_alias() {
    let src = r#"
struct Pair {
    a u64
    b u64
    c u64
    d u64
    e u64
    f u64
    g u64
    h u64
}
fn mk(v u64) Pair {
    return Pair{a: v, b: v + 1, c: v + 2, d: v + 3, e: v + 4, f: v + 5, g: v + 6, h: v + 7}
}
fn sum_ref(p &Pair) u64 {
    return p.a + p.d + p.h
}
fn main() {
    var p1 = mk(10)
    var p2 = mk(100)
    r1 := &p1
    {
        var tmp = mk(999)
        sum_ref(&tmp)
    }
    // Both p1 (via long-lived borrow r1) and p2 (overlapping across tmp) must be intact.
    print(sum_ref(r1))
    print(sum_ref(&p2))
}
"#;
    let (out, _) = compile_and_run(src, "overlap-borrow");
    assert_eq!(out.stdout, b"40\n310\n");
}

#[test]
fn loops_joins_moves_and_vec_drops_remain_sound() {
    let src = r#"
struct Holder {
    tag u64
    items Vec<u64>
}
fn make_holder(n u64, flag bool) Holder {
    var v: Vec<u64> = Vec.new()
    v.push(n)
    v.push(n + 1)
    if flag {
        v.push(n + 2)
    }
    return Holder{tag: n, items: v}
}
fn consume(h Holder) u64 {
    return h.tag + u64(h.items.len())
}
fn main() {
    var total: u64 = 0
    var i: u64 = 0
    for i < 6 {
        if (i & 1) == 0 {
            h_even := make_holder(i, true)
            total = total + consume(h_even)
        } else {
            h_odd := make_holder(i, false)
            total = total + consume(h_odd)
        }
        i = i + 1
    }
    print(total)
}
"#;
    let (out, _) = compile_and_run(src, "loops-drops");
    // i=0..5: 0+3 + 1+2 + 2+3 + 3+2 + 4+3 + 5+2 = 30
    assert_eq!(out.stdout, b"30\n");
}

#[test]
fn deterministic_packing_and_freestanding_emission() {
    let res = tarn_driver::check(Path::new("../../tests/freestanding/arithmetic.tarn")).unwrap();
    assert!(!res.has_errors());
    let drops = res.drops.as_ref().unwrap();
    let typed = res.typed.as_ref().unwrap();
    let target: tarn_backend::Target = "x86_64-none".parse().unwrap();
    let a = tarn_backend::emit_object_for(drops, typed, target).unwrap();
    let b = tarn_backend::emit_object_for(drops, typed, target).unwrap();
    assert_eq!(a, b, "stack slot packing must be deterministic");
}

#[test]
fn in_place_nested_variant_and_struct_aggregate_does_not_alias_source() {
    let src = r#"
copy struct Span {
    off usize
    len usize
}
copy struct Wait {
    name Span
    min_epoch u64
    deadline_ms u32
}
copy enum Req {
    Lookup(Span)
    WaitEpoch(Wait)
}
fn check_req(r Req, data_len usize) u64 {
    match r {
        Lookup(s) => {
            if s.off + s.len > data_len { return 99 }
            return u64(s.len)
        }
        WaitEpoch(w) => {
            if w.name.off != 0 || w.name.len != data_len { return 99 }
            if w.min_epoch != 1 || w.deadline_ms != 2000 { return 98 }
            return u64(w.name.len) + u64(w.deadline_ms)
        }
    }
}
fn main() {
    name := "console.vt1"
    r := Req.WaitEpoch(Wait{
        name: Span{off: 0, len: name.len()},
        min_epoch: 1,
        deadline_ms: 2000,
    })
    print(check_req(r, name.len()))
}
"#;
    let (out, _) = compile_and_run(src, "nested-agg-inplace");
    assert_eq!(out.stdout, b"2011\n");
}

#[test]
fn console_reply_info_encode_does_not_clobber() {
    let src = r#"
copy struct Info {
    cols u16
    rows u16
    vt u8
    mode u8
    echo bool
    scrollback u16
}
copy enum Reply {
    Ok
    Info(Info)
    Closed
}
copy enum WireErr {
    Trunc
    Value
}
copy struct WireW {
    len usize
    err u8
}
fn w_new() WireW { return WireW{len: 0, err: 0} }
fn w_u8(w &mut WireW, out &mut []u8, v u8) {
    if w.err != 0 { return }
    if w.len >= out.len() {
        w.err = 1
        return
    }
    out[w.len] = v
    w.len = w.len + 1
}
fn w_u16(w &mut WireW, out &mut []u8, v u16) {
    w_u8(w, out, u8(v & 255))
    w_u8(w, out, u8((v >> 8) & 255))
}
fn w_finish(w WireW) Result<usize, WireErr> {
    if w.err != 0 { return Err(WireErr.Trunc) }
    return Ok(w.len)
}
fn zeros256() Vec<u8> {
    var v: Vec<u8> = Vec.new()
    block := [64]u8{0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0}
    var i: usize = 0
    for i + 64 <= 256 {
        v.extend_from_slice(&block[0..64])
        i = i + 64
    }
    return v
}
fn encode_reply(out &mut []u8, seq u32, r Reply, data &[]u8) Result<usize, WireErr> {
    var w = w_new()
    match r {
        Reply.Ok => { w_u8(&mut w, out, 1) }
        Reply.Info(i) => {
            if i.mode > 1 || i.cols == 0 || i.rows == 0 {
                return Err(WireErr.Value)
            }
            w_u8(&mut w, out, u8(seq))
            w_u16(&mut w, out, i.cols)
            w_u16(&mut w, out, i.rows)
            w_u8(&mut w, out, i.vt)
            w_u8(&mut w, out, i.mode)
            if i.echo { w_u8(&mut w, out, 1) } else { w_u8(&mut w, out, 0) }
            w_u16(&mut w, out, i.scrollback)
        }
        Reply.Closed => { w_u8(&mut w, out, 2) }
    }
    return w_finish(w)
}
fn answer(seq u32, r Reply, data &[]u8) usize {
    var payload = zeros256()
    n := match encode_reply(payload.as_mut_slice(), seq, r, data) {
        Ok(v) => v
        Err(_) => 999
    }
    return n + payload.len()
}
copy struct ReqMsg {
    kind u8
    seq u32
}
fn dispatch(msg ReqMsg, mode u8, echo bool) usize {
    match msg.kind {
        1 => {
            return answer(msg.seq, Reply.Info(Info{
                cols: 80,
                rows: 25,
                vt: 1,
                mode: mode,
                echo: echo,
                scrollback: 0,
            }), "".bytes())
        }
        _ => { return 0 }
    }
}
fn main() {
    m := ReqMsg{kind: 1, seq: 42}
    print(dispatch(m, 0, true))
}
"#;
    let (out, _) = compile_and_run(src, "console-reply-info");
    assert_eq!(out.stdout, b"266\n");
}

