//! `const` items (ADR 0090): evaluated once, at compile time, to a scalar or
//! string. Arithmetic is checked exactly as at run time, so an overflowing or
//! dividing-by-zero constant is a compile error instead of a trap.
use crate::env::Env;
use crate::ty::{FloatTy, IntTy, Ty};
use std::collections::HashMap;
use tarn_ast::{BinaryOp, ConstDecl, Expr, ExprKind, ItemKind, UnaryOp};
use tarn_diagnostics::{Diagnostic, Span};
use tarn_resolve::{ModuleId, Res, SymbolId, SymbolKind};

#[derive(Clone, Debug, PartialEq)]
pub enum ConstValue {
    Int(i128),
    /// Literal text, as written (without `_`).
    Float(String),
    Bool(bool),
    Str(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConstDef {
    pub ty: Ty,
    pub value: ConstValue,
}

enum State {
    Active,
    Done(Option<ConstDef>),
}

struct Eval<'e, 'a> {
    env: &'e mut Env<'a>,
    decls: HashMap<SymbolId, (ModuleId, &'a ConstDecl)>,
    state: HashMap<SymbolId, State>,
}

/// Boxed: a diagnostic is large and errors are the rare path.
type Fail = Box<Diagnostic>;

/// Stands for an error already reported at another constant's declaration.
fn reported() -> Diagnostic {
    Diagnostic::error("", "", "")
}

fn invalid(span: Span, message: impl Into<String>) -> Diagnostic {
    Diagnostic::error("E3084", "invalid_constant", message.into()).primary(span, "")
}

/// Evaluate every `const` item into `env.decls.consts`.
pub fn evaluate(env: &mut Env<'_>) {
    let mut decls = HashMap::new();
    for (mi, input) in env.inputs.iter().enumerate() {
        for item in &input.ast.items {
            if let ItemKind::Const(c) = &item.kind
                && let Some(&sym) = env.r.tables[mi].defs.get(&item.id)
            {
                decls.insert(sym, (ModuleId(mi as u32), c));
            }
        }
    }
    let mut order: Vec<SymbolId> = decls.keys().copied().collect();
    order.sort_by_key(|s| s.0);
    let mut ev = Eval { env, decls, state: HashMap::new() };
    for sym in order {
        ev.constant(sym);
    }
}

impl Eval<'_, '_> {
    fn constant(&mut self, sym: SymbolId) -> Option<ConstDef> {
        match self.state.get(&sym) {
            Some(State::Done(d)) => return d.clone(),
            Some(State::Active) => return None,
            None => {}
        }
        let (m, c) = *self.decls.get(&sym)?;
        self.state.insert(sym, State::Active);
        let ty = self.env.lower(m, &c.ty);
        let def = if matches!(ty, Ty::Int(_) | Ty::Float(_) | Ty::Bool | Ty::Str) {
            match self.value(m, &c.value, &ty) {
                Ok(value) => Some(ConstDef { ty, value }),
                Err(d) => {
                    if !d.code.is_empty() {
                        self.env.diags.push(*d);
                    }
                    None
                }
            }
        } else {
            if ty != Ty::Error {
                self.env.diags.push(
                    invalid(c.ty.span, "a constant must be an integer, float, `bool` or `string`")
                        .help("use a function for other values"),
                );
            }
            None
        };
        if let Some(d) = &def {
            self.env.decls.consts.insert(sym, d.clone());
        }
        self.state.insert(sym, State::Done(def.clone()));
        def
    }

    /// A referenced constant, if `e` names one.
    fn named(&mut self, m: ModuleId, e: &Expr) -> Result<Option<ConstDef>, Fail> {
        if !matches!(e.kind, ExprKind::Ident(_) | ExprKind::Field { .. }) {
            return Ok(None);
        }
        let Some(Res::Symbol(s)) = self.env.r.tables[m.0 as usize].uses.get(&e.id).map(|u| u.res.clone()) else {
            return Err(invalid(e.span, "a constant can only refer to other constants").into());
        };
        if !matches!(self.env.r.symbol(s).kind, SymbolKind::Const) {
            return Err(invalid(e.span, "a constant can only refer to other constants").help("constants are evaluated at compile time; use a function instead").into());
        }
        if matches!(self.state.get(&s), Some(State::Active)) {
            return Err(invalid(e.span, format!("constant `{}` depends on itself", self.env.r.symbol(s).name)).into());
        }
        // Errors in the referenced constant are reported at its declaration.
        Ok(Some(self.constant(s).ok_or_else(reported)?))
    }

    fn value(&mut self, m: ModuleId, e: &Expr, ty: &Ty) -> Result<ConstValue, Fail> {
        let mismatch = |found: &str| invalid(e.span, format!("expected `{}`, found {found}", show(ty)));
        if let Some(def) = self.named(m, e)? {
            if &def.ty != ty {
                return Err(mismatch(&format!("constant of type `{}`", show(&def.ty))).help(format!("convert explicitly: `{}(...)`", show(ty))).into());
            }
            return Ok(def.value);
        }
        match (&e.kind, ty) {
            (ExprKind::Paren(inner), _) => self.value(m, inner, ty),
            (ExprKind::Int(v), Ty::Int(it)) => fit(*v as i128, *it, e.span),
            (ExprKind::Float(s), Ty::Float(_)) => Ok(ConstValue::Float(s.clone())),
            (ExprKind::Bool(b), Ty::Bool) => Ok(ConstValue::Bool(*b)),
            (ExprKind::Str(s), Ty::Str) => Ok(ConstValue::Str(s.clone())),
            (ExprKind::Unary { op: UnaryOp::Neg, operand }, Ty::Float(_)) if matches!(operand.kind, ExprKind::Float(_)) => {
                let ExprKind::Float(s) = &operand.kind else { unreachable!() };
                Ok(ConstValue::Float(format!("-{s}")))
            }
            (ExprKind::Unary { op: UnaryOp::Neg, operand }, Ty::Int(it)) => {
                let v = self.int(m, operand, *it)?;
                fit(-v, *it, e.span)
            }
            (ExprKind::Unary { op: UnaryOp::Not, operand }, Ty::Bool) => Ok(ConstValue::Bool(!self.bool(m, operand)?)),
            (ExprKind::Unary { op: UnaryOp::Not, operand }, Ty::Int(it)) => {
                let v = self.int(m, operand, *it)?;
                let (lo, hi) = it.range();
                Ok(ConstValue::Int(if it.signed() { !v } else { hi - (v - lo) }))
            }
            (ExprKind::Binary { op: BinaryOp::And | BinaryOp::Or, lhs, rhs }, Ty::Bool) => {
                let (a, b) = (self.bool(m, lhs)?, self.bool(m, rhs)?);
                Ok(ConstValue::Bool(if matches!(e.kind, ExprKind::Binary { op: BinaryOp::And, .. }) { a && b } else { a || b }))
            }
            (ExprKind::Binary { op, lhs, rhs }, Ty::Int(it)) => {
                let a = self.int(m, lhs, *it)?;
                let b = self.int(m, rhs, if matches!(op, BinaryOp::Shl | BinaryOp::Shr) { IntTy::U32 } else { *it })?;
                let bits = (it.range().1 - it.range().0 + 1).ilog2() as i128;
                let v = match op {
                    BinaryOp::Add => a + b,
                    BinaryOp::Sub => a - b,
                    BinaryOp::Mul => a.checked_mul(b).ok_or_else(|| invalid(e.span, "constant arithmetic overflows"))?,
                    BinaryOp::Div | BinaryOp::Rem if b == 0 => return Err(invalid(rhs.span, "division by zero in a constant").into()),
                    BinaryOp::Div => a / b,
                    BinaryOp::Rem => a % b,
                    BinaryOp::BitAnd => a & b,
                    BinaryOp::BitOr => a | b,
                    BinaryOp::BitXor => a ^ b,
                    BinaryOp::Shl | BinaryOp::Shr if b >= bits => return Err(invalid(rhs.span, format!("shift by {b} is not less than the {bits}-bit width")).into()),
                    BinaryOp::Shl => a << b,
                    BinaryOp::Shr => a >> b,
                    _ => return Err(invalid(e.span, format!("`{}` does not produce `{}`", op.symbol(), it.name())).into()),
                };
                fit(v, *it, e.span)
            }
            // `u32(OTHER)`: an explicit integer conversion, checked for range.
            (ExprKind::Call { callee, args }, Ty::Int(it)) if args.len() == 1 && self.int_conversion(m, callee) == Some(*it) => {
                let v = match self.named(m, &args[0])? {
                    Some(ConstDef { value: ConstValue::Int(v), .. }) => v,
                    Some(_) => return Err(invalid(args[0].span, "only integers convert to an integer type").into()),
                    None => self.int(m, &args[0], IntTy::I64)?,
                };
                fit(v, *it, e.span)
            }
            (ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Str(_), _) => Err(mismatch("a literal of another type").into()),
            _ => Err(invalid(e.span, "this expression is not a constant")
                .note("constants allow literals, other constants, arithmetic, bitwise and boolean operators, and integer conversions").into()),
        }
    }

    fn int(&mut self, m: ModuleId, e: &Expr, it: IntTy) -> Result<i128, Fail> {
        match self.value(m, e, &Ty::Int(it))? {
            ConstValue::Int(v) => Ok(v),
            _ => unreachable!("integer constant"),
        }
    }

    fn bool(&mut self, m: ModuleId, e: &Expr) -> Result<bool, Fail> {
        match self.value(m, e, &Ty::Bool)? {
            ConstValue::Bool(b) => Ok(b),
            _ => unreachable!("boolean constant"),
        }
    }

    fn int_conversion(&self, m: ModuleId, callee: &Expr) -> Option<IntTy> {
        let Some(Res::Symbol(s)) = self.env.r.tables[m.0 as usize].uses.get(&callee.id).map(|u| u.res.clone()) else { return None };
        let sym = self.env.r.symbol(s);
        if !matches!(sym.kind, SymbolKind::Primitive) {
            return None;
        }
        match crate::env::primitive(&sym.name) {
            Ty::Int(it) => Some(it),
            _ => None,
        }
    }
}

fn fit(v: i128, it: IntTy, span: Span) -> Result<ConstValue, Fail> {
    let (lo, hi) = it.range();
    if v < lo || v > hi {
        return Err(invalid(span, format!("constant value {v} does not fit in `{}`", it.name())).note(format!("`{}` holds {lo}..={hi}", it.name())).into());
    }
    Ok(ConstValue::Int(v))
}

fn show(t: &Ty) -> String {
    match t {
        Ty::Int(it) => it.name().to_string(),
        Ty::Float(FloatTy::F32) => "f32".into(),
        Ty::Float(FloatTy::F64) => "f64".into(),
        Ty::Bool => "bool".into(),
        Ty::Str => "string".into(),
        _ => "_".into(),
    }
}
