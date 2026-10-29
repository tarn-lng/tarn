//! Stable S-expression dump of the AST, used by `tarn ast` and snapshot tests.
//! Spans and node ids are omitted so snapshots only change when structure does.

use crate::*;
use std::fmt::Write;

pub fn dump_module(m: &Module) -> String {
    let mut d = Dumper { out: String::new() };
    for item in &m.items {
        d.item(item, 0);
        d.out.push('\n');
    }
    d.out
}

/// A type in source syntax, e.g. `Option<&[]u8>`.
pub fn type_to_string(t: &Type) -> String {
    let mut d = Dumper { out: String::new() };
    d.ty(t);
    d.out
}

struct Dumper {
    out: String,
}

impl Dumper {
    fn nl(&mut self, indent: usize) {
        self.out.push('\n');
        for _ in 0..indent {
            self.out.push_str("  ");
        }
    }

    fn s(&mut self, s: &str) {
        self.out.push_str(s);
    }

    fn item(&mut self, item: &Item, ind: usize) {
        let p = if item.is_pub { "pub " } else { "" };
        match &item.kind {
            ItemKind::Import(i) => {
                let _ = write!(self.out, "(import {:?})", i.path);
            }
            ItemKind::Fn(f) => self.func(f, p, ind),
            ItemKind::Const(c) => {
                let _ = write!(self.out, "({p}const {} ", c.name.name);
                self.ty(&c.ty);
                self.s(" ");
                self.expr(&c.value, ind);
                self.s(")");
            }
            ItemKind::Struct(s) => {
                let _ = write!(self.out, "({p}{}struct {}", if s.is_copy { "copy " } else { "" }, s.name.name);
                self.generics(&s.generics);
                for f in &s.fields {
                    self.nl(ind + 1);
                    let _ = write!(self.out, "({}{} ", if f.is_pub { "pub " } else { "" }, f.name.name);
                    self.ty(&f.ty);
                    self.s(")");
                }
                self.s(")");
            }
            ItemKind::Enum(e) => {
                let _ = write!(self.out, "({p}{}enum {}", if e.is_copy { "copy " } else { "" }, e.name.name);
                self.generics(&e.generics);
                for v in &e.variants {
                    self.nl(ind + 1);
                    let _ = write!(self.out, "({}", v.name.name);
                    for t in &v.fields {
                        self.s(" ");
                        self.ty(t);
                    }
                    self.s(")");
                }
                self.s(")");
            }
            ItemKind::Interface(i) => {
                let _ = write!(self.out, "({p}interface {}", i.name.name);
                self.generics(&i.generics);
                for m in &i.methods {
                    self.nl(ind + 1);
                    self.func(m, "", ind + 1);
                }
                self.s(")");
            }
            ItemKind::Impl(i) => {
                self.s("(impl ");
                self.path(&i.interface);
                self.s(" for ");
                self.ty(&i.target);
                for (index, bounds) in i.binder_bounds.iter().enumerate() {
                    for (j, b) in bounds.iter().enumerate() {
                        if j == 0 {
                            let _ = write!(self.out, " where #{index}: ");
                        } else {
                            self.s(" + ");
                        }
                        self.path(b);
                    }
                }
                for m in &i.methods {
                    self.nl(ind + 1);
                    self.func(m, "", ind + 1);
                }
                self.s(")");
            }
            ItemKind::Error => self.s("(error-item)"),
        }
    }

    fn generics(&mut self, g: &[GenericParam]) {
        if g.is_empty() {
            return;
        }
        self.s(" <");
        for (i, p) in g.iter().enumerate() {
            if i > 0 {
                self.s(", ");
            }
            self.s(&p.name.name);
            for (j, b) in p.bounds.iter().enumerate() {
                self.s(if j == 0 { ": " } else { " + " });
                self.path(b);
            }
        }
        self.s(">");
    }

    fn func(&mut self, f: &FnDecl, vis: &str, ind: usize) {
        self.s("(");
        self.s(vis);
        if f.is_unsafe { self.s("unsafe "); }
        if f.is_async { self.s("async "); }
        self.s("fn ");
        if let Some(abi) = &f.abi {
            let _ = write!(self.out, "extern {abi:?} ");
        }
        if let Some(o) = &f.owner {
            self.s(&o.name.name);
            if !o.params.is_empty() {
                self.generics(&o.params);
                // `generics` prints a leading space; owner lists are tight.
                let n = self.out.rfind(" <").unwrap();
                self.out.remove(n);
            }
            self.s(".");
        }
        self.s(&f.name.name);
        self.generics(&f.generics);
        self.s(" (");
        let mut first = true;
        if let Some(r) = &f.receiver {
            self.s(match r.kind {
                ReceiverKind::Value => "self",
                ReceiverKind::Ref => "&self",
                ReceiverKind::RefMut => "&mut self",
            });
            first = false;
        }
        for p in &f.params {
            if !first {
                self.s(" ");
            }
            first = false;
            let _ = write!(self.out, "({} ", p.name.name);
            self.ty(&p.ty);
            self.s(")");
        }
        self.s(")");
        if let Some(r) = &f.ret {
            self.s(" -> ");
            self.ty(r);
        }
        if let Some(sources) = &f.borrows {
            self.s(" borrows(");
            self.s(&sources.iter().map(|s| s.name.clone()).collect::<Vec<_>>().join(", "));
            self.s(")");
        }
        if let Some(b) = &f.body {
            self.nl(ind + 1);
            self.block(b, ind + 1);
        }
        self.s(")");
    }

    fn path(&mut self, p: &Path) {
        for (i, s) in p.segments.iter().enumerate() {
            if i > 0 {
                self.s(".");
            }
            self.s(&s.name);
        }
        if !p.args.is_empty() {
            self.s("<");
            for (i, a) in p.args.iter().enumerate() {
                if i > 0 {
                    self.s(", ");
                }
                self.ty(a);
            }
            self.s(">");
        }
    }

    fn ty(&mut self, t: &Type) {
        match &t.kind {
            TypeKind::Path(p) => self.path(p),
            TypeKind::Ref { mutable, inner } => {
                self.s(if *mutable { "&mut " } else { "&" });
                self.ty(inner);
            }
            TypeKind::Ptr { mutable, inner } => {
                self.s(if *mutable { "*mut " } else { "*" });
                self.ty(inner);
            }
            TypeKind::Slice(e) => {
                self.s("[]");
                self.ty(e);
            }
            TypeKind::Array { len, elem } => {
                self.s("[");
                self.expr(len, 0);
                self.s("]");
                self.ty(elem);
            }
            TypeKind::Fn { mode, params, ret } => {
                self.s(mode.prefix());
                self.s("fn(");
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        self.s(", ");
                    }
                    self.ty(p);
                }
                self.s(")");
                if let Some(r) = ret {
                    self.s(" ");
                    self.ty(r);
                }
            }
            TypeKind::Any(p) => {
                self.s("any ");
                self.path(p);
            }
            TypeKind::Error => self.s("<error-type>"),
        }
    }

    fn block(&mut self, b: &Block, ind: usize) {
        self.s("(block");
        for st in &b.stmts {
            self.nl(ind + 1);
            self.stmt(st, ind + 1);
        }
        self.s(")");
    }

    fn match_(&mut self, m: &MatchStmt, ind: usize) {
        self.s("(match ");
        self.expr(&m.scrutinee, ind);
        for arm in &m.arms {
            self.nl(ind + 1);
            self.s("(");
            self.pattern(&arm.pattern);
            if let Some(g) = &arm.guard {
                self.s(" if ");
                self.expr(g, ind + 1);
            }
            self.s(" => ");
            self.stmt(&arm.body, ind + 1);
            self.s(")");
        }
        self.s(")");
    }

    fn stmt(&mut self, st: &Stmt, ind: usize) {
        match &st.kind {
            StmtKind::Let { mutable, name, ty, value } => {
                let _ = write!(self.out, "({} {}", if *mutable { "var" } else { "let" }, name.name);
                if let Some(t) = ty {
                    self.s(" ");
                    self.ty(t);
                }
                match value {
                    Some(v) => {
                        self.s(" ");
                        self.expr(v, ind);
                    }
                    None => self.s(" <uninit>"),
                }
                self.s(")");
            }
            StmtKind::Assign { target, value } => {
                self.s("(= ");
                self.expr(target, ind);
                self.s(" ");
                self.expr(value, ind);
                self.s(")");
            }
            StmtKind::Expr(e) => self.expr(e, ind),
            StmtKind::Return(e) => {
                self.s("(return");
                if let Some(e) = e {
                    self.s(" ");
                    self.expr(e, ind);
                }
                self.s(")");
            }
            StmtKind::Break => self.s("(break)"),
            StmtKind::Continue => self.s("(continue)"),
            StmtKind::If(i) => self.if_stmt(i, ind),
            StmtKind::For(f) => {
                self.s("(for");
                match &f.kind {
                    ForKind::Infinite => {}
                    ForKind::While(c) => {
                        self.s(" ");
                        self.expr(c, ind);
                    }
                    ForKind::In { binding, iter } => {
                        let _ = write!(self.out, " {} in ", binding.name);
                        self.expr(iter, ind);
                    }
                }
                self.nl(ind + 1);
                self.block(&f.body, ind + 1);
                self.s(")");
            }
            StmtKind::Match(m) => self.match_(m, ind),
            StmtKind::Block(b) => self.block(b, ind),
            StmtKind::Unsafe(b) => {
                self.s("(unsafe ");
                self.block(b, ind);
                self.s(")");
            }
            StmtKind::Scope(b) => {
                self.s("(scope ");
                self.block(b, ind);
                self.s(")");
            }
            StmtKind::Spawn(e) => {
                self.s("(spawn ");
                self.expr(e, ind);
                self.s(")");
            }
            StmtKind::Error => self.s("(error-stmt)"),
        }
    }

    fn if_stmt(&mut self, i: &IfStmt, ind: usize) {
        self.s("(if ");
        self.expr(&i.cond, ind);
        self.nl(ind + 1);
        self.block(&i.then_block, ind + 1);
        match i.else_branch.as_deref() {
            None => {}
            Some(ElseBranch::Block(b)) => {
                self.nl(ind + 1);
                self.s("else ");
                self.block(b, ind + 1);
            }
            Some(ElseBranch::If(e)) => {
                self.nl(ind + 1);
                self.s("else ");
                self.if_stmt(e, ind + 1);
            }
        }
        self.s(")");
    }

    fn exprs(&mut self, es: &[Expr], ind: usize) {
        for e in es {
            self.s(" ");
            self.expr(e, ind);
        }
    }

    fn expr(&mut self, e: &Expr, ind: usize) {
        match &e.kind {
            ExprKind::Int(v) => {
                let _ = write!(self.out, "{v}");
            }
            ExprKind::Float(s) => self.s(s),
            ExprKind::Str(s) => {
                let _ = write!(self.out, "{s:?}");
            }
            ExprKind::Interpolated(parts) => {
                self.s("(interp");
                for part in parts {
                    self.s(" ");
                    match part {
                        InterpPart::Text(t) => {
                            let _ = write!(self.out, "{t:?}");
                        }
                        InterpPart::Value(v) => self.expr(v, ind),
                    }
                }
                self.s(")");
            }
            ExprKind::Bool(b) => {
                let _ = write!(self.out, "{b}");
            }
            ExprKind::Unit => self.s("()"),
            ExprKind::Ident(n) => self.s(n),
            ExprKind::Paren(inner) => {
                self.s("(paren ");
                self.expr(inner, ind);
                self.s(")");
            }
            ExprKind::Field { base, name } => {
                self.s("(. ");
                self.expr(base, ind);
                let _ = write!(self.out, " {})", name.name);
            }
            ExprKind::Call { callee, args } => {
                self.s("(call ");
                self.expr(callee, ind);
                self.exprs(args, ind);
                self.s(")");
            }
            ExprKind::Index { base, index } => {
                self.s("(index ");
                self.expr(base, ind);
                self.s(" ");
                self.expr(index, ind);
                self.s(")");
            }
            ExprKind::Unary { op, operand } => {
                let _ = write!(self.out, "({} ", op.symbol());
                self.expr(operand, ind);
                self.s(")");
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let _ = write!(self.out, "({} ", op.symbol());
                self.expr(lhs, ind);
                self.s(" ");
                self.expr(rhs, ind);
                self.s(")");
            }
            ExprKind::Range { start, end, inclusive } => {
                self.s(if *inclusive { "(..= " } else { "(.. " });
                match start {
                    Some(s) => self.expr(s, ind),
                    None => self.s("_"),
                }
                self.s(" ");
                match end {
                    Some(s) => self.expr(s, ind),
                    None => self.s("_"),
                }
                self.s(")");
            }
            ExprKind::Try(inner) => {
                self.s("(try ");
                self.expr(inner, ind);
                self.s(")");
            }
            ExprKind::Await(inner) => {
                self.s("(await ");
                self.expr(inner, ind);
                self.s(")");
            }
            ExprKind::StructLit { path, fields } => {
                self.s("(struct ");
                self.path(path);
                for f in fields {
                    let _ = write!(self.out, " ({}", f.name.name);
                    if let Some(v) = &f.value {
                        self.s(" ");
                        self.expr(v, ind);
                    }
                    self.s(")");
                }
                self.s(")");
            }
            ExprKind::ArrayLit { ty, elems } => {
                self.s("(array ");
                self.ty(ty);
                self.exprs(elems, ind);
                self.s(")");
            }
            ExprKind::Match(m) => self.match_(m, ind),
            ExprKind::Closure { owned, params, ret, body } => {
                self.s(if *owned { "(move-closure (" } else { "(closure (" });
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        self.s(" ");
                    }
                    self.s(&p.name.name);
                    if let Some(t) = &p.ty {
                        self.s(" ");
                        self.ty(t);
                    }
                }
                self.s(")");
                if let Some(r) = ret {
                    self.s(" -> ");
                    self.ty(r);
                }
                self.nl(ind + 1);
                self.block(body, ind + 1);
                self.s(")");
            }
            ExprKind::Error => self.s("<error-expr>"),
        }
    }

    fn pattern(&mut self, p: &Pattern) {
        match &p.kind {
            PatternKind::Wildcard => self.s("_"),
            PatternKind::Ident(n) => self.s(n),
            PatternKind::Literal(e) => self.expr(e, 0),
            PatternKind::Range { start, end, inclusive } => {
                self.s(if *inclusive { "(..= " } else { "(.. " });
                self.expr(start, 0);
                self.s(" ");
                self.expr(end, 0);
                self.s(")");
            }
            PatternKind::Variant { path, args } => {
                self.s("(");
                self.path(path);
                for a in args {
                    self.s(" ");
                    self.pattern(a);
                }
                self.s(")");
            }
            PatternKind::Struct { path, fields } => {
                self.s("(struct ");
                self.path(path);
                for f in fields {
                    let _ = write!(self.out, " ({}", f.name.name);
                    if let Some(p) = &f.pattern {
                        self.s(" ");
                        self.pattern(p);
                    }
                    self.s(")");
                }
                self.s(")");
            }
            PatternKind::Error => self.s("<error-pattern>"),
        }
    }
}
