//! Lowering from the typed AST to IR.
//!
//! Consumes only decisions recorded by earlier phases: resolution (`uses`,
//! `defs`, captures), types and the type checker's decision tables
//! (coercions, receivers, binding modes, type arguments, pattern variants).

use crate::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use tarn_ast::{self as ast, ElseBranch, Expr, ExprKind, ForKind, ItemKind, Pattern, PatternKind, ReceiverKind, Stmt, StmtKind};
use tarn_diagnostics::{Diagnostic, Severity};
use tarn_resolve::{ModuleId, ModuleInput, Res, Resolved, SymbolKind};
use tarn_types::consts::ConstValue;
use tarn_types::{BindingMode, CoercionKind, MethodTarget, Typed};

/// Program-wide lowering context.
struct Lx<'a> {
    r: &'a Resolved,
    t: &'a Typed,
    by_symbol: HashMap<SymbolId, FunctionId>,
    kinds: HashMap<FunctionId, FnKind>,
    next_id: Cell<u32>,
    /// Closures lowered while lowering their parents.
    extra: RefCell<Vec<Function>>,
    /// Source positions for `panic` and caller locations (ADR 0094).
    sources: Option<&'a tarn_diagnostics::SourceMap>,
}

impl Lx<'_> {
    /// `path:line:column` of `span`, relative to the working directory.
    fn location(&self, span: Span) -> String {
        let Some(sources) = self.sources else { return "<unknown>".into() };
        let name = &sources.file(span.file).name;
        let cwd = std::env::current_dir().ok();
        let shown = cwd.as_ref().and_then(|c| std::path::Path::new(name).strip_prefix(c).ok()).map_or_else(|| name.clone(), |p| p.display().to_string());
        let at = sources.line_col(span);
        format!("{shown}:{}:{}", at.line, at.column)
    }
}

pub fn lower_program(inputs: &[ModuleInput], r: &Resolved, t: &Typed) -> (Program, Vec<Diagnostic>) {
    lower_program_located(inputs, r, t, None)
}

/// `lower_program` that also records source locations: `panic` reports where
/// it was called, and omitted `Location` arguments are filled (ADR 0094).
pub fn lower_program_located(inputs: &[ModuleInput], r: &Resolved, t: &Typed, sources: Option<&tarn_diagnostics::SourceMap>) -> (Program, Vec<Diagnostic>) {
    // 1. Assign an id to every function, method and impl method.
    let mut decls: Vec<(ModuleId, &ast::FnDecl, String)> = Vec::new();
    for (mi, input) in inputs.iter().enumerate() {
        for item in &input.ast.items {
            match &item.kind {
                ItemKind::Fn(f) => {
                    let name = match &f.owner {
                        Some(o) => format!("{}.{}", o.name.name, f.name.name),
                        None => f.name.name.clone(),
                    };
                    decls.push((ModuleId(mi as u32), f, name));
                }
                ItemKind::Impl(i) => {
                    let tname = match &i.target.kind {
                        ast::TypeKind::Path(p) => p.segments.last().map(|s| s.name.clone()).unwrap_or_default(),
                        _ => "?".into(),
                    };
                    for f in &i.methods {
                        decls.push((ModuleId(mi as u32), f, format!("{tname}.{}", f.name.name)));
                    }
                }
                _ => {}
            }
        }
    }
    let mut by_symbol = HashMap::new();
    let mut kinds = HashMap::new();
    let mut ids = Vec::new();
    for (i, (m, f, _)) in decls.iter().enumerate() {
        let id = FunctionId(i as u32);
        let kind = match f.abi.as_deref() {
            Some("intrinsic") => FnKind::Intrinsic,
            Some(_) => FnKind::Extern,
            None => FnKind::Body,
        };
        kinds.insert(id, kind);
        if let Some(sym) = r.tables[m.0 as usize].defs.get(&f.id) {
            by_symbol.insert(*sym, id);
        }
        ids.push(id);
    }
    let lx = Lx { r, t, by_symbol, kinds, next_id: Cell::new(decls.len() as u32), extra: RefCell::new(Vec::new()), sources };

    // 2. Lower bodies.
    let mut functions = Vec::new();
    let mut diags = Vec::new();
    for ((m, f, name), id) in decls.iter().zip(ids) {
        let mut b = Builder::new(&lx, *m, id, name.clone(), f.span);
        b.function(f);
        diags.append(&mut b.diags);
        functions.push(b.finish());
    }
    functions.extend(lx.extra.into_inner());
    functions.sort_by_key(|f| f.id);
    (Program { functions, by_symbol: lx.by_symbol }, diags)
}

struct LoopCx {
    continue_to: BlockId,
    break_to: BlockId,
    depth: usize,
}

struct Builder<'a, 'l> {
    lx: &'l Lx<'a>,
    m: ModuleId,
    f: Function,
    terminated: Vec<bool>,
    cur: BlockId,
    /// The current block is unreachable (after `return`, `break`, `panic`...).
    dead: bool,
    warned_dead: bool,
    vars: HashMap<SymbolId, LocalId>,
    /// In a closure: captured symbol → parameter holding `&T`/`&mut T`.
    captures: HashMap<SymbolId, LocalId>,
    scopes: Vec<Vec<LocalId>>,
    /// Lexical scope depths whose tasks must finish before storage ends.
    task_scopes: Vec<usize>,
    task_witnesses: Vec<LocalId>,
    loops: Vec<LoopCx>,
    /// Temporaries of the statements being lowered (dropped at statement end).
    stmt_temps: Vec<Vec<LocalId>>,
    /// Lowering `x := &<temporary>`: that temporary lives to the end of the scope.
    extend_temps: bool,
    closure_count: u32,
    diags: Vec<Diagnostic>,
}

impl<'a, 'l> Builder<'a, 'l> {
    fn new(lx: &'l Lx<'a>, m: ModuleId, id: FunctionId, name: String, span: Span) -> Self {
        let kind = lx.kinds.get(&id).cloned().unwrap_or(FnKind::Body);
        let f = Function { id, name, symbol: None, kind, generics: Vec::new(), param_count: 0, ret: Ty::Void, locals: Vec::new(), blocks: Vec::new(), span, asynchronous: None };
        let mut b = Builder {
            lx,
            m,
            f,
            terminated: Vec::new(),
            cur: BlockId(0),
            dead: false,
            warned_dead: false,
            vars: HashMap::new(),
            captures: HashMap::new(),
            scopes: Vec::new(),
            task_scopes: Vec::new(),
            task_witnesses: Vec::new(),
            loops: Vec::new(),
            stmt_temps: Vec::new(),
            extend_temps: false,
            closure_count: 0,
            diags: Vec::new(),
        };
        b.cur = b.new_block();
        b
    }

    // ------------------------------------------------------------ tables

    fn tables(&self) -> &'a tarn_types::TypeTables {
        &self.lx.t.tables[self.m.0 as usize]
    }

    fn res(&self, node: ast::NodeId) -> Option<&'a Res> {
        self.lx.r.tables[self.m.0 as usize].uses.get(&node).map(|u| &u.res)
    }

    fn def(&self, node: ast::NodeId) -> Option<SymbolId> {
        self.lx.r.tables[self.m.0 as usize].defs.get(&node).copied()
    }

    fn ty(&self, e: &Expr) -> Ty {
        self.tables().expr_types.get(&e.id).cloned().unwrap_or(Ty::Error)
    }

    fn sym_ty(&self, s: SymbolId) -> Ty {
        self.lx.t.locals.get(&s).cloned().unwrap_or(Ty::Error)
    }

    fn is_copy(&self, t: &Ty) -> bool {
        self.lx.t.decls.is_copy(t)
    }

    /// Values with ownership to release. References, functions and closures
    /// (whose captures are references) own nothing.
    fn needs_drop(&self, t: &Ty) -> bool {
        match t {
            Ty::Ref(..) | Ty::Never | Ty::Void | Ty::Opaque | Ty::Error => false,
            _ => !self.is_copy(t),
        }
    }

    // ------------------------------------------------------------ building blocks

    fn new_block(&mut self) -> BlockId {
        self.f.blocks.push(BasicBlock { stmts: Vec::new(), term: Terminator::Unreachable, term_span: self.f.span });
        self.terminated.push(false);
        BlockId(self.f.blocks.len() as u32 - 1)
    }

    fn push(&mut self, kind: StatementKind, span: Span) {
        self.f.blocks[self.cur.0 as usize].stmts.push(Statement { kind, span });
    }

    fn assign(&mut self, place: Place, rv: Rvalue, span: Span) {
        self.push(StatementKind::Assign(place, rv), span);
    }

    /// End the current block; following code goes to a fresh (dead) block.
    fn terminate(&mut self, term: Terminator, span: Span) {
        let i = self.cur.0 as usize;
        self.f.blocks[i].term = term;
        self.f.blocks[i].term_span = span;
        self.terminated[i] = true;
        self.cur = self.new_block();
        self.dead = true;
    }

    fn switch_to(&mut self, b: BlockId) {
        self.cur = b;
        self.dead = false;
    }

    fn goto(&mut self, target: BlockId, span: Span) {
        self.terminate(Terminator::Goto(target), span);
    }

    fn new_local(&mut self, ty: Ty, kind: LocalKind, name: Option<String>, symbol: Option<SymbolId>, mutable: bool, span: Span) -> LocalId {
        self.f.locals.push(LocalDecl { ty, kind, name, symbol, mutable, span });
        LocalId(self.f.locals.len() as u32 - 1)
    }

    /// A temporary; non-copy temporaries are dropped (if still initialized)
    /// at the end of the enclosing statement.
    fn temp(&mut self, ty: Ty, span: Span) -> LocalId {
        let drop = self.needs_drop(&ty);
        let l = self.new_local(ty, LocalKind::Temp, None, None, false, span);
        if drop && !self.task_scopes.is_empty() && self.lx.t.decls.contains_task(&self.f.local(l).ty) {
            self.scopes.last_mut().unwrap().push(l);
        } else if drop && let Some(ts) = self.stmt_temps.last_mut() {
            ts.push(l);
        }
        l
    }

    fn declare_user(&mut self, sym: SymbolId, span: Span) -> LocalId {
        let s = self.lx.r.symbol(sym);
        let mutable = matches!(s.kind, SymbolKind::Local { mutable: true });
        let l = self.new_local(self.sym_ty(sym), LocalKind::User, Some(s.name.clone()), Some(sym), mutable, span);
        self.vars.insert(sym, l);
        self.push(StatementKind::StorageLive(l), span);
        if let Some(scope) = self.scopes.last_mut() {
            scope.push(l);
        }
        l
    }

    // ------------------------------------------------------------ scopes and drops

    fn drop_locals(&mut self, locals: &[LocalId], span: Span) {
        let mut order: Vec<_> = locals.iter().rev().copied().collect();
        if !self.task_scopes.is_empty() {
            order.sort_by_key(|l| !self.lx.t.decls.contains_task(&self.f.local(*l).ty));
        }
        for l in order {
            if self.needs_drop(&self.f.locals[l.0 as usize].ty.clone()) {
                self.push(StatementKind::Drop(Place::local(l)), span);
            }
            if self.f.locals[l.0 as usize].kind == LocalKind::User || self.f.locals[l.0 as usize].kind == LocalKind::TaskScopeWitness {
                self.push(StatementKind::StorageDead(l), span);
            }
        }
    }

    fn pop_scope(&mut self, span: Span) {
        if !self.dead && self.task_scopes.contains(&(self.scopes.len() - 1)) {
            self.join_scope(&self.scopes.last().cloned().unwrap_or_default(), span);
        }
        let locals = self.scopes.pop().unwrap_or_default();
        if !self.dead {
            self.drop_locals(&locals, span);
        }
    }

    /// Drops for leaving every scope deeper than `depth` (and all pending
    /// statement temporaries), without popping them: `return`, `break`, `try`.
    fn exit_to(&mut self, depth: usize, span: Span) {
        let temps: Vec<LocalId> = self.stmt_temps.iter().rev().flatten().copied().collect();
        for t in temps {
            self.push(StatementKind::Drop(Place::local(t)), span);
        }
        let scopes: Vec<Vec<LocalId>> = self.scopes[depth..].iter().rev().cloned().collect();
        for (offset, s) in scopes.into_iter().enumerate() {
            let level = self.scopes.len() - 1 - offset;
            if self.task_scopes.contains(&level) {
                self.join_scope(&s, span);
            }
            self.drop_locals(&s, span);
        }
    }

    fn join_scope(&mut self, locals: &[LocalId], span: Span) {
        // Scope-owned task values (including discarded expression temporaries)
        // are the completion obligations. A scope witness prevents their escape.
        // Destruction waits before any borrowed storage can end. Later abstract
        // drops are dead sites removed by ordinary drop elaboration.
        for &local in locals.iter().rev() {
            if self.lx.t.decls.contains_task(&self.f.local(local).ty) && self.needs_drop(&self.f.local(local).ty) {
                self.push(StatementKind::Drop(Place::local(local)), span);
            }
        }
        let t = self.temp(Ty::Void, span);
        let next = self.new_block();
        self.terminate(
            Terminator::Call {
                callee: Callee::Builtin(Builtin::JoinScope),
                args: Vec::new(),
                arg_spans: Vec::new(),
                dest: Place::local(t),
                next: Some(next),
                spawn: false,
            },
            span,
        );
        self.switch_to(next);
    }

    fn emit_return(&mut self, span: Span) {
        self.exit_to(0, span);
        self.terminate(Terminator::Return, span);
    }

    // ------------------------------------------------------------ functions

    fn function(&mut self, f: &ast::FnDecl) {
        let sym = self.def(f.id);
        self.f.symbol = sym;
        let sig = sym.and_then(|s| self.lx.t.decls.fns.get(&s));
        if let Some(sig) = sig {
            self.f.ret = sig.ret.clone();
            self.f.generics = sig.generics.clone();
        }
        let ret = self.f.ret.clone();
        self.new_local(ret, LocalKind::Return, None, None, false, f.span);
        self.scopes.push(Vec::new());
        if let Some(r) = &f.receiver
            && let Some(s) = self.def(r.id)
        {
            let l = self.new_local(self.sym_ty(s), LocalKind::Param, Some("self".into()), Some(s), false, r.span);
            self.vars.insert(s, l);
            self.scopes[0].push(l);
        }
        for p in &f.params {
            if let Some(s) = self.def(p.id) {
                let l = self.new_local(self.sym_ty(s), LocalKind::Param, Some(p.name.name.clone()), Some(s), false, p.span);
                self.vars.insert(s, l);
                self.scopes[0].push(l);
            }
        }
        if f.is_async && f.body.is_some() {
            let waker = self.waker_ty();
            let l = self.new_local(waker, LocalKind::Param, Some("async waker".into()), None, false, f.span);
            self.f.asynchronous = Some(AsyncInfo { waker: l, frame: None });
        }
        self.f.param_count = self.f.locals.len() as u32 - 1;
        match &f.body {
            Some(body) => {
                if f.is_async {
                    // Lazy construction: the body starts at the first poll.
                    // Abandoning the unstarted computation destroys its parameters.
                    self.suspend(f.span);
                }
                self.stmts(&body.stmts);
                self.fall_off_end(body.span);
            }
            None => {
                // Extern / intrinsic declaration: no blocks.
                self.f.blocks.clear();
                self.terminated.clear();
            }
        }
    }

    fn fall_off_end(&mut self, span: Span) {
        if self.dead {
            return;
        }
        let end = Span::new(span.file, span.end.saturating_sub(1), span.end);
        if matches!(self.f.ret, Ty::Void) {
            self.assign(Place::local(RETURN), Rvalue::Use(Operand::Const(Const::Unit)), end);
            self.emit_return(end);
        } else {
            // The type checker proved this point unreachable (E3009 otherwise).
            self.terminate(Terminator::Unreachable, end);
        }
    }

    fn finish(mut self) -> Function {
        prune(&mut self.f);
        self.f
    }

    // ------------------------------------------------------------ statements

    fn stmts(&mut self, stmts: &[Stmt]) {
        for s in stmts {
            if self.dead && !self.warned_dead && !matches!(s.kind, StmtKind::Error) {
                self.warned_dead = true;
                let mut d = Diagnostic::error("W3002", "unreachable_code", "unreachable code")
                    .primary(s.span, "this statement can never run")
                    .note("an earlier statement in this block always returns, breaks or panics");
                d.severity = Severity::Warning;
                self.diags.push(d);
            }
            self.stmt_temps.push(Vec::new());
            self.stmt(s);
            let temps = self.stmt_temps.pop().unwrap_or_default();
            if !self.dead {
                for t in temps.into_iter().rev() {
                    self.push(StatementKind::Drop(Place::local(t)), s.span);
                }
            }
        }
    }

    fn block(&mut self, b: &ast::Block) {
        self.scopes.push(Vec::new());
        self.stmts(&b.stmts);
        self.pop_scope(Span::new(b.span.file, b.span.end.saturating_sub(1), b.span.end));
    }

    fn stmt(&mut self, s: &Stmt) {
        let span = s.span;
        match &s.kind {
            StmtKind::Let { value: None, .. } => {
                // `var x: T`: storage only; the move/init checker tracks it.
                if let Some(sym) = self.def(s.id) {
                    self.declare_user(sym, span);
                }
            }
            StmtKind::Let { value: Some(value), .. } => {
                let Some(sym) = self.def(s.id) else { return };
                // Evaluate before declaring: the initializer cannot see the name.
                // `r := &make()` keeps the temporary alive as long as `r`'s
                // scope; a temporary only borrowed *inside* the initializer
                // (`n := len(&make())`) dies at the end of the statement.
                // A literal borrowed by coercion (`r: &string := "x"`) is the
                // same initializer as `r := &"x"` (Phase 26B).
                let borrowed_literal = self.tables().coercions.get(&value.id).is_some_and(|c| c.kind == CoercionKind::BorrowLiteral);
                self.extend_temps = borrowed_literal || matches!(value.kind, ExprKind::Unary { op: ast::UnaryOp::Ref | ast::UnaryOp::RefMut, .. });
                let op = self.operand(value);
                self.extend_temps = false;
                let l = self.declare_user(sym, span);
                self.assign(Place::local(l), Rvalue::Use(op), value.span);
            }
            StmtKind::Assign { target, value } => {
                let mut op = self.operand(value);
                let place = self.place(target);
                let ty = self.ty(target);
                if self.needs_drop(&ty) {
                    // Evaluate an overlapping owned RHS before destroying the
                    // destination (x = x, p.field = p.field). An Operand is
                    // deferred until Assign; merely lowering it is not a read.
                    if let Operand::Move(source) = &op
                        && source.local == place.local
                        && (source.proj.starts_with(&place.proj) || place.proj.starts_with(&source.proj))
                    {
                        let tmp = self.temp(ty.clone(), value.span);
                        self.assign(Place::local(tmp), Rvalue::Use(op), value.span);
                        op = Operand::Move(Place::local(tmp));
                    }
                    // The old value is dropped before being overwritten.
                    self.push(StatementKind::Drop(place.clone()), span);
                }
                self.assign(place, Rvalue::Use(op), span);
            }
            StmtKind::Expr(e) => {
                let ty = self.ty(e);
                let t = self.temp(ty, e.span);
                self.expr_into(e, Place::local(t));
            }
            StmtKind::Spawn(e) => {
                let t = self.temp(Ty::Void, e.span);
                if let ExprKind::Call { callee, args } = &e.kind {
                    self.call(e, callee, args, Place::local(t), true);
                }
            }
            StmtKind::Return(value) => {
                match value {
                    Some(v) => self.expr_into(v, Place::local(RETURN)),
                    None => self.assign(Place::local(RETURN), Rvalue::Use(Operand::Const(Const::Unit)), span),
                }
                if !self.dead {
                    self.emit_return(span);
                }
            }
            StmtKind::Break => {
                if let Some(l) = self.loops.last() {
                    let (to, depth) = (l.break_to, l.depth);
                    self.exit_to(depth, span);
                    self.goto(to, span);
                }
            }
            StmtKind::Continue => {
                if let Some(l) = self.loops.last() {
                    let (to, depth) = (l.continue_to, l.depth);
                    self.exit_to(depth, span);
                    self.goto(to, span);
                }
            }
            StmtKind::If(i) => self.if_stmt(i),
            StmtKind::For(f) => self.for_stmt(s, f),
            StmtKind::Match(m) => self.match_stmt(m, None),
            StmtKind::Block(b) | StmtKind::Unsafe(b) => self.block(b),
            StmtKind::Scope(b) => {
                self.task_scopes.push(self.scopes.len());
                self.scopes.push(Vec::new());
                let witness = self.new_local(Ty::Bool, LocalKind::TaskScopeWitness, Some("task scope".into()), None, false, b.span);
                self.push(StatementKind::StorageLive(witness), b.span);
                self.assign(Place::local(witness), Rvalue::Use(Operand::Const(Const::Bool(true))), b.span);
                self.scopes.last_mut().unwrap().push(witness);
                self.task_witnesses.push(witness);
                self.stmts(&b.stmts);
                self.pop_scope(b.span);
                self.task_witnesses.pop();
                self.task_scopes.pop();
            }
            StmtKind::Error => {}
        }
    }

    fn if_stmt(&mut self, i: &ast::IfStmt) {
        let cond = self.operand(&i.cond);
        let (then_bb, else_bb, join) = (self.new_block(), self.new_block(), self.new_block());
        self.terminate(Terminator::Switch { discr: cond, cases: vec![(0, else_bb)], otherwise: then_bb }, i.cond.span);
        self.switch_to(then_bb);
        self.block(&i.then_block);
        if !self.dead {
            self.goto(join, i.span);
        }
        self.switch_to(else_bb);
        match i.else_branch.as_deref() {
            Some(ElseBranch::If(e)) => self.if_stmt(e),
            Some(ElseBranch::Block(b)) => self.block(b),
            None => {}
        }
        if !self.dead {
            self.goto(join, i.span);
        }
        self.switch_to(join);
        // `join` is unreachable when every branch diverged.
        self.dead = !self.has_predecessor(join);
    }

    fn has_predecessor(&self, b: BlockId) -> bool {
        self.f.blocks.iter().enumerate().any(|(i, blk)| self.terminated[i] && blk.term.successors().contains(&b))
    }

    fn loop_body(&mut self, body: &ast::Block, continue_to: BlockId, break_to: BlockId, before: impl FnOnce(&mut Self)) {
        let depth = self.scopes.len();
        self.loops.push(LoopCx { continue_to, break_to, depth });
        self.scopes.push(Vec::new());
        before(self);
        self.stmts(&body.stmts);
        self.pop_scope(Span::new(body.span.file, body.span.end.saturating_sub(1), body.span.end));
        self.loops.pop();
        if !self.dead {
            self.goto(continue_to, body.span);
        }
    }

    fn for_stmt(&mut self, s: &Stmt, f: &ast::ForStmt) {
        let span = s.span;
        let exit = self.new_block();
        match &f.kind {
            ForKind::Infinite => {
                let head = self.new_block();
                self.goto(head, span);
                self.switch_to(head);
                self.loop_body(&f.body, head, exit, |_| {});
            }
            ForKind::While(c) => {
                let (head, body) = (self.new_block(), self.new_block());
                self.goto(head, span);
                self.switch_to(head);
                let cond = self.operand(c);
                self.terminate(Terminator::Switch { discr: cond, cases: vec![(0, exit)], otherwise: body }, c.span);
                self.switch_to(body);
                self.loop_body(&f.body, head, exit, |_| {});
            }
            ForKind::In { iter, .. } => {
                let sym = self.def(s.id);
                match &iter.kind {
                    ExprKind::Range { start, end, inclusive } => self.for_range(span, sym, start, end, *inclusive, &f.body, exit),
                    _ => self.for_elements(span, sym, s, iter, &f.body, exit),
                }
            }
        }
        self.switch_to(exit);
        self.dead = !self.has_predecessor(exit);
    }

    /// `for i in a..b`: counter loop; `a..=b` stops *at* `b` without
    /// computing `b + 1` (no overflow at the type's maximum).
    #[allow(clippy::too_many_arguments)]
    fn for_range(&mut self, span: Span, sym: Option<SymbolId>, start: &Option<Box<Expr>>, end: &Option<Box<Expr>>, inclusive: bool, body: &ast::Block, exit: BlockId) {
        let elem = sym.map(|s| self.sym_ty(s)).unwrap_or(Ty::Error);
        let (Some(start), Some(end)) = (start, end) else { return };
        let cur = self.new_local(elem.clone(), LocalKind::Temp, None, None, false, span);
        let lo = self.operand(start);
        self.assign(Place::local(cur), Rvalue::Use(lo), start.span);
        let hi_op = self.operand(end);
        let hi = self.new_local(elem.clone(), LocalKind::Temp, None, None, false, end.span);
        self.assign(Place::local(hi), Rvalue::Use(hi_op), end.span);
        let (head, body_bb, latch) = (self.new_block(), self.new_block(), self.new_block());
        self.goto(head, span);
        self.switch_to(head);
        let c = self.new_local(Ty::Bool, LocalKind::Temp, None, None, false, span);
        let op = if inclusive { BinOp::Le } else { BinOp::Lt };
        self.assign(Place::local(c), Rvalue::Binary(op, Operand::Copy(Place::local(cur)), Operand::Copy(Place::local(hi))), span);
        self.terminate(Terminator::Switch { discr: Operand::Copy(Place::local(c)), cases: vec![(0, exit)], otherwise: body_bb }, span);
        self.switch_to(body_bb);
        self.loop_body(body, latch, exit, |b| {
            if let Some(sym) = sym {
                let l = b.declare_user(sym, span);
                b.assign(Place::local(l), Rvalue::Use(Operand::Copy(Place::local(cur))), span);
            }
        });
        // latch: (inclusive: stop at the last value) cur = cur + 1
        self.switch_to(latch);
        if inclusive {
            let last = self.new_local(Ty::Bool, LocalKind::Temp, None, None, false, span);
            self.assign(Place::local(last), Rvalue::Binary(BinOp::Eq, Operand::Copy(Place::local(cur)), Operand::Copy(Place::local(hi))), span);
            let step = self.new_block();
            self.terminate(Terminator::Switch { discr: Operand::Copy(Place::local(last)), cases: vec![(0, step)], otherwise: exit }, span);
            self.switch_to(step);
        }
        let one = match &elem {
            Ty::Int(it) => Const::Int(1, *it),
            _ => Const::Int(1, tarn_types::IntTy::I64),
        };
        self.assign(Place::local(cur), Rvalue::Binary(BinOp::Add, Operand::Copy(Place::local(cur)), Operand::Const(one)), span);
        self.goto(head, span);
    }

    /// `for x in &xs` / `for x in arr`: index loop with ADR 0018 binding modes.
    fn for_elements(&mut self, span: Span, sym: Option<SymbolId>, s: &Stmt, iter: &Expr, body: &ast::Block, exit: BlockId) {
        let it_ty = self.ty(iter);
        let mode = self.tables().binding_modes.get(&s.id).copied().unwrap_or(BindingMode::Copy);
        if matches!(it_ty, Ty::Opaque | Ty::Error) {
            // Opaque std iterator: the loop shape is kept, the values are opaque.
            let _ = self.operand(iter);
            let (head, body_bb) = (self.new_block(), self.new_block());
            self.goto(head, span);
            self.switch_to(head);
            self.terminate(Terminator::Switch { discr: Operand::Const(Const::Opaque), cases: vec![(0, exit)], otherwise: body_bb }, span);
            self.switch_to(body_bb);
            self.loop_body(body, head, exit, |b| {
                if let Some(sym) = sym {
                    let l = b.declare_user(sym, span);
                    b.assign(Place::local(l), Rvalue::Use(Operand::Const(Const::Opaque)), span);
                }
            });
            return;
        }
        if let Some(elem_ty) = self.vec_elem(peel_ty(&it_ty)) {
            if matches!(it_ty, Ty::Ref(..)) {
                return self.for_vec_slice(span, sym, s, iter, &it_ty, elem_ty, body, exit);
            }
            return self.for_vec_owned(span, sym, iter, &it_ty, elem_ty, body, exit);
        }
        // Base place of the elements: `*it` through a reference, or the array
        // itself. Iterating an owned non-copy array consumes it: it is moved
        // into an `IterArray` local whose elements are then moved out by index.
        let owned_noncopy = !matches!(it_ty, Ty::Ref(..)) && self.needs_drop(&it_ty);
        let kind = if owned_noncopy { LocalKind::IterArray } else { LocalKind::Temp };
        let it = self.new_local(it_ty.clone(), kind, None, None, false, iter.span);
        let it_op = self.operand(iter);
        self.assign(Place::local(it), Rvalue::Use(it_op), iter.span);
        if owned_noncopy {
            self.scopes.last_mut().unwrap().push(it);
        }
        let mut base = Place::local(it);
        let mut element_base = &it_ty;
        while let Ty::Ref(_, inner) = element_base {
            base = base.project(Proj::Deref);
            element_base = inner;
        }
        let usize_ = Ty::Int(tarn_types::IntTy::Usize);
        let len = self.new_local(usize_.clone(), LocalKind::Temp, None, None, false, span);
        self.assign(Place::local(len), Rvalue::Len(base.clone()), span);
        let idx = self.new_local(usize_.clone(), LocalKind::Temp, None, None, false, span);
        self.assign(Place::local(idx), Rvalue::Use(Operand::Const(Const::Int(0, tarn_types::IntTy::Usize))), span);
        let (head, body_bb, latch) = (self.new_block(), self.new_block(), self.new_block());
        self.goto(head, span);
        self.switch_to(head);
        let c = self.new_local(Ty::Bool, LocalKind::Temp, None, None, false, span);
        self.assign(Place::local(c), Rvalue::Binary(BinOp::Lt, Operand::Copy(Place::local(idx)), Operand::Copy(Place::local(len))), span);
        self.terminate(Terminator::Switch { discr: Operand::Copy(Place::local(c)), cases: vec![(0, exit)], otherwise: body_bb }, span);
        self.switch_to(body_bb);
        let elem = base.project(Proj::Index(idx));
        self.loop_body(body, latch, exit, |b| {
            if let Some(sym) = sym {
                let l = b.declare_user(sym, span);
                let rv = match mode {
                    BindingMode::Copy => Rvalue::Use(Operand::Copy(elem.clone())),
                    BindingMode::Move => Rvalue::Use(Operand::Move(elem.clone())),
                    BindingMode::Ref(m) => Rvalue::Ref(m, elem.clone()),
                };
                b.assign(Place::local(l), rv, span);
            }
        });
        self.switch_to(latch);
        self.assign(Place::local(idx), Rvalue::Binary(BinOp::Add, Operand::Copy(Place::local(idx)), Operand::Const(Const::Int(1, tarn_types::IntTy::Usize))), span);
        self.goto(head, span);
    }

    fn vec_elem(&self, t: &Ty) -> Option<Ty> {
        match t {
            Ty::Adt(id, args) if Some(*id) == self.lx.t.decls.vec && args.len() == 1 => Some(args[0].clone()),
            _ => None,
        }
    }

    /// `for x in &v` / `for x in &mut v`: iterate `v.as_slice()` (or
    /// `as_mut_slice`) with the usual reference binding modes (ADR 0086).
    #[allow(clippy::too_many_arguments)]
    fn for_vec_slice(&mut self, span: Span, sym: Option<SymbolId>, s: &Stmt, iter: &Expr, it_ty: &Ty, elem: Ty, body: &ast::Block, exit: BlockId) {
        let mode = self.tables().binding_modes.get(&s.id).copied().unwrap_or(BindingMode::Copy);
        // Read through outer references (`&&v`) down to one `&Vec<T>`.
        let it = self.new_local(it_ty.clone(), LocalKind::Temp, None, None, false, iter.span);
        let op = self.operand(iter);
        self.assign(Place::local(it), Rvalue::Use(op), iter.span);
        let mut place = Place::local(it);
        let mut ty = it_ty.clone();
        while let Ty::Ref(_, inner) = &ty
            && matches!(**inner, Ty::Ref(..))
        {
            place = place.project(Proj::Deref);
            ty = (**inner).clone();
        }
        let mutable = matches!(ty, Ty::Ref(true, _));
        let exclusive = mutable && matches!(mode, BindingMode::Ref(true));
        let slice_ty = Ty::Ref(exclusive, Box::new(Ty::Slice(Box::new(elem))));
        let slice = self.new_local(slice_ty, LocalKind::Temp, None, None, false, iter.span);
        let name = if exclusive { "Vec.as_mut_slice" } else { "Vec.as_slice" };
        self.finish_call(Callee::Intrinsic(name.into()), vec![Operand::Copy(place)], vec![iter.span], Place::local(slice), false, false, iter.span);
        let base = Place::local(slice).project(Proj::Deref);
        self.index_loop(span, sym, base, mode, body, exit);
    }

    /// `for x in v`: `v` moves into an `IterVec` local; each iteration moves
    /// the element at the cursor out and advances the cursor before the body,
    /// so every exit drops exactly the elements not yet yielded (ADR 0086).
    #[allow(clippy::too_many_arguments)]
    fn for_vec_owned(&mut self, span: Span, sym: Option<SymbolId>, iter: &Expr, it_ty: &Ty, elem: Ty, body: &ast::Block, exit: BlockId) {
        let usize_ = Ty::Int(tarn_types::IntTy::Usize);
        // The cursor exists before the vector so its drop can always read it.
        let cursor = self.new_local(usize_.clone(), LocalKind::Temp, None, None, false, span);
        self.assign(Place::local(cursor), Rvalue::Use(Operand::Const(Const::Int(0, tarn_types::IntTy::Usize))), span);
        let it = self.new_local(it_ty.clone(), LocalKind::IterVec(cursor), None, None, false, iter.span);
        let it_op = self.operand(iter);
        self.assign(Place::local(it), Rvalue::Use(it_op), iter.span);
        self.scopes.last_mut().unwrap().push(it);
        let shared = self.new_local(Ty::Ref(false, Box::new(it_ty.clone())), LocalKind::Temp, None, None, false, span);
        self.assign(Place::local(shared), Rvalue::Ref(false, Place::local(it)), span);
        let len = self.new_local(usize_.clone(), LocalKind::Temp, None, None, false, span);
        self.finish_call(Callee::Intrinsic("Vec.len".into()), vec![Operand::Copy(Place::local(shared))], vec![span], Place::local(len), false, false, span);
        let (head, body_bb) = (self.new_block(), self.new_block());
        self.goto(head, span);
        self.switch_to(head);
        let c = self.new_local(Ty::Bool, LocalKind::Temp, None, None, false, span);
        self.assign(Place::local(c), Rvalue::Binary(BinOp::Lt, Operand::Copy(Place::local(cursor)), Operand::Copy(Place::local(len))), span);
        self.terminate(Terminator::Switch { discr: Operand::Copy(Place::local(c)), cases: vec![(0, exit)], otherwise: body_bb }, span);
        self.switch_to(body_bb);
        let item = self.new_local(elem, LocalKind::Temp, None, None, false, span);
        let exclusive = self.new_local(Ty::Ref(true, Box::new(it_ty.clone())), LocalKind::Temp, None, None, false, span);
        self.assign(Place::local(exclusive), Rvalue::Ref(true, Place::local(it)), span);
        self.finish_call(
            Callee::Intrinsic("Vec.iter_take".into()),
            vec![Operand::Copy(Place::local(exclusive)), Operand::Copy(Place::local(cursor))],
            vec![span, span],
            Place::local(item),
            false,
            false,
            span,
        );
        let one = Operand::Const(Const::Int(1, tarn_types::IntTy::Usize));
        self.assign(Place::local(cursor), Rvalue::Binary(BinOp::Add, Operand::Copy(Place::local(cursor)), one), span);
        self.loop_body(body, head, exit, |b| {
            if let Some(sym) = sym {
                let l = b.declare_user(sym, span);
                b.assign(Place::local(l), Rvalue::Use(Operand::Move(Place::local(item))), span);
            }
        });
    }

    /// Index loop over `base` (an array or slice place) binding each element
    /// with `mode`.
    fn index_loop(&mut self, span: Span, sym: Option<SymbolId>, base: Place, mode: BindingMode, body: &ast::Block, exit: BlockId) {
        let usize_ = Ty::Int(tarn_types::IntTy::Usize);
        let len = self.new_local(usize_.clone(), LocalKind::Temp, None, None, false, span);
        self.assign(Place::local(len), Rvalue::Len(base.clone()), span);
        let idx = self.new_local(usize_, LocalKind::Temp, None, None, false, span);
        self.assign(Place::local(idx), Rvalue::Use(Operand::Const(Const::Int(0, tarn_types::IntTy::Usize))), span);
        let (head, body_bb, latch) = (self.new_block(), self.new_block(), self.new_block());
        self.goto(head, span);
        self.switch_to(head);
        let c = self.new_local(Ty::Bool, LocalKind::Temp, None, None, false, span);
        self.assign(Place::local(c), Rvalue::Binary(BinOp::Lt, Operand::Copy(Place::local(idx)), Operand::Copy(Place::local(len))), span);
        self.terminate(Terminator::Switch { discr: Operand::Copy(Place::local(c)), cases: vec![(0, exit)], otherwise: body_bb }, span);
        self.switch_to(body_bb);
        let elem = base.project(Proj::Index(idx));
        self.loop_body(body, latch, exit, |b| {
            if let Some(sym) = sym {
                let l = b.declare_user(sym, span);
                let rv = match mode {
                    BindingMode::Copy => Rvalue::Use(Operand::Copy(elem.clone())),
                    BindingMode::Move => Rvalue::Use(Operand::Move(elem.clone())),
                    BindingMode::Ref(m) => Rvalue::Ref(m, elem.clone()),
                };
                b.assign(Place::local(l), rv, span);
            }
        });
        self.switch_to(latch);
        self.assign(Place::local(idx), Rvalue::Binary(BinOp::Add, Operand::Copy(Place::local(idx)), Operand::Const(Const::Int(1, tarn_types::IntTy::Usize))), span);
        self.goto(head, span);
    }

    // ------------------------------------------------------------ match

    /// A `match` statement, or with `dest` a `match` expression whose value
    /// arms store into `dest` (ADR 0089).
    fn match_stmt(&mut self, m: &ast::MatchStmt, dest: Option<Place>) {
        let st = self.ty(&m.scrutinee);
        let scrut = self.place(&m.scrutinee);
        let join = self.new_block();
        let tests: Vec<BlockId> = m.arms.iter().map(|_| self.new_block()).collect();
        let fallthrough = self.new_block();
        self.goto(tests[0], m.scrutinee.span);
        for (i, arm) in m.arms.iter().enumerate() {
            let fail = tests.get(i + 1).copied().unwrap_or(fallthrough);
            self.switch_to(tests[i]);
            self.test(&arm.pattern, scrut.clone(), st.clone(), fail);
            self.scopes.push(Vec::new());
            self.bind(&arm.pattern, scrut.clone(), st.clone());
            if let Some(g) = &arm.guard {
                let c = self.operand(g);
                let body = self.new_block();
                // Guard failed: leave the arm scope (bindings dead) and try the next arm.
                let undo = self.new_block();
                self.terminate(Terminator::Switch { discr: c, cases: vec![(0, undo)], otherwise: body }, g.span);
                self.switch_to(undo);
                let locals = self.scopes.last().cloned().unwrap_or_default();
                self.drop_locals(&locals, g.span);
                self.goto(fail, g.span);
                self.switch_to(body);
            }
            match &arm.body.kind {
                StmtKind::Block(b) => self.stmts(&b.stmts),
                _ => {
                    self.stmt_temps.push(Vec::new());
                    match (&arm.body.kind, &dest) {
                        (StmtKind::Expr(value), Some(dest)) => self.expr_into(value, dest.clone()),
                        _ => self.stmt(&arm.body),
                    }
                    let temps = self.stmt_temps.pop().unwrap_or_default();
                    if !self.dead {
                        for t in temps.into_iter().rev() {
                            self.push(StatementKind::Drop(Place::local(t)), arm.span);
                        }
                    }
                }
            }
            self.pop_scope(arm.span);
            if !self.dead {
                self.goto(join, arm.span);
            }
        }
        // Exhaustiveness was proven by the type checker.
        self.switch_to(fallthrough);
        self.terminate(Terminator::Unreachable, m.scrutinee.span);
        self.switch_to(join);
        self.dead = !self.has_predecessor(join);
    }

    fn variant_index(&self, enum_sym: SymbolId, v: SymbolId) -> u32 {
        self.lx.t.decls.enums.get(&enum_sym).and_then(|d| d.variants.iter().position(|x| x.sym == v)).unwrap_or(0) as u32
    }

    fn variant_field_tys(&self, ty: &Ty, v: SymbolId) -> Vec<Ty> {
        let Ty::Adt(e, args) = ty else { return Vec::new() };
        let Some(def) = self.lx.t.decls.enums.get(e) else { return Vec::new() };
        let map: HashMap<_, _> = def.generics.iter().copied().zip(args.iter().cloned()).collect();
        def.variants.iter().find(|x| x.sym == v).map(|x| x.fields.iter().map(|f| tarn_types::subst(f, &map)).collect()).unwrap_or_default()
    }

    fn struct_field(&self, ty: &Ty, name: &str) -> Option<(u32, Ty)> {
        let Ty::Adt(s, args) = ty else { return None };
        let def = self.lx.t.decls.structs.get(s)?;
        let map: HashMap<_, _> = def.generics.iter().copied().zip(args.iter().cloned()).collect();
        let i = def.fields.iter().position(|f| f.name == name)?;
        Some((i as u32, tarn_types::subst(&def.fields[i].ty, &map)))
    }

    /// Emit the tests of `p` against `place`; continue in the current block
    /// on success, jump to `fail` otherwise. References are transparent.
    fn test(&mut self, p: &Pattern, mut place: Place, mut ty: Ty, fail: BlockId) {
        while let Ty::Ref(_, inner) = ty {
            place = place.project(Proj::Deref);
            ty = *inner;
        }
        let span = p.span;
        match &p.kind {
            PatternKind::Wildcard | PatternKind::Error => {}
            PatternKind::Ident(_) | PatternKind::Variant { .. } => {
                let Some(&v) = self.tables().pattern_variants.get(&p.id) else { return };
                let Ty::Adt(enum_sym, _) = &ty else { return };
                let idx = self.variant_index(*enum_sym, v);
                let d = self.new_local(Ty::Int(tarn_types::IntTy::U32), LocalKind::Temp, None, None, false, span);
                self.assign(Place::local(d), Rvalue::Discriminant(place.clone()), span);
                let ok = self.new_block();
                self.terminate(Terminator::Switch { discr: Operand::Copy(Place::local(d)), cases: vec![(idx as i128, ok)], otherwise: fail }, span);
                self.switch_to(ok);
                if let PatternKind::Variant { args, .. } = &p.kind {
                    let ftys = self.variant_field_tys(&ty, v);
                    for (i, (a, ft)) in args.iter().zip(ftys).enumerate() {
                        let sub = place.project(Proj::Downcast(idx)).project(Proj::Field(i as u32));
                        self.test(a, sub, ft, fail);
                    }
                }
            }
            PatternKind::Literal(e) => {
                if let ExprKind::Bool(b) = e.kind {
                    let ok = self.new_block();
                    self.terminate(Terminator::Switch { discr: Operand::Copy(place), cases: vec![(b as i128, ok)], otherwise: fail }, span);
                    self.switch_to(ok);
                } else {
                    let c = self.compare(BinOp::Eq, place, &ty, e, span);
                    self.branch_on(c, fail, span);
                }
            }
            PatternKind::Range { start, end, inclusive } => {
                let lo = self.compare(BinOp::Ge, place.clone(), &ty, start, span);
                self.branch_on(lo, fail, span);
                let hi = self.compare(if *inclusive { BinOp::Le } else { BinOp::Lt }, place, &ty, end, span);
                self.branch_on(hi, fail, span);
            }
            PatternKind::Struct { fields, .. } => {
                for fp in fields {
                    if let (Some(sub), Some((i, ft))) = (&fp.pattern, self.struct_field(&ty, &fp.name.name)) {
                        self.test(sub, place.project(Proj::Field(i)), ft, fail);
                    }
                }
            }
        }
    }

    /// `place <op> literal` as a bool temp (strings compare through an intrinsic).
    fn compare(&mut self, op: BinOp, place: Place, ty: &Ty, lit: &Expr, span: Span) -> LocalId {
        let c = self.new_local(Ty::Bool, LocalKind::Temp, None, None, false, span);
        let rhs = self.operand(lit);
        if matches!(ty, Ty::Str) {
            let lhs_ref = self.new_local(Ty::Ref(false, Box::new(Ty::Str)), LocalKind::Temp, None, None, false, span);
            self.assign(Place::local(lhs_ref), Rvalue::Ref(false, place), span);
            let rhs_t = self.new_local(Ty::Str, LocalKind::Temp, None, None, false, span);
            self.assign(Place::local(rhs_t), Rvalue::Use(rhs), span);
            let rhs_ref = self.new_local(Ty::Ref(false, Box::new(Ty::Str)), LocalKind::Temp, None, None, false, span);
            self.assign(Place::local(rhs_ref), Rvalue::Ref(false, Place::local(rhs_t)), span);
            let next = self.new_block();
            self.terminate(
                Terminator::Call {
                    callee: Callee::Intrinsic(format!("string.{}", binop_name(op))),
                    args: vec![Operand::Copy(Place::local(lhs_ref)), Operand::Copy(Place::local(rhs_ref))],
                    arg_spans: vec![span, lit.span],
                    dest: Place::local(c),
                    next: Some(next),
                    spawn: false,
                },
                span,
            );
            self.switch_to(next);
            self.push(StatementKind::Drop(Place::local(rhs_t)), span);
        } else {
            self.assign(Place::local(c), Rvalue::Binary(op, Operand::Copy(place), rhs), span);
        }
        c
    }

    fn branch_on(&mut self, c: LocalId, fail: BlockId, span: Span) {
        let ok = self.new_block();
        self.terminate(Terminator::Switch { discr: Operand::Copy(Place::local(c)), cases: vec![(0, fail)], otherwise: ok }, span);
        self.switch_to(ok);
    }

    /// Create the pattern's bindings after all its tests succeeded, using the
    /// binding modes recorded by the type checker (ADR 0018).
    fn bind(&mut self, p: &Pattern, mut place: Place, mut ty: Ty) {
        while let Ty::Ref(_, inner) = ty {
            place = place.project(Proj::Deref);
            ty = *inner;
        }
        match &p.kind {
            PatternKind::Ident(_) => {
                if self.tables().pattern_variants.contains_key(&p.id) {
                    return;
                }
                if let Some(sym) = self.def(p.id) {
                    self.bind_one(p.id, sym, place, p.span);
                }
            }
            PatternKind::Variant { args, .. } => {
                let Some(&v) = self.tables().pattern_variants.get(&p.id) else { return };
                let Ty::Adt(enum_sym, _) = &ty else { return };
                let idx = self.variant_index(*enum_sym, v);
                let ftys = self.variant_field_tys(&ty, v);
                for (i, (a, ft)) in args.iter().zip(ftys).enumerate() {
                    self.bind(a, place.project(Proj::Downcast(idx)).project(Proj::Field(i as u32)), ft);
                }
            }
            PatternKind::Struct { fields, .. } => {
                for fp in fields {
                    let Some((i, ft)) = self.struct_field(&ty, &fp.name.name) else { continue };
                    let sub = place.project(Proj::Field(i));
                    match &fp.pattern {
                        Some(sp) => self.bind(sp, sub, ft),
                        None => {
                            if let Some(sym) = self.def(fp.id) {
                                self.bind_one(fp.id, sym, sub, fp.span);
                            }
                        }
                    }
                }
            }
            PatternKind::Wildcard | PatternKind::Literal(_) | PatternKind::Range { .. } | PatternKind::Error => {}
        }
    }

    fn bind_one(&mut self, node: ast::NodeId, sym: SymbolId, place: Place, span: Span) {
        let mode = self.tables().binding_modes.get(&node).copied().unwrap_or(BindingMode::Copy);
        let l = self.declare_user(sym, span);
        let rv = match mode {
            BindingMode::Copy => Rvalue::Use(Operand::Copy(place)),
            BindingMode::Move => Rvalue::Use(Operand::Move(place)),
            BindingMode::Ref(m) => Rvalue::Ref(m, place),
        };
        self.assign(Place::local(l), rv, span);
    }

    // ------------------------------------------------------------ places and operands

    /// Place of a local symbol (a capture is reached through its reference).
    fn symbol_place(&self, s: SymbolId) -> Option<Place> {
        if let Some(&l) = self.vars.get(&s) {
            return Some(Place::local(l));
        }
        self.captures.get(&s).map(|&l| Place::local(l).project(Proj::Deref))
    }

    /// Is `e` a place expression (named storage), as opposed to a value?
    fn is_place(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Ident(_) => {
                matches!(self.res(e.id), Some(Res::Symbol(s)) if self.symbol_place(*s).is_some())
            }
            ExprKind::Field { .. } => self.res(e.id).is_none(),
            ExprKind::Index { index, .. } => !matches!(index.kind, ExprKind::Range { .. }),
            ExprKind::Paren(inner) => self.is_place(inner),
            ExprKind::Unary { op: ast::UnaryOp::Deref, .. } => true,
            _ => false,
        }
    }

    /// The place `e` denotes; values are first stored in a temporary.
    fn place(&mut self, e: &Expr) -> Place {
        match &e.kind {
            ExprKind::Paren(inner) => self.place(inner),
            ExprKind::Ident(_) if self.is_place(e) => {
                let Some(Res::Symbol(s)) = self.res(e.id) else { unreachable!() };
                self.symbol_place(*s).unwrap()
            }
            ExprKind::Field { base, name } if self.res(e.id).is_none() => {
                let (mut p, mut bt) = (self.place(base), self.ty(base));
                while let Ty::Ref(_, inner) = bt {
                    p = p.project(Proj::Deref);
                    bt = *inner;
                }
                match self.struct_field(&bt, &name.name) {
                    Some((i, _)) => p.project(Proj::Field(i)),
                    None => p,
                }
            }
            ExprKind::Unary { op: ast::UnaryOp::Deref, operand } => self.place(operand).project(Proj::Deref),
            ExprKind::Index { base, index } if !matches!(index.kind, ExprKind::Range { .. }) => {
                let (mut p, mut bt) = (self.place(base), self.ty(base));
                while let Ty::Ref(_, inner) = bt {
                    p = p.project(Proj::Deref);
                    bt = *inner;
                }
                let iop = self.operand(index);
                let i = self.new_local(Ty::Int(tarn_types::IntTy::Usize), LocalKind::Temp, None, None, false, index.span);
                self.assign(Place::local(i), Rvalue::Use(iop), index.span);
                p.project(Proj::Index(i))
            }
            _ => {
                let t = self.temp(self.ty(e), e.span);
                self.expr_into(e, Place::local(t));
                Place::local(t)
            }
        }
    }

    fn read(&self, place: Place, ty: &Ty) -> Operand {
        if self.is_copy(ty) { Operand::Copy(place) } else { Operand::Move(place) }
    }

    /// A `&mut T` place used where a reference is expected is *reborrowed*
    /// (`&*p` / `&mut *p`) instead of moved, so it stays usable afterwards.
    fn reborrow(&mut self, e: &Expr, mutable: bool) -> Option<Operand> {
        let Ty::Ref(true, inner) = self.ty(e) else { return None };
        if !self.is_place(e) {
            return None;
        }
        let p = self.place(e).project(Proj::Deref);
        Some(self.ref_temp(mutable, p, &inner, e.span))
    }

    /// Argument of a call: like `operand`, but a `&mut` place is reborrowed.
    fn arg(&mut self, e: &Expr) -> Operand {
        if !self.tables().coercions.contains_key(&e.id)
            && let Some(op) = self.reborrow(e, true)
        {
            return op;
        }
        self.operand(e)
    }

    /// The value of `e` as an operand, with the checker's coercion applied.
    fn operand(&mut self, e: &Expr) -> Operand {
        let Some(c) = self.tables().coercions.get(&e.id).copied() else { return self.operand_raw(e) };
        // `&mut T → &T` of a place: a shared reborrow, not a conversion.
        if c.mut_to_shared
            && c.kind == CoercionKind::None
            && let Some(op) = self.reborrow(e, false)
        {
            return op;
        }
        if c.kind == CoercionKind::BorrowLiteral {
            let target = Ty::Ref(false, Box::new(Ty::Str));
            let rv = self.borrow(e, e);
            let t = self.new_local(target.clone(), LocalKind::Temp, None, None, false, e.span);
            self.assign(Place::local(t), rv, e.span);
            return self.read(Place::local(t), &target);
        }
        let op = self.operand_raw(e);
        if c.kind == CoercionKind::Poller {
            let Ty::Async(output) = self.ty(e) else { return op };
            let (Some(waker), Some(progress)) = (self.lx.t.decls.exec_waker, self.lx.t.decls.exec_progress) else { return op };
            let target = Ty::Fn(tarn_types::CallMode::Mutable, vec![Ty::Ref(false, Box::new(Ty::Adt(waker, Vec::new())))], Box::new(Ty::Adt(progress, vec![*output])));
            let t = self.new_local(target.clone(), LocalKind::Temp, None, None, false, e.span);
            self.assign(Place::local(t), Rvalue::Coerce(CoerceKind::Poller, op, target.clone()), e.span);
            return self.read(Place::local(t), &target);
        }
        if let Ty::Ptr(true, inner) = self.ty(e) {
            let target = Ty::Ptr(false, inner);
            let t = self.new_local(target.clone(), LocalKind::Temp, None, None, false, e.span);
            self.assign(Place::local(t), Rvalue::Coerce(CoerceKind::MutToShared, op, target.clone()), e.span);
            return self.read(Place::local(t), &target);
        }
        let Ty::Ref(m, inner) = self.ty(e) else { return op };
        let shared = m && !c.mut_to_shared;
        let (kind, target) = match (c.kind, *inner) {
            (CoercionKind::Unsize, Ty::Array(el, _)) => (CoerceKind::Unsize, Ty::Ref(shared, Box::new(Ty::Slice(el)))),
            (CoercionKind::ToDyn(i), _) => (CoerceKind::ToDyn(i), Ty::Ref(shared, Box::new(Ty::Any(i)))),
            (_, inner) => (CoerceKind::MutToShared, Ty::Ref(false, Box::new(inner))),
        };
        let t = self.new_local(target.clone(), LocalKind::Temp, None, None, false, e.span);
        self.assign(Place::local(t), Rvalue::Coerce(kind, op, target.clone()), e.span);
        self.read(Place::local(t), &target)
    }

    /// The value of a `const` item named by `e` (ADR 0090), as an immediate.
    fn constant(&self, e: &Expr) -> Option<Const> {
        let Some(Res::Symbol(s)) = self.res(e.id) else { return None };
        let def = self.lx.t.decls.consts.get(s)?;
        Some(match (&def.value, &def.ty) {
            (ConstValue::Int(v), ty) => int_const(*v, ty),
            (ConstValue::Float(text), Ty::Float(f)) => Const::Float(text.clone(), *f),
            (ConstValue::Bool(b), _) => Const::Bool(*b),
            (ConstValue::Str(text), _) => Const::Str(text.clone()),
            (ConstValue::Float(_), _) => return None,
        })
    }

    /// A `core.Location` value naming `span`, for an omitted argument.
    fn caller_location(&mut self, span: Span) -> Operand {
        let Some(location) = self.lx.t.decls.location else { return Operand::Const(Const::Opaque) };
        let ty = Ty::Adt(location, Vec::new());
        let t = self.new_local(ty.clone(), LocalKind::Temp, None, None, false, span);
        let text = Operand::Const(Const::Str(self.lx.location(span)));
        self.assign(Place::local(t), Rvalue::Aggregate(Aggregate::Struct(location, Vec::new()), vec![text]), span);
        self.read(Place::local(t), &ty)
    }

    fn operand_raw(&mut self, e: &Expr) -> Operand {
        let ty = self.ty(e);
        match &e.kind {
            ExprKind::Int(v) => Operand::Const(int_const(*v as i128, &ty)),
            ExprKind::Float(s) => Operand::Const(Const::Float(
                s.clone(),
                match ty {
                    Ty::Float(f) => f,
                    _ => tarn_types::FloatTy::F64,
                },
            )),
            ExprKind::Str(s) => Operand::Const(Const::Str(s.clone())),
            ExprKind::Bool(b) => Operand::Const(Const::Bool(*b)),
            ExprKind::Unit => Operand::Const(Const::Unit),
            ExprKind::Unary { op: ast::UnaryOp::Neg, operand } if matches!(operand.kind, ExprKind::Int(_)) => {
                let ExprKind::Int(v) = operand.kind else { unreachable!() };
                Operand::Const(int_const(-(v as i128), &ty))
            }
            ExprKind::Paren(inner) => self.operand(inner),
            ExprKind::Ident(_) | ExprKind::Field { .. } if let Some(c) = self.constant(e) => Operand::Const(c),
            _ if self.is_place(e) => {
                let p = self.place(e);
                self.read(p, &ty)
            }
            _ => {
                if matches!(ty, Ty::Opaque) && matches!(e.kind, ExprKind::Ident(_) | ExprKind::Field { .. }) {
                    return Operand::Const(Const::Opaque);
                }
                let t = self.temp(ty.clone(), e.span);
                self.expr_into_raw(e, Place::local(t));
                self.read(Place::local(t), &ty)
            }
        }
    }

    /// Store the value of `e` into `dest`, with the checker's coercion applied.
    fn expr_into(&mut self, e: &Expr, dest: Place) {
        if self.tables().coercions.contains_key(&e.id) {
            let op = self.operand(e);
            self.assign(dest, Rvalue::Use(op), e.span);
        } else {
            self.expr_into_raw(e, dest);
        }
    }

    /// Store the uncoerced value of `e` into `dest`. Never consults
    /// coercions, so `operand` → `operand_raw` → here cannot loop.
    fn expr_into_raw(&mut self, e: &Expr, dest: Place) {
        let span = e.span;
        let ty = self.ty(e);
        match &e.kind {
            ExprKind::Unary { op: ast::UnaryOp::Spawn, operand } => self.spawn_task(e, operand, dest),
            ExprKind::Call { callee, args } => self.call(e, callee, args, dest, false),
            ExprKind::Binary { op, lhs, rhs } => self.binary(e, *op, lhs, rhs, dest),
            ExprKind::Interpolated(parts) => self.interpolate(parts, dest, span),
            ExprKind::Unary { op: ast::UnaryOp::Ref | ast::UnaryOp::RefMut, operand } => {
                let rv = self.borrow(e, operand);
                self.assign(dest, rv, span);
            }
            ExprKind::Unary { op: ast::UnaryOp::Deref, .. } => {
                let place = self.place(e);
                let o = self.read(place, &ty);
                self.assign(dest, Rvalue::Use(o), span);
            }
            ExprKind::Unary { op, operand } if !matches!(operand.kind, ExprKind::Int(_)) => {
                let o = self.operand(operand);
                let rv = match op {
                    ast::UnaryOp::Neg => Rvalue::Unary(UnOp::Neg, o),
                    ast::UnaryOp::Not => Rvalue::Unary(UnOp::Not, o),
                    _ => unreachable!(),
                };
                self.assign(dest, rv, span);
            }
            ExprKind::Try(inner) => self.try_into(e, inner, dest),
            ExprKind::Await(inner) => self.await_into(e, inner, dest),
            ExprKind::StructLit { fields, .. } => {
                let Ty::Adt(s, targs) = &ty else {
                    self.assign(dest, Rvalue::Use(Operand::Const(Const::Opaque)), span);
                    return;
                };
                let Some(def) = self.lx.t.decls.structs.get(s) else { return };
                // Operands in the order the fields are written (evaluation
                // order), then assembled in declaration order.
                let mut by_name: HashMap<&str, Operand> = HashMap::new();
                for f in fields {
                    let op = match &f.value {
                        Some(v) => self.operand(v),
                        None => match self.res(f.id) {
                            Some(Res::Symbol(sym)) => {
                                let p = self.symbol_place(*sym).unwrap_or(Place::local(RETURN));
                                self.read(p, &self.sym_ty(*sym))
                            }
                            _ => Operand::Const(Const::Opaque),
                        },
                    };
                    by_name.insert(&f.name.name, op);
                }
                let ops = def.fields.iter().map(|f| by_name.remove(f.name.as_str()).unwrap_or(Operand::Const(Const::Opaque))).collect();
                self.assign(dest, Rvalue::Aggregate(Aggregate::Struct(*s, targs.clone()), ops), span);
            }
            ExprKind::ArrayLit { elems, .. } => {
                let elem = match &ty {
                    Ty::Array(el, _) => (**el).clone(),
                    _ => Ty::Error,
                };
                let ops = elems.iter().map(|x| self.operand(x)).collect();
                self.assign(dest, Rvalue::Aggregate(Aggregate::Array(elem), ops), span);
            }
            ExprKind::Closure { params, body, .. } => self.closure(e, params, body, dest),
            ExprKind::Match(m) => self.match_stmt(m, Some(dest)),
            ExprKind::Ident(_) | ExprKind::Field { .. } if !self.is_place(e) => {
                let rv = self.static_value(e, &ty);
                self.assign(dest, rv, span);
            }
            // An unsized slice value outside `&` has no representation.
            ExprKind::Index { index, .. } if matches!(index.kind, ExprKind::Range { .. }) => {
                self.assign(dest, Rvalue::Use(Operand::Const(Const::Opaque)), span);
            }
            // Constants, places, parenthesized expressions: no recursion back here.
            _ => {
                let op = self.operand_raw(e);
                self.assign(dest, Rvalue::Use(op), span);
            }
        }
    }

    /// A payload variant used as a function value (Phase 23B): a generated
    /// capture-free function that builds the variant from its parameters,
    /// referenced like a capture-free closure. Types are those of this use, so
    /// it shares this function's generics.
    fn variant_constructor(&mut self, parent: SymbolId, variant: SymbolId, params: &[Ty], result: &Ty, span: Span) -> Rvalue {
        let id = FunctionId(self.lx.next_id.get());
        self.lx.next_id.set(id.0 + 1);
        let name = format!("{}.{}::constructor", self.lx.r.symbol(parent).name, self.lx.r.symbol(variant).name);
        let mut vb = Builder::new(self.lx, self.m, id, name, span);
        vb.f.generics = self.f.generics.clone();
        vb.f.ret = result.clone();
        vb.new_local(result.clone(), LocalKind::Return, None, None, false, span);
        vb.scopes.push(Vec::new());
        let mut fields = Vec::new();
        for ty in params {
            let l = vb.new_local(ty.clone(), LocalKind::Param, None, None, false, span);
            fields.push(vb.read(Place::local(l), ty));
        }
        vb.f.param_count = params.len() as u32;
        let targs = match result {
            Ty::Adt(_, a) => a.clone(),
            _ => Vec::new(),
        };
        let index = vb.variant_index(parent, variant);
        vb.assign(Place::local(RETURN), Rvalue::Aggregate(Aggregate::Variant(parent, index, targs), fields), span);
        vb.emit_return(span);
        self.diags.append(&mut vb.diags);
        let f = vb.finish();
        self.lx.extra.borrow_mut().push(f);
        Rvalue::Use(Operand::Const(Const::Fn(id, self.f.generics.iter().copied().map(Ty::Param).collect())))
    }

    /// A name that is not a place: unit variant, function value, std member.
    fn static_value(&mut self, e: &Expr, ty: &Ty) -> Rvalue {
        if let Some(c) = self.constant(e) {
            return Rvalue::Use(Operand::Const(c));
        }
        match self.res(e.id) {
            Some(Res::Symbol(s)) => match &self.lx.r.symbol(*s).kind {
                SymbolKind::Variant { parent } => {
                    if let Ty::Fn(_, params, result) = ty {
                        return self.variant_constructor(*parent, *s, params, result, e.span);
                    }
                    let targs = match ty {
                        Ty::Adt(_, a) => a.clone(),
                        _ => Vec::new(),
                    };
                    Rvalue::Aggregate(Aggregate::Variant(*parent, self.variant_index(*parent, *s), targs), Vec::new())
                }
                SymbolKind::Function | SymbolKind::Method { .. } | SymbolKind::ImplMethod { .. } => match self.lx.by_symbol.get(s) {
                    Some(id) => Rvalue::Use(Operand::Const(Const::Fn(*id, self.tables().type_args.get(&e.id).cloned().unwrap_or_default()))),
                    None => Rvalue::Use(Operand::Const(Const::Opaque)),
                },
                _ => Rvalue::Use(Operand::Const(Const::Opaque)),
            },
            _ => Rvalue::Use(Operand::Const(Const::Opaque)),
        }
    }

    /// `&e` / `&mut e`. Borrowing a value (not a place) borrows a temporary;
    /// in a `let` initializer that temporary lives until the end of the scope.
    fn borrow(&mut self, e: &Expr, operand: &Expr) -> Rvalue {
        let mutable = matches!(e.kind, ExprKind::Unary { op: ast::UnaryOp::RefMut, .. });
        if let ExprKind::Index { base, index } = &operand.kind
            && let ExprKind::Range { start, end, .. } = &index.kind
        {
            let (mut p, mut bt) = (self.place(base), self.ty(base));
            while let Ty::Ref(_, inner) = bt {
                p = p.project(Proj::Deref);
                bt = *inner;
            }
            let start = start.as_ref().map(|s| self.operand(s));
            let end = end.as_ref().map(|s| self.operand(s));
            return Rvalue::SliceRef { mutable, base: p, start, end };
        }
        if self.is_place(operand) {
            let p = self.place(operand);
            return Rvalue::Ref(mutable, p);
        }
        let t = self.new_local(self.ty(operand), LocalKind::Temp, None, None, false, operand.span);
        let extend = std::mem::take(&mut self.extend_temps);
        if self.needs_drop(&self.ty(operand)) {
            if extend {
                if let Some(s) = self.scopes.last_mut() {
                    s.push(t);
                }
            } else if let Some(ts) = self.stmt_temps.last_mut() {
                ts.push(t);
            }
        }
        // An auto-borrowed literal (BorrowLiteral) is its own operand; storing
        // it raw avoids applying the same coercion again.
        if std::ptr::eq(e, operand) {
            self.expr_into_raw(operand, Place::local(t));
        } else {
            self.expr_into(operand, Place::local(t));
        }
        Rvalue::Ref(mutable, Place::local(t))
    }

    fn binary(&mut self, e: &Expr, op: ast::BinaryOp, lhs: &Expr, rhs: &Expr, dest: Place) {
        use ast::BinaryOp as B;
        let span = e.span;
        if matches!(op, B::And | B::Or) {
            // Short-circuit: `a && b` = if a { b } else { false }.
            let l = self.operand(lhs);
            let (eval_rhs, short, join) = (self.new_block(), self.new_block(), self.new_block());
            let (on_false, on_true) = if op == B::And { (short, eval_rhs) } else { (eval_rhs, short) };
            self.terminate(Terminator::Switch { discr: l, cases: vec![(0, on_false)], otherwise: on_true }, lhs.span);
            self.switch_to(short);
            self.assign(dest.clone(), Rvalue::Use(Operand::Const(Const::Bool(op == B::Or))), span);
            self.goto(join, span);
            self.switch_to(eval_rhs);
            let r = self.operand(rhs);
            self.assign(dest, Rvalue::Use(r), span);
            self.goto(join, span);
            self.switch_to(join);
            return;
        }
        let bop = match op {
            B::Add => BinOp::Add,
            B::Sub => BinOp::Sub,
            B::Mul => BinOp::Mul,
            B::Div => BinOp::Div,
            B::Rem => BinOp::Rem,
            B::BitAnd => BinOp::BitAnd,
            B::BitOr => BinOp::BitOr,
            B::BitXor => BinOp::BitXor,
            B::Shl => BinOp::Shl,
            B::Shr => BinOp::Shr,
            B::Eq => BinOp::Eq,
            B::Ne => BinOp::Ne,
            B::Lt => BinOp::Lt,
            B::Le => BinOp::Le,
            B::Gt => BinOp::Gt,
            B::Ge => BinOp::Ge,
            B::And | B::Or => unreachable!(),
        };
        let lt = self.ty(lhs);
        if let Some(method) = self.tables().eq_ops.get(&e.id).copied() {
            // `a == b` is `Eq.eq(&a, &b)`; `a != b` negates it (Phase 26C).
            let a = self.value_ref(lhs);
            let b = self.value_ref(rhs);
            let result = if bop == BinOp::Ne { Place::local(self.new_local(Ty::Bool, LocalKind::Temp, None, None, false, span)) } else { dest.clone() };
            let next = self.new_block();
            self.terminate(
                Terminator::Call {
                    callee: Callee::Virtual { method, type_args: Vec::new() },
                    args: vec![a, b],
                    arg_spans: vec![lhs.span, rhs.span],
                    dest: result.clone(),
                    next: Some(next),
                    spawn: false,
                },
                span,
            );
            self.switch_to(next);
            if bop == BinOp::Ne {
                self.assign(dest, Rvalue::Unary(UnOp::Not, Operand::Copy(result)), span);
            }
            return;
        }
        if matches!(peel_ty(&lt), Ty::Str) {
            // String operators borrow their operands: `string.add(&a, &b)`.
            let a = self.str_ref(lhs);
            let b = self.str_ref(rhs);
            let next = self.new_block();
            self.terminate(
                Terminator::Call {
                    callee: Callee::Intrinsic(format!("string.{}", binop_name(bop))),
                    args: vec![a, b],
                    arg_spans: vec![lhs.span, rhs.span],
                    dest,
                    next: Some(next),
                    spawn: false,
                },
                span,
            );
            self.switch_to(next);
            return;
        }
        let a = self.operand(lhs);
        let b = self.operand(rhs);
        self.assign(dest, Rvalue::Binary(bop, a, b), span);
    }

    /// `&string` operand for a string-typed expression (borrowing places).
    /// A shared reference to the value of `e` (an `Eq` operand): references
    /// are passed through, other values are borrowed in place.
    fn value_ref(&mut self, e: &Expr) -> Operand {
        let ty = self.ty(e);
        if matches!(ty, Ty::Ref(..)) {
            return self.operand(e);
        }
        let p = self.place(e);
        let t = self.new_local(Ty::Ref(false, Box::new(ty)), LocalKind::Temp, None, None, false, e.span);
        self.assign(Place::local(t), Rvalue::Ref(false, p), e.span);
        Operand::Copy(Place::local(t))
    }

    fn str_ref(&mut self, e: &Expr) -> Operand {
        if matches!(self.ty(e), Ty::Ref(..)) {
            return self.operand(e);
        }
        let p = self.place(e);
        let t = self.new_local(Ty::Ref(false, Box::new(Ty::Str)), LocalKind::Temp, None, None, false, e.span);
        self.assign(Place::local(t), Rvalue::Ref(false, p), e.span);
        Operand::Copy(Place::local(t))
    }

    /// `try inner`: on `Err(e)`/`None` return early (with drops), else unwrap.
    fn try_into(&mut self, e: &Expr, inner: &Expr, dest: Place) {
        let span = e.span;
        let it = self.ty(inner);
        let Ty::Adt(family, args) = &it else {
            let _ = self.operand(inner);
            self.assign(dest, Rvalue::Use(Operand::Const(Const::Opaque)), span);
            return;
        };
        let p = &self.lx.t.prelude;
        let is_result = *family == p.result;
        let (ok_name, err_name) = if is_result { ("Ok", "Err") } else { ("Some", "None") };
        let def = &self.lx.t.decls.enums[family];
        let idx = |n: &str| def.variants.iter().position(|v| v.name == n).unwrap_or(0) as u32;
        let (ok, err) = (idx(ok_name), idx(err_name));
        let val = self.place(inner);
        let d = self.new_local(Ty::Int(tarn_types::IntTy::U32), LocalKind::Temp, None, None, false, span);
        self.assign(Place::local(d), Rvalue::Discriminant(val.clone()), span);
        let (ok_bb, err_bb) = (self.new_block(), self.new_block());
        self.terminate(Terminator::Switch { discr: Operand::Copy(Place::local(d)), cases: vec![(ok as i128, ok_bb)], otherwise: err_bb }, span);
        // Early return with the error (or None), converted to the function's
        // own Result/Option type (same error type: ADR/E3017).
        self.switch_to(err_bb);
        let ret_args = match &self.f.ret {
            Ty::Adt(_, a) => a.clone(),
            _ => Vec::new(),
        };
        let payload = if is_result {
            let et = args.get(1).cloned().unwrap_or(Ty::Error);
            vec![self.read(val.project(Proj::Downcast(err)).project(Proj::Field(0)), &et)]
        } else {
            Vec::new()
        };
        self.assign(Place::local(RETURN), Rvalue::Aggregate(Aggregate::Variant(*family, err, ret_args), payload), span);
        self.emit_return(span);
        self.switch_to(ok_bb);
        let t = args.first().cloned().unwrap_or(Ty::Error);
        let op = self.read(val.project(Proj::Downcast(ok)).project(Proj::Field(0)), &t);
        self.assign(dest, Rvalue::Use(op), span);
    }

    // ------------------------------------------------------------ calls

    #[allow(clippy::too_many_arguments)]
    fn finish_call(&mut self, callee: Callee, args: Vec<Operand>, arg_spans: Vec<Span>, dest: Place, diverges: bool, spawn: bool, span: Span) {
        let next = (!diverges).then(|| self.new_block());
        self.terminate(Terminator::Call { callee, args, arg_spans, dest, next, spawn }, span);
        if let Some(n) = next {
            self.switch_to(n);
        }
    }

    fn call(&mut self, e: &Expr, callee: &Expr, args: &[Expr], dest: Place, spawn: bool) {
        let span = e.span;
        let type_args = self.tables().type_args.get(&e.id).cloned().unwrap_or_default();
        match self.res(callee.id) {
            Some(Res::External { module, path }) => {
                let ops = args.iter().map(|a| self.operand(a)).collect();
                self.finish_call(Callee::Opaque(format!("{module}.{}", path.join("."))), ops, spans_of(args), dest, false, spawn, span);
            }
            Some(Res::Symbol(s)) => {
                let s = *s;
                match self.lx.r.symbol(s).kind.clone() {
                    SymbolKind::Builtin => {
                        let name = self.lx.r.symbol(s).name.as_str();
                        let (b, diverges) = match name {
                            "print" => (Builtin::Print, false),
                            "panic" => (Builtin::Panic, true),
                            _ => (Builtin::Channel, false),
                        };
                        // `print` borrows its argument (printing never consumes a
                        // value); copy values are passed directly.
                        let mut ops: Vec<Operand> = args
                            .iter()
                            .map(|a| {
                                let t = self.ty(a);
                                if b == Builtin::Print && !self.is_copy(&t) {
                                    let p = self.place(a);
                                    self.ref_temp(false, p, &t, a.span)
                                } else {
                                    self.operand(a)
                                }
                            })
                            .collect();
                        let mut spans = spans_of(args);
                        // `panic` also reports where it was called, or the
                        // `&Location` it was given (ADR 0094).
                        if b == Builtin::Panic && args.len() == 2 {
                            let at = self.place(&args[1]).project(Proj::Deref).project(Proj::Field(0));
                            ops[1] = self.ref_temp(false, at, &Ty::Str, args[1].span);
                        } else if b == Builtin::Panic && self.lx.sources.is_some() {
                            ops.push(Operand::Const(Const::Str(self.lx.location(span))));
                            spans.push(span);
                        }
                        self.finish_call(Callee::Builtin(b), ops, spans, dest, diverges, spawn, span);
                    }
                    SymbolKind::Primitive => {
                        let o = args.first().map(|a| self.operand(a)).unwrap_or(Operand::Const(Const::Opaque));
                        self.assign(dest, Rvalue::Cast(o, self.ty(e)), span);
                    }
                    SymbolKind::Variant { parent } => {
                        let targs = match self.ty(e) {
                            Ty::Adt(_, a) => a,
                            _ => Vec::new(),
                        };
                        let ops = args.iter().map(|a| self.operand(a)).collect();
                        let idx = self.variant_index(parent, s);
                        self.assign(dest, Rvalue::Aggregate(Aggregate::Variant(parent, idx, targs), ops), span);
                    }
                    SymbolKind::Function | SymbolKind::Method { .. } | SymbolKind::ImplMethod { .. } => {
                        if Some(s) == self.lx.t.decls.ffi_callback {
                            // The frontend established a direct, non-generic function
                            // identity. Keep it explicit through specialization.
                            let mut argument = &args[0];
                            while let ExprKind::Paren(inner) = &argument.kind { argument = inner; }
                            let Rvalue::Use(function) = self.static_value(argument, &self.ty(argument)) else { unreachable!("typed C callback") };
                            self.finish_call(self.fn_callee(s, Vec::new()), vec![function], spans_of(args), dest, false, spawn, span);
                            return;
                        }
                        if self.async_primitive(s, args, dest.clone(), span) {
                            return;
                        }
                        let mut ops: Vec<Operand> = args.iter().map(|a| self.arg(a)).collect();
                        let mut spans = spans_of(args);
                        if self.tables().caller_locations.contains(&e.id) {
                            ops.push(self.caller_location(span));
                            spans.push(span);
                        }
                        if self.is_async_fn(s) {
                            self.async_construct(s, type_args, ops, dest, span);
                            return;
                        }
                        let c = self.fn_callee(s, type_args);
                        self.finish_call(c, ops, spans, dest, false, spawn, span);
                    }
                    _ => {
                        // A local holding a function or closure.
                        let f = self.callable_operand(callee);
                        let ops = args.iter().map(|a| self.arg(a)).collect();
                        self.finish_call(Callee::Value(f), ops, spans_of(args), dest, false, spawn, span);
                    }
                }
            }
            Some(Res::ScrutineeVariant(_)) => {}
            None => match &callee.kind {
                ExprKind::Field { base, name } if !self.tables().callable_calls.contains(&e.id) => self.method_call(e, base, name, args, dest, spawn),
                _ => {
                    let f = self.callable_operand(callee);
                    let ops = args.iter().map(|a| self.operand(a)).collect();
                    self.finish_call(Callee::Value(f), ops, spans_of(args), dest, false, spawn, span);
                }
            },
        }
    }

    fn callable_operand(&mut self, e: &Expr) -> Operand {
        let ty = self.ty(e);
        let mut inner = &ty;
        let mut derefs = 0;
        while let Ty::Ref(_, t) = inner {
            inner = t;
            derefs += 1;
        }
        match inner {
            Ty::Fn(mode, _, _) if *mode != tarn_types::CallMode::Once => {
                let mut p = self.place(e);
                for _ in 0..derefs {
                    p = p.project(Proj::Deref);
                }
                self.ref_temp(*mode == tarn_types::CallMode::Mutable, p, inner, e.span)
            }
            _ => self.operand(e),
        }
    }

    /// `f"a{x}b"` (ADR 0077): `Builder.new()`, one `push` per text piece and
    /// one `Display.display(&x, &mut b)` per value, then `b.finish()`. Values
    /// are only borrowed; the checker recorded each `display` method.
    fn interpolate(&mut self, parts: &[ast::InterpPart], dest: Place, span: Span) {
        let decls = &self.lx.t.decls;
        let builder = decls.builder.expect("checked interpolation needs core.Builder");
        let method = |name: &str| self.lx.r.member(builder, name).expect("core.Builder declares its methods");
        let (new, push, finish) = (method("new"), method("push"), method("finish"));
        let bty = Ty::Adt(builder, Vec::new());
        let b = self.temp(bty.clone(), span);
        self.finish_call(self.fn_callee(new, Vec::new()), Vec::new(), Vec::new(), Place::local(b), false, false, span);
        for part in parts {
            let unit = self.temp(Ty::Void, span);
            match part {
                ast::InterpPart::Text(text) => {
                    let out = self.ref_temp(true, Place::local(b), &bty, span);
                    let t = self.temp(Ty::Str, span);
                    self.assign(Place::local(t), Rvalue::Use(Operand::Const(Const::Str(text.clone()))), span);
                    let text_ref = self.ref_temp(false, Place::local(t), &Ty::Str, span);
                    self.finish_call(self.fn_callee(push, Vec::new()), vec![out, text_ref], vec![span, span], Place::local(unit), false, false, span);
                }
                ast::InterpPart::Value(v) => {
                    let display = self.tables().interpolations[&v.id];
                    let (mut place, mut ty) = if self.is_place(v) {
                        (self.place(v), self.ty(v))
                    } else {
                        let ty = self.ty(v);
                        let t = self.temp(ty.clone(), v.span);
                        self.expr_into(v, Place::local(t));
                        (Place::local(t), ty)
                    };
                    while let Ty::Ref(_, inner) = ty {
                        place = place.project(Proj::Deref);
                        ty = *inner;
                    }
                    let value = self.ref_temp(false, place, &ty, v.span);
                    let out = self.ref_temp(true, Place::local(b), &bty, span);
                    let callee = Callee::Virtual { method: display, type_args: Vec::new() };
                    self.finish_call(callee, vec![value, out], vec![v.span, span], Place::local(unit), false, false, span);
                }
            }
        }
        let owned = self.read(Place::local(b), &bty);
        self.finish_call(self.fn_callee(finish, Vec::new()), vec![owned], vec![span], dest, false, false, span);
    }

    fn fn_callee(&self, s: SymbolId, type_args: Vec<Ty>) -> Callee {
        match self.lx.by_symbol.get(&s) {
            Some(id) if self.lx.kinds.get(id) == Some(&FnKind::Intrinsic) => Callee::Intrinsic(self.qualified(s)),
            Some(id) => Callee::Fn(*id, type_args),
            None => Callee::Virtual { method: s, type_args },
        }
    }

    fn qualified(&self, s: SymbolId) -> String {
        let sym = self.lx.r.symbol(s);
        match &sym.kind {
            SymbolKind::Method { owner } => {
                format!("{}.{}", self.lx.r.symbol(*owner).name, sym.name)
            }
            SymbolKind::Function if self.lx.t.decls.fns.get(&s).is_some_and(|sig| sig.abi.as_deref() == Some("intrinsic")) => {
                sym.module.map(|m| format!("{}.{}", self.lx.r.modules[m.0 as usize].name, sym.name)).unwrap_or_else(|| sym.name.clone())
            }
            _ => sym.name.clone(),
        }
    }

    fn method_call(&mut self, e: &Expr, base: &Expr, name: &ast::Ident, args: &[Expr], dest: Place, spawn: bool) {
        let span = e.span;
        let target = self.tables().method_calls.get(&e.id).cloned();
        let recv = self.tables().receivers.get(&e.id).copied();
        let type_args = self.tables().type_args.get(&e.id).cloned().unwrap_or_default();
        // Receiver place after the recorded auto-derefs.
        let mut place = self.place(base);
        let mut bt = self.ty(base);
        for _ in 0..recv.map_or(0, |r| r.derefs) {
            place = place.project(Proj::Deref);
            bt = match bt {
                Ty::Ref(_, inner) => *inner,
                t => t,
            };
        }
        match target {
            Some(MethodTarget::Intrinsic(n)) if n == "len" || n == "is_empty" => {
                let len = if n == "len" { dest.clone() } else { Place::local(self.new_local(Ty::Int(tarn_types::IntTy::Usize), LocalKind::Temp, None, None, false, span)) };
                self.assign(len.clone(), Rvalue::Len(place), span);
                if n == "is_empty" {
                    self.assign(dest, Rvalue::Binary(BinOp::Eq, Operand::Copy(len), Operand::Const(Const::Int(0, tarn_types::IntTy::Usize))), span);
                }
            }
            Some(MethodTarget::Symbol(m)) => {
                // Arguments are evaluated *before* the receiver is borrowed, so
                // `c.add(c.value)` reads `c` before `&mut c` exists. This gives
                // the effect of two-phase borrows without a separate concept.
                let arg_ops: Vec<Operand> = args
                    .iter()
                    .map(|a| {
                        // Materialize place operands now: the call terminator
                        // reads its operands after the receiver borrow.
                        let op = self.arg(a);
                        match op {
                            Operand::Copy(_) | Operand::Move(_) => {
                                let place = match &op { Operand::Copy(p) | Operand::Move(p) => p, _ => unreachable!() };
                                let ty = crate::post_drop::place_ty(&self.f, self.lx.t, place).expect("checked argument place");
                                let t = self.new_local(ty.clone(), LocalKind::Temp, None, None, false, a.span);
                                self.assign(Place::local(t), Rvalue::Use(op), a.span);
                                self.read(Place::local(t), &ty)
                            }
                            c => c,
                        }
                    })
                    .collect();
                let recv_op = match recv.map(|r| r.kind) {
                    Some(ReceiverKind::Ref) | None => self.ref_temp(false, place, &bt, base.span),
                    Some(ReceiverKind::RefMut) => self.ref_temp(true, place, &bt, base.span),
                    Some(ReceiverKind::Value) => self.read(place, &bt),
                };
                let mut ops = vec![recv_op];
                ops.extend(arg_ops);
                let mut spans: Vec<Span> = std::iter::once(base.span).chain(args.iter().map(|a| a.span)).collect();
                if self.tables().caller_locations.contains(&e.id) {
                    ops.push(self.caller_location(span));
                    spans.push(span);
                }
                if self.is_async_fn(m) {
                    self.async_construct(m, type_args, ops, dest, span);
                    return;
                }
                let c = self.fn_callee(m, type_args);
                self.finish_call(c, ops, spans, dest, false, spawn, span);
            }
            _ => {
                // Opaque std method: its signature is unknown, so the receiver
                // is assumed to be borrowed (never moved): an unchecked call
                // must not invent moves the real API may not make.
                let mut ops = vec![self.ref_temp(false, place, &bt, span)];
                ops.extend(args.iter().map(|a| self.operand(a)));
                let spans = std::iter::once(base.span).chain(args.iter().map(|a| a.span)).collect();
                self.finish_call(Callee::Opaque(format!("<std>.{}", name.name)), ops, spans, dest, false, spawn, span);
            }
        }
    }

    fn ref_temp(&mut self, mutable: bool, place: Place, ty: &Ty, span: Span) -> Operand {
        let rt = Ty::Ref(mutable, Box::new(ty.clone()));
        let t = self.new_local(rt.clone(), LocalKind::Temp, None, None, false, span);
        self.assign(Place::local(t), Rvalue::Ref(mutable, place), span);
        self.read(Place::local(t), &rt)
    }

    fn spawn_task(&mut self, e: &Expr, callable: &Expr, dest: Place) {
        let ty = self.ty(callable);
        let Ty::Fn(mode, _, result) = &ty else { return };
        let worker = FunctionId(self.lx.next_id.get());
        let drop_result = FunctionId(worker.0 + 1);
        self.lx.next_id.set(worker.0 + 2);
        let mut wb = Builder::new(self.lx, self.m, worker, format!("{}::task-worker#{}", self.f.name, worker.0), e.span);
        wb.f.generics = self.f.generics.clone();
        wb.f.ret = *result.clone();
        wb.new_local(*result.clone(), LocalKind::Return, None, None, false, e.span);
        wb.scopes.push(Vec::new());
        let input = wb.new_local(ty.clone(), LocalKind::Param, None, None, false, e.span);
        wb.scopes[0].push(input);
        wb.f.param_count = 1;
        let call = match mode {
            tarn_types::CallMode::Once => Operand::Move(Place::local(input)),
            mode => wb.ref_temp(*mode == tarn_types::CallMode::Mutable, Place::local(input), &ty, e.span),
        };
        wb.finish_call(Callee::Value(call), Vec::new(), Vec::new(), Place::local(RETURN), false, false, e.span);
        wb.emit_return(e.span);
        self.lx.extra.borrow_mut().push(wb.finish());
        let mut db = Builder::new(self.lx, self.m, drop_result, format!("{}::task-result-drop#{}", self.f.name, worker.0), e.span);
        db.f.generics = self.f.generics.clone();
        db.new_local(Ty::Void, LocalKind::Return, None, None, false, e.span);
        db.scopes.push(Vec::new());
        let input = db.new_local(*result.clone(), LocalKind::Param, None, None, false, e.span);
        db.scopes[0].push(input);
        db.f.param_count = 1;
        db.fall_off_end(e.span);
        self.lx.extra.borrow_mut().push(db.finish());
        let input = self.operand(callable);
        let scoped = self.tables().scoped_spawns.contains(&e.id);
        let mut inputs = vec![input];
        if scoped {
            let witness = *self.task_witnesses.last().expect("typed scoped spawn without scope");
            inputs.push(self.ref_temp(false, Place::local(witness), &Ty::Bool, e.span));
        }
        let spans = vec![callable.span; inputs.len()];
        self.finish_call(Callee::TaskSpawn { worker, drop_result, scoped,
            type_args: self.f.generics.iter().copied().map(Ty::Param).collect() },
            inputs, spans, dest, false, true, e.span);
    }

    // ------------------------------------------------------------ async (ADR 0037)

    fn is_async_fn(&self, s: SymbolId) -> bool {
        self.lx.t.decls.fns.get(&s).is_some_and(|sig| sig.is_async) && self.lx.by_symbol.contains_key(&s)
    }

    /// Trusted `net` async-body primitives. `_with_waker(body)` calls `body`
    /// with the waker of the current poll; `_async_park()` is a bare
    /// suspension edge. Neither creates a second wake mechanism: the stdlib
    /// arms readiness on the same Phase-12C Waker before parking.
    fn async_primitive(&mut self, s: SymbolId, args: &[Expr], dest: Place, span: Span) -> bool {
        let decls = &self.lx.t.decls;
        let (waker, park) = (decls.exec_async_waker == Some(s), decls.exec_async_park == Some(s));
        if !waker && !park {
            return false;
        }
        let Some(info) = self.f.asynchronous.clone() else {
            self.diags.push(Diagnostic::error("E3060", "await_outside_async", "async-body primitive used outside an async function").primary(span, "this function does not suspend"));
            return true;
        };
        if waker {
            let Some(body) = args.first() else { return true };
            let callee = self.callable_operand(body);
            self.finish_call(Callee::Value(callee), vec![Operand::Copy(Place::local(info.waker))], vec![span], dest, false, false, span);
        } else {
            self.suspend(span);
            self.assign(dest, Rvalue::Use(Operand::Const(Const::Unit)), span);
        }
        true
    }

    fn waker_ty(&self) -> Ty {
        match self.lx.t.decls.exec_waker {
            Some(w) => Ty::Ref(false, Box::new(Ty::Adt(w, Vec::new()))),
            None => Ty::Error,
        }
    }

    /// Calling an async function only moves its arguments into a new frame.
    fn async_construct(&mut self, s: SymbolId, type_args: Vec<Ty>, ops: Vec<Operand>, dest: Place, span: Span) {
        let id = self.lx.by_symbol[&s];
        self.assign(dest, Rvalue::Aggregate(Aggregate::AsyncFrame(id, type_args), ops), span);
    }

    /// An explicit suspension point. Abandonment runs the drops of every
    /// enclosing scope and pending temporary, exactly like an early return.
    fn suspend(&mut self, span: Span) {
        let (resume, abandon) = (self.new_block(), self.new_block());
        self.terminate(Terminator::Suspend { resume, abandon }, span);
        self.switch_to(abandon);
        self.exit_to(0, span);
        self.terminate(Terminator::Abandon, span);
        self.switch_to(resume);
    }

    /// `await child`: poll the owned child with this poll's waker until Ready.
    /// Source computations are pollers called directly with the parent waker;
    /// trusted manual Operations forward wakes through `poll_with` (ADR 0036).
    fn await_into(&mut self, e: &Expr, inner: &Expr, dest: Place) {
        let span = e.span;
        let child_ty = self.ty(inner);
        let output = self.ty(e);
        let (Some(progress), Some(info)) = (self.lx.t.decls.exec_progress, self.f.asynchronous.clone()) else {
            let _ = self.operand(inner);
            self.assign(dest, Rvalue::Use(Operand::Const(Const::Opaque)), span);
            return;
        };
        // The completed child is destroyed with the statement, releasing its
        // child-only loans; a borrowed result keeps only the loans it carries.
        // A manual Operation's result may borrow its poller storage, so the
        // ordinary borrow checker then rejects that early destruction.
        let child = self.temp(child_ty.clone(), inner.span);
        self.expr_into(inner, Place::local(child));
        let progress_ty = Ty::Adt(progress, vec![output.clone()]);
        let def = &self.lx.t.decls.enums[&progress];
        let pending = def.variants.iter().position(|v| v.name == "Pending").unwrap_or(0) as u32;
        let ready = def.variants.iter().position(|v| v.name == "Ready").unwrap_or(1) as u32;
        let poll = self.new_block();
        self.goto(poll, span);
        self.switch_to(poll);
        let r = self.new_local(progress_ty.clone(), LocalKind::Temp, None, None, false, span);
        let waker = Operand::Copy(Place::local(info.waker));
        let callee = match &child_ty {
            Ty::Adt(op, args) if Some(*op) == self.lx.t.decls.exec_operation => {
                let with = self.lx.t.decls.exec_poll_with.and_then(|m| self.lx.by_symbol.get(&m).copied());
                let this = self.ref_temp(true, Place::local(child), &child_ty, inner.span);
                match with {
                    Some(id) => (Callee::Fn(id, args.clone()), vec![this, waker]),
                    None => (Callee::Opaque("runtime.Operation.poll_with".into()), vec![this, waker]),
                }
            }
            _ => {
                let this = self.ref_temp(true, Place::local(child), &child_ty, inner.span);
                (Callee::Value(this), vec![waker])
            }
        };
        let (callee, args) = callee;
        let spans = vec![inner.span; args.len()];
        self.finish_call(callee, args, spans, Place::local(r), false, false, span);
        let d = self.new_local(Ty::Int(tarn_types::IntTy::U32), LocalKind::Temp, None, None, false, span);
        self.assign(Place::local(d), Rvalue::Discriminant(Place::local(r)), span);
        let (pending_bb, ready_bb) = (self.new_block(), self.new_block());
        self.terminate(Terminator::Switch { discr: Operand::Copy(Place::local(d)), cases: vec![(pending as i128, pending_bb)], otherwise: ready_bb }, span);
        self.switch_to(pending_bb);
        self.push(StatementKind::Drop(Place::local(r)), span);
        let (resume, abandon) = (self.new_block(), self.new_block());
        self.terminate(Terminator::Suspend { resume, abandon }, span);
        self.switch_to(resume);
        self.goto(poll, span);
        self.switch_to(abandon);
        self.exit_to(0, span);
        self.terminate(Terminator::Abandon, span);
        self.switch_to(ready_bb);
        let value = self.read(Place::local(r).project(Proj::Downcast(ready)).project(Proj::Field(0)), &output);
        self.assign(dest, Rvalue::Use(value), span);
    }

    // ------------------------------------------------------------ closures

    /// A closure becomes its own function whose first parameters are
    /// references to the captured locals (`&mut` when the body mutates them).
    fn closure(&mut self, e: &Expr, params: &[ast::ClosureParam], body: &ast::Block, dest: Place) {
        let captured: Vec<SymbolId> = self.lx.r.tables[self.m.0 as usize].captures.get(&e.id).map(|c| c.symbols.clone()).unwrap_or_default();
        let mutated = self.tables().mutable_captures.get(&e.id).cloned().unwrap_or_default();
        let owned = matches!(e.kind, ExprKind::Closure { owned: true, .. });
        let consumes = matches!(self.ty(e), Ty::Fn(tarn_types::CallMode::Once, ..));
        let environment: Vec<Ty> = captured.iter().map(|s| if owned { self.sym_ty(*s) } else { Ty::Ref(mutated.contains(s), Box::new(self.sym_ty(*s))) }).collect();
        let id = FunctionId(self.lx.next_id.get());
        self.lx.next_id.set(id.0 + 1);
        let name = format!("{}::closure#{}", self.f.name, self.closure_count);
        self.closure_count += 1;
        let mut cb = Builder::new(self.lx, self.m, id, name, e.span);
        let (param_tys, ret) = match self.ty(e) {
            Ty::Fn(_, ps, r) => (ps, *r),
            _ => (vec![Ty::Error; params.len()], Ty::Error),
        };
        let modes = self.tables().closure_captures.get(&e.id).map(|cs| cs.iter().map(|(_, mode)| *mode).collect()).unwrap_or_default();

        let destructor = if owned {
            let id = FunctionId(self.lx.next_id.get());
            self.lx.next_id.set(id.0 + 1);
            Some(id)
        } else {
            None
        };
        cb.f.kind = FnKind::Closure { parent: self.f.id, captures: modes, environment: environment.clone(), owned, consumes, destructor, destructor_body: false };
        cb.f.ret = ret.clone();
        cb.f.generics = self.f.generics.clone();
        cb.new_local(ret, LocalKind::Return, None, None, false, e.span);
        cb.scopes.push(Vec::new());
        let mut cap_ops = Vec::new();
        for &s in &captured {
            let m = mutated.contains(&s);
            let t = if owned && consumes { self.sym_ty(s) } else { Ty::Ref(m, Box::new(self.sym_ty(s))) };
            let name = self.lx.r.symbol(s).name.clone();
            let l = cb.new_local(t.clone(), LocalKind::Param, Some(name), None, false, e.span);
            if owned && consumes {
                cb.vars.insert(s, l);
                cb.scopes[0].push(l);
            } else {
                cb.captures.insert(s, l);
            }
            let p = self.symbol_place(s).unwrap_or(Place::local(RETURN));
            cap_ops.push(if owned { self.read(p, &self.sym_ty(s)) } else { self.ref_temp(m, p, &self.sym_ty(s), e.span) });
        }
        for (p, t) in params.iter().zip(param_tys) {
            if let Some(s) = self.def(p.id) {
                let l = cb.new_local(t, LocalKind::Param, Some(p.name.name.clone()), Some(s), false, p.name.span);
                cb.vars.insert(s, l);
                cb.scopes[0].push(l);
            }
        }
        cb.f.param_count = cb.f.locals.len() as u32 - 1;
        cb.stmts(&body.stmts);
        cb.fall_off_end(body.span);
        self.diags.append(&mut cb.diags);
        let f = cb.finish();
        self.lx.extra.borrow_mut().push(f);
        if let Some(did) = destructor {
            let mut db = Builder::new(self.lx, self.m, did, format!("{}::environment-drop#{}", self.f.name, self.closure_count - 1), e.span);
            db.f.generics = self.f.generics.clone();
            db.f.kind = FnKind::Closure { parent: self.f.id, captures: vec![CaptureMode::Move; captured.len()], environment: environment.clone(), owned: true, consumes: true, destructor: None, destructor_body: true };
            db.new_local(Ty::Void, LocalKind::Return, None, None, false, e.span);
            db.scopes.push(Vec::new());
            for ty in environment {
                let l = db.new_local(ty, LocalKind::Param, None, None, false, e.span);
                db.scopes[0].insert(0, l);
            }
            db.f.param_count = captured.len() as u32;
            db.fall_off_end(e.span);
            self.lx.extra.borrow_mut().push(db.finish());
        }
        if captured.is_empty() {
            self.assign(dest, Rvalue::Use(Operand::Const(Const::Fn(id, self.f.generics.iter().copied().map(Ty::Param).collect()))), e.span);
        } else {
            let storage = if owned { None } else {
                let l = self.new_local(Ty::Bool, LocalKind::User, Some("closure environment".into()), None, false, e.span);
                self.push(StatementKind::StorageLive(l), e.span);
                self.assign(Place::local(l), Rvalue::Use(Operand::Const(Const::Bool(true))), e.span);
                if let Some(scope) = self.scopes.last_mut() { scope.push(l); }
                Some(Place::local(l))
            };
            self.assign(dest, Rvalue::Aggregate(Aggregate::Closure(id, storage), cap_ops), e.span);
        }
    }
}

fn spans_of(args: &[Expr]) -> Vec<Span> {
    args.iter().map(|a| a.span).collect()
}

fn peel_ty(t: &Ty) -> &Ty {
    match t {
        Ty::Ref(_, inner) => peel_ty(inner),
        t => t,
    }
}

fn int_const(v: i128, ty: &Ty) -> Const {
    match ty {
        Ty::Int(it) => Const::Int(v, *it),
        _ => Const::Int(v, tarn_types::IntTy::I64),
    }
}

pub(crate) fn binop_name(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "add",
        BinOp::Sub => "sub",
        BinOp::Mul => "mul",
        BinOp::AddProven => "add_proven",
        BinOp::SubProven => "sub_proven",
        BinOp::MulProven => "mul_proven",
        BinOp::Div => "div",
        BinOp::Rem => "rem",
        BinOp::BitAnd => "bitand",
        BinOp::BitOr => "bitor",
        BinOp::BitXor => "bitxor",
        BinOp::Shl => "shl",
        BinOp::Shr => "shr",
        BinOp::Eq => "eq",
        BinOp::Ne => "ne",
        BinOp::Lt => "lt",
        BinOp::Le => "le",
        BinOp::Gt => "gt",
        BinOp::Ge => "ge",
    }
}

fn prune(f: &mut Function) {
    if f.blocks.is_empty() {
        return;
    }
    let mut reach = vec![false; f.blocks.len()];
    let mut stack = vec![0usize];
    while let Some(b) = stack.pop() {
        if std::mem::replace(&mut reach[b], true) {
            continue;
        }
        stack.extend(f.blocks[b].term.successors().into_iter().map(|s| s.0 as usize));
    }
    let mut map = vec![u32::MAX; f.blocks.len()];
    let mut n = 0;
    for (i, r) in reach.iter().enumerate() {
        if *r {
            map[i] = n;
            n += 1;
        }
    }
    let old = std::mem::take(&mut f.blocks);
    for (i, mut b) in old.into_iter().enumerate() {
        if !reach[i] {
            continue;
        }
        for t in b.term.targets_mut() {
            t.0 = map[t.0 as usize];
        }
        f.blocks.push(b);
    }
}
