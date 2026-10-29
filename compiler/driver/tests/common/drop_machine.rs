//! Shared resource-token execution oracle for post-drop and native differential tests.
#![allow(dead_code)]
use std::collections::HashSet;
use tarn_ir::{post_drop as post, *};
#[derive(Clone, Debug)]
pub(crate) enum Value {
    Unit,
    Number(i128),
    Token(usize),
    Fields(Vec<Option<Value>>),
    Variant(usize, Vec<Option<Value>>),
}
#[derive(Clone, Debug)]
enum Flag {
    Bit(bool),
    Bits(Vec<bool>),
}
struct Frame {
    locals: Vec<Option<Value>>,
    flags: Vec<Option<Flag>>,
}
pub(crate) struct Machine<'a> {
    pub(crate) p: &'a post::Program,
    pub(crate) created: usize,
    pub(crate) labels: Vec<String>,
    pub(crate) destroyed: HashSet<usize>,
    pub(crate) trace: Vec<usize>,
    pub(crate) steps: usize,
    pub(crate) aborted: bool,
}
impl Machine<'_> {
    fn token(&mut self, label: &str) -> Value {
        let id = self.created;
        self.created += 1;
        self.labels.push(label.to_string());
        Value::Token(id)
    }
    fn destroy(&mut self, v: Value) {
        match v {
            Value::Token(id) => {
                assert!(self.destroyed.insert(id), "double drop {id}");
                self.trace.push(id);
            }
            Value::Fields(xs) | Value::Variant(_, xs) => {
                for x in xs {
                    self.destroy(x.expect("complete drop of partial value"));
                }
            }
            _ => {}
        }
    }
    fn slot<'a>(fr: &'a mut Frame, p: &Place) -> &'a mut Option<Value> {
        let mut slot = &mut fr.locals[p.local.0 as usize];
        for pr in &p.proj {
            match pr {
                Proj::Downcast(v) => assert!(matches!(slot, Some(Value::Variant(tag, _)) if *tag == *v as usize)),
                Proj::Field(i) => {
                    let xs = match slot.as_mut().expect("projection through moved place") {
                        Value::Fields(xs) | Value::Variant(_, xs) => xs,
                        v => panic!("field on {v:?}"),
                    };
                    slot = &mut xs[*i as usize];
                }
                Proj::Index(i) => {
                    // The index itself is always a scalar; read before borrowing
                    // the array slot by restarting the walk with a constant.
                    panic!("index must be resolved before slot: {i:?}");
                }
                Proj::Deref => panic!("reference interpreter not used in drop fixtures"),
            }
        }
        slot
    }
    fn take(fr: &mut Frame, p: &Place, copy: bool) -> Value {
        if let Some(Proj::Index(i)) = p.proj.last() {
            let Some(Value::Number(idx)) = fr.locals[i.0 as usize] else { panic!("bad index") };
            let base = Place { local: p.local, proj: p.proj[..p.proj.len() - 1].to_vec() };
            let Some(Value::Fields(xs)) = Self::slot(fr, &base) else { panic!("index on non-array") };
            return if copy { xs[idx as usize].clone().unwrap() } else { xs[idx as usize].take().unwrap() };
        }
        let slot = Self::slot(fr, p);
        if copy { slot.clone().expect("copy uninit") } else { slot.take().expect("move uninit") }
    }
    fn op(&mut self, fr: &mut Frame, o: &Operand) -> Value {
        match o {
            Operand::Copy(p) => Self::take(fr, p, true),
            Operand::Move(p) => Self::take(fr, p, false),
            Operand::Const(Const::Int(n, _)) => Value::Number(*n),
            Operand::Const(Const::Bool(b)) => Value::Number(i128::from(*b)),
            Operand::Const(Const::Str(s)) => self.token(s),
            Operand::Const(Const::Unit) => Value::Unit,
            x => panic!("unsupported operand {x:?}"),
        }
    }
    fn rv(&mut self, fr: &mut Frame, rv: &Rvalue) -> Value {
        match rv {
            Rvalue::Use(o) => self.op(fr, o),
            Rvalue::Aggregate(a, os) => {
                let xs = os.iter().map(|o| Some(self.op(fr, o))).collect();
                match a {
                    Aggregate::Variant(_, v, _) => Value::Variant(*v as usize, xs),
                    _ => Value::Fields(xs),
                }
            }
            Rvalue::Binary(op, a, b) => {
                let Value::Number(a) = self.op(fr, a) else { panic!("non-number") };
                let Value::Number(b) = self.op(fr, b) else { panic!("non-number") };
                Value::Number(match op {
                    BinOp::Add | BinOp::AddProven => a + b,
                    BinOp::Sub | BinOp::SubProven => a - b,
                    BinOp::Lt => (a < b).into(),
                    BinOp::Eq => (a == b).into(),
                    _ => panic!("binary {op:?}"),
                })
            }
            Rvalue::Discriminant(p) => {
                let Some(Value::Variant(v, _)) = Self::slot(fr, p) else { panic!("bad discriminant") };
                Value::Number(*v as i128)
            }
            Rvalue::Len(p) => {
                let Some(Value::Fields(xs)) = Self::slot(fr, p) else { panic!("bad length") };
                Value::Number(xs.len() as i128)
            }
            x => panic!("unsupported rvalue {x:?}"),
        }
    }
    fn drop_plan(&mut self, fr: &mut Frame, d: &post::Drop) {
        match d {
            post::Drop::Value(p) => self.destroy(Self::take(fr, p, false)),
            post::Drop::Guard(id, d) => {
                let Some(Flag::Bit(b)) = fr.flags[id.0 as usize] else { panic!("uninit flag") };
                if b {
                    self.drop_plan(fr, d);
                }
            }
            post::Drop::Fields { fields, .. } => {
                for (_, d) in fields {
                    self.drop_plan(fr, d);
                }
            }
            post::Drop::Variants { place, variants } => {
                let Some(Value::Variant(tag, _)) = Self::slot(fr, place) else { panic!("dead discriminant") };
                let tag = *tag;
                for (_, d) in &variants[tag] {
                    self.drop_plan(fr, d);
                }
            }
            // The abstract machine has no vector storage: a consuming vector
            // loop's leftovers are the whole remaining value.
            post::Drop::RemainingVec { place, .. } => self.destroy(Self::take(fr, place, false)),
            post::Drop::Remaining { place, flag } => {
                let Some(Flag::Bits(bits)) = fr.flags[flag.0 as usize].clone() else { panic!("uninit bitmap") };
                let Some(Value::Fields(xs)) = Self::slot(fr, place).take() else { panic!("dead array") };
                for (live, v) in bits.into_iter().zip(xs) {
                    if live {
                        self.destroy(v.expect("drop hole"));
                    } else {
                        assert!(v.is_none(), "lost live array element");
                    }
                }
            }
        }
    }
    pub(crate) fn call(&mut self, id: FunctionId, args: Vec<Value>) -> Value {
        let f = &self.p.functions[id.0 as usize];
        let mut fr = Frame { locals: vec![None; f.decl.locals.len()], flags: vec![None; f.flags.len()] };
        for (i, v) in args.into_iter().enumerate() {
            fr.locals[i + 1] = Some(v);
        }
        let mut bi = 0;
        loop {
            self.steps += 1;
            assert!(self.steps < 10000, "non-terminating fixture");
            let b = &f.blocks[bi];
            for s in &b.stmts {
                match &s.op {
                    post::Op::Set(id, v) => {
                        fr.flags[id.0 as usize] = Some(match &f.flags[id.0 as usize] {
                            post::FlagKind::Value(_) => Flag::Bit(*v),
                            post::FlagKind::Elements(_, n) => Flag::Bits(vec![*v; *n as usize]),
                        })
                    }
                    post::Op::ClearElement(id, idx) => {
                        let Some(Value::Number(i)) = fr.locals[idx.0 as usize] else { panic!("bad bitmap index") };
                        let Some(Flag::Bits(bits)) = &mut fr.flags[id.0 as usize] else { panic!("uninit bitmap") };
                        assert!(bits[i as usize], "double take array element");
                        bits[i as usize] = false;
                    }
                    post::Op::Destroy(d) => self.drop_plan(&mut fr, d),
                    post::Op::Plain(StatementKind::Assign(p, rv)) => {
                        let v = self.rv(&mut fr, rv);
                        let slot = Self::slot(&mut fr, p);
                        assert!(!slot.as_ref().is_some_and(has_resource), "overwrite without destruction: {p:?}");
                        *slot = Some(v);
                    }
                    post::Op::Plain(StatementKind::StorageLive(l) | StatementKind::StorageDead(l)) => {
                        assert!(!fr.locals[l.0 as usize].as_ref().is_some_and(has_resource), "scope lost live resource");
                        fr.locals[l.0 as usize] = None;
                    }
                    x => panic!("unexpected {x:?}"),
                }
            }
            match &b.term {
                Terminator::Goto(next) => bi = next.0 as usize,
                Terminator::Switch { discr, cases, otherwise } => {
                    let Value::Number(v) = self.op(&mut fr, discr) else { panic!("bad switch") };
                    bi = cases.iter().find(|(n, _)| *n == v).map(|(_, b)| *b).unwrap_or(*otherwise).0 as usize;
                }
                Terminator::Call { callee, args, dest, next, .. } => {
                    let args = args.iter().map(|o| self.op(&mut fr, o)).collect();
                    let value = match callee {
                        Callee::Fn(id, _) => self.call(*id, args),
                        Callee::Builtin(Builtin::Panic) => {
                            self.aborted = true;
                            return Value::Unit;
                        }
                        c => panic!("unsupported callee {c:?}"),
                    };
                    if self.aborted {
                        return Value::Unit;
                    }
                    let slot = Self::slot(&mut fr, dest);
                    assert!(!slot.as_ref().is_some_and(has_resource), "call overwrite live");
                    *slot = Some(value);
                    bi = next.expect("returning call without edge").0 as usize;
                }
                Terminator::Return => {
                    assert!(fr.locals.iter().skip(1).all(|v| !v.as_ref().is_some_and(has_resource)), "return leaked resources: {:?}", fr.locals);
                    return fr.locals[0].take().unwrap_or(Value::Unit);
                }
                Terminator::Unreachable => panic!("executed unreachable"),
                Terminator::Suspend { .. } | Terminator::Abandon => panic!("suspension edge after frame lowering"),
            }
        }
    }
}
fn has_resource(v: &Value) -> bool {
    match v {
        Value::Token(_) => true,
        Value::Fields(xs) | Value::Variant(_, xs) => xs.iter().flatten().any(has_resource),
        _ => false,
    }
}

/// Run a fixture entry and return the actual destruction labels in order.
pub fn execute(p: &post::Program, id: FunctionId, args: &[bool]) -> (Vec<String>, bool) {
    let mut m = Machine { p, created: 0, labels: Vec::new(), destroyed: HashSet::new(), trace: Vec::new(), steps: 0, aborted: false };
    m.call(id, args.iter().map(|v| Value::Number(i128::from(*v))).collect());
    if !m.aborted {
        assert_eq!(m.created, m.destroyed.len(), "resource leak");
    }
    (m.trace.iter().map(|id| m.labels[*id].clone()).collect(), m.aborted)
}
