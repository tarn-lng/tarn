//! Reachable specialization of already typed post-drop semantics.
use crate::{Error, Result};
use std::collections::HashMap;
use tarn_ir::{post_drop as post, *};
use tarn_types::{Ty, Typed};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub function: FunctionId,
    pub args: Vec<Ty>,
}
const MAX_INSTANCES: usize = 1 << 20;
struct Instances<'a> {
    source: &'a post::Program,
    keys: Vec<Key>,
    index: HashMap<Key, FunctionId>,
    functions: Vec<post::Function>,
}
impl Instances<'_> {
    fn intern(&mut self, function: FunctionId, args: Vec<Ty>) -> Result<FunctionId> {
        let key = Key { function, args };
        if let Some(id) = self.index.get(&key) {
            return Ok(*id);
        }
        // Expanding instantiation cycles are rejected before this phase
        // (E3074); type-size limits bound the cases that check cannot see.
        // This cap only guards memory against a compiler bug.
        if self.keys.len() >= MAX_INSTANCES {
            return Err(Error::bug(format!("monomorphization exceeds {MAX_INSTANCES} reachable instances")));
        }
        let template = self.source.functions.get(function.0 as usize).ok_or_else(|| Error::bug("missing specialization template"))?;
        if template.decl.generics.len() != key.args.len() {
            return Err(Error::bug(format!("{}: generic argument arity", template.decl.name)));
        }
        for ty in &key.args {
            concrete(ty)?;
        }
        let id = FunctionId(self.keys.len() as u32);
        self.index.insert(key.clone(), id);
        self.keys.push(key);
        self.functions.push(template.clone());
        Ok(id)
    }
}
fn concrete(ty: &Ty) -> Result<()> {
    concrete_inner(ty, 0, &mut 0)
}
fn concrete_inner(ty: &Ty, depth: usize, nodes: &mut usize) -> Result<()> {
    *nodes += 1;
    if depth >= 64 || *nodes > 4096 {
        return Err(Error::unsupported("monomorphization type exceeds 64 levels or 4096 nodes"));
    }
    match ty {
        Ty::Param(_) | Ty::Var(_) | Ty::Opaque | Ty::Error | Ty::Slice(_) | Ty::Any(_) => {
            return Err(Error::unsupported(format!("nonconcrete/unsized monomorphization argument {ty:?}")));
        }
        Ty::Adt(_, args) => {
            for ty in args {
                concrete_inner(ty, depth + 1, nodes)?;
            }
        }
        Ty::Array(ty, _) => concrete_inner(ty, depth + 1, nodes)?,
        Ty::Ref(_, ty) => {
            if matches!(ty.as_ref(), Ty::Any(_)) {
                return Ok(());
            }
            if let Ty::Slice(elem) = ty.as_ref() {
                concrete_inner(elem, depth + 1, nodes)?;
            } else {
                concrete_inner(ty, depth + 1, nodes)?;
            }
        }
        Ty::Fn(_, args, ret) => {
            for ty in args {
                concrete_inner(ty, depth + 1, nodes)?;
            }
            concrete_inner(ret, depth + 1, nodes)?;
        }
        Ty::Async(output) => concrete_inner(output, depth + 1, nodes)?,
        _ => {}
    }
    Ok(())
}
pub fn specialize(p: &post::Program, t: &Typed) -> Result<post::Program> {
    let main = p.functions.iter().find(|f| f.decl.name == "main" && f.decl.symbol.and_then(|s| t.decls.fns.get(&s)).is_some_and(|sig| sig.module.0 == 0)).ok_or_else(|| Error::unsupported("program requires fn main()"))?;
    specialize_entry(p, t, main.decl.id)
}

pub fn specialize_entry(p: &post::Program, t: &Typed, entry: FunctionId) -> Result<post::Program> {
    let mut cx = Instances { source: p, keys: Vec::new(), index: HashMap::new(), functions: Vec::new() };
    cx.intern(entry, Vec::new())?;
    let mut i = 0;
    while i < cx.keys.len() {
        let key = cx.keys[i].clone();
        let mut f = cx.functions[i].clone();
        let map: HashMap<_, _> = f.decl.generics.iter().copied().zip(key.args.iter().cloned()).collect();
        f.decl.id = FunctionId(i as u32);
        f.decl.generics.clear();
        f.decl.ret = tarn_types::subst(&f.decl.ret, &map);
        for l in &mut f.decl.locals {
            l.ty = tarn_types::subst(&l.ty, &map);
        }
        // Concrete source locals were checked by the frontend (E3075); generic
        // ones become concrete only here, before inlining hides their function.
        if !map.is_empty() {
            let mut locals: Vec<_> = f.decl.locals.iter().collect();
            locals.sort_by_key(|l| l.name.is_none());
            for l in locals {
                if let Err(e) = tarn_ir::layout::layout(t, &l.ty)
                    && e.limit
                {
                    let binding = l.name.as_deref().map_or_else(|| "a temporary".to_string(), |n| format!("`{n}`"));
                    return Err(Error::unsupported(format!("{} for {binding} in `{}` once its generic arguments are known", e.message, f.decl.name)));
                }
            }
        }
        if let Some(frame) = f.decl.asynchronous.as_mut().and_then(|a| a.frame.as_mut()) {
            frame.output = tarn_types::subst(&frame.output, &map);
        }
        if let FnKind::Closure { environment, destructor, .. } = &mut f.decl.kind {
            for ty in environment {
                *ty = tarn_types::subst(ty, &map);
            }
            if let Some(id) = destructor {
                let def = &p.functions[id.0 as usize].decl;
                let args = def.generics.iter().map(|p| map.get(p).cloned().unwrap_or(Ty::Param(*p))).collect();
                *id = cx.intern(*id, args)?;
            }
        }
        // IDs follow deterministic FIFO traversal, never hash iteration.
        if i != 0 {
            f.decl.name = format!("{}::instance#{i}", f.decl.name);
        }
        for b in &mut f.blocks {
            for stmt in &mut b.stmts {
                if let post::Op::Plain(StatementKind::Assign(_, rv)) = &mut stmt.op {
                    match rv {
                        Rvalue::Use(o) | Rvalue::Unary(_, o) => operand(o, &map, &mut cx)?,
                        Rvalue::Binary(_, a, b) => {
                            operand(a, &map, &mut cx)?;
                            operand(b, &map, &mut cx)?;
                        }
                        Rvalue::Coerce(CoerceKind::ToDyn(interface), o, ty) => {
                            let source = operand_ty(&f.decl, t, o)?;
                            let Ty::Ref(_, concrete) = source else {
                                return Err(Error::bug("dynamic coercion source is not a reference"));
                            };
                            let methods = table(&concrete, *interface, t, p, &mut cx)?;
                            let interface = *interface;
                            let ty = tarn_types::subst(ty, &map);
                            *rv = Rvalue::Coerce(CoerceKind::DynTable { interface, concrete: *concrete, methods }, o.clone(), ty);
                        }
                        Rvalue::Cast(o, ty) | Rvalue::Coerce(_, o, ty) => {
                            operand(o, &map, &mut cx)?;
                            *ty = tarn_types::subst(ty, &map);
                        }
                        Rvalue::Aggregate(a, os) => {
                            match a {
                                Aggregate::Struct(_, ts) | Aggregate::Variant(_, _, ts) => {
                                    for ty in ts {
                                        *ty = tarn_types::subst(ty, &map);
                                    }
                                }
                                Aggregate::Array(ty) => *ty = tarn_types::subst(ty, &map),
                                Aggregate::AsyncFrame(id, ts) => {
                                    for ty in ts.iter_mut() {
                                        *ty = tarn_types::subst(ty, &map);
                                    }
                                    *id = cx.intern(*id, ts.clone())?;
                                }
                                Aggregate::Closure(id, _) => {
                                    let def = &p.functions[id.0 as usize].decl;
                                    let args = def.generics.iter().map(|p| map.get(p).cloned().unwrap_or(Ty::Param(*p))).collect();
                                    *id = cx.intern(*id, args)?;
                                }
                            }
                            for o in os {
                                operand(o, &map, &mut cx)?;
                            }
                        }
                        Rvalue::SliceRef { start, end, .. } => {
                            for o in start.iter_mut().chain(end.iter_mut()) {
                                operand(o, &map, &mut cx)?;
                            }
                        }
                        _ => {}
                    }
                }
            }
            match &mut b.term {
                Terminator::Call { callee, args, .. } => {
                    match callee {
                        Callee::TaskSpawn { worker, drop_result, type_args, .. } => {
                            let ts = type_args.iter().map(|ty| tarn_types::subst(ty, &map)).collect::<Vec<_>>();
                            *worker = cx.intern(*worker, ts.clone())?;
                            *drop_result = cx.intern(*drop_result, ts)?;
                            type_args.clear();
                        }
                        Callee::Fn(id, ts) => {
                            for ty in ts.iter_mut() {
                                *ty = tarn_types::subst(ty, &map);
                            }
                            *id = cx.intern(*id, ts.clone())?;
                            ts.clear();
                        }
                        Callee::Value(o) => operand(o, &map, &mut cx)?,
                        Callee::Virtual { method, type_args } => {
                            let source = operand_ty(&f.decl, t, args.first().ok_or_else(|| Error::bug("virtual call missing receiver"))?)?;
                            let receiver_type = match source {
                                Ty::Ref(_, inner) => *inner,
                                ty => ty,
                            };
                            if let Some(target) = t.decls.impl_target(&receiver_type) {
                                let concrete_args = match &receiver_type {
                                    Ty::Adt(_, args) => args.clone(),
                                    _ => Vec::new(),
                                };
                                let (interface, index) = t.decls.interface_methods.get(method).copied().ok_or_else(|| Error::bug("unregistered interface method"))?;
                                let symbol = t
                                    .decls
                                    .implementations
                                    .get(&(interface, target))
                                    .and_then(|ms| ms.get(index))
                                    .ok_or_else(|| Error::bug("missing resolved static implementation"))?;
                                let id = p.by_symbol.get(symbol).copied().ok_or_else(|| Error::bug("missing static implementation function"))?;
                                let mut instance_args = concrete_args.clone();
                                instance_args.extend(type_args.iter().map(|ty| tarn_types::subst(ty, &map)));
                                *callee = Callee::Fn(cx.intern(id, instance_args)?, Vec::new());
                            } else {
                                for ty in type_args {
                                    *ty = tarn_types::subst(ty, &map);
                                }
                            }
                        }
                        _ => {}
                    }
                    for o in args {
                        operand(o, &map, &mut cx)?;
                    }
                }
                Terminator::Switch { discr, .. } => operand(discr, &map, &mut cx)?,
                _ => {}
            }
        }
        // A generic owned T may specialize to Copy: remove resource glue using
        // concrete type semantics, never initialization or borrow information.
        let decl = f.decl.clone();
        for b in &mut f.blocks {
            b.stmts.retain_mut(|s| if let post::Op::Destroy(d) = &mut s.op { prune(d, &decl, t) } else { true });
        }
        cx.functions[i] = f;
        i += 1;
    }
    Ok(post::Program { functions: cx.functions, by_symbol: HashMap::new() })
}
fn operand(o: &mut Operand, map: &HashMap<tarn_types::ParamId, Ty>, cx: &mut Instances<'_>) -> Result<()> {
    if let Operand::Const(Const::Fn(id, ts)) = o {
        for ty in ts.iter_mut() {
            *ty = tarn_types::subst(ty, map);
        }
        *id = cx.intern(*id, ts.clone())?;
        ts.clear();
    }
    Ok(())
}
fn prune(d: &mut post::Drop, f: &tarn_ir::Function, t: &Typed) -> bool {
    match d {
        post::Drop::Value(p) => post::place_ty(f, t, p).is_none_or(|ty| !t.decls.is_copy(&ty) && !matches!(ty, Ty::Ref(..))),
        post::Drop::Guard(_, d) => prune(d, f, t),
        post::Drop::Fields { fields, .. } => {
            fields.retain_mut(|(_, d)| prune(d, f, t));
            !fields.is_empty()
        }
        post::Drop::Variants { variants, .. } => {
            for fs in variants.iter_mut() {
                fs.retain_mut(|(_, d)| prune(d, f, t));
            }
            variants.iter().any(|fs| !fs.is_empty())
        }
        post::Drop::Remaining { place, .. } => post::place_ty(f, t, place).is_none_or(|ty| !t.decls.is_copy(&ty)),
        // The storage is always freed, whatever the element type.
        post::Drop::RemainingVec { .. } => true,
    }
}

fn operand_ty(f: &Function, t: &Typed, o: &Operand) -> Result<Ty> {
    match o {
        Operand::Copy(p) | Operand::Move(p) => post::place_ty(f, t, p).ok_or_else(|| Error::bug("invalid dynamic operand")),
        _ => Err(Error::bug("dynamic receiver must be a place")),
    }
}
fn table(concrete: &Ty, interface: tarn_resolve::SymbolId, t: &Typed, p: &post::Program, cx: &mut Instances<'_>) -> Result<Vec<FunctionId>> {
    let target = t.decls.impl_target(concrete).ok_or_else(|| Error::bug("dynamic coercion without resolved implementation target"))?;
    let args = match concrete { Ty::Adt(_, args) => args.clone(), _ => Vec::new() };
    let declarations = t.decls.interfaces.get(&interface).ok_or_else(|| Error::bug("missing interface table"))?;
    let methods = t.decls.implementations.get(&(interface, target)).ok_or_else(|| Error::bug("missing resolved implementation"))?;
    if declarations.len() != methods.len() {
        return Err(Error::bug("incomplete interface table"));
    }
    methods
        .iter()
        .map(|symbol| {
            let id = p.by_symbol.get(symbol).copied().ok_or_else(|| Error::bug("missing implementation function"))?;
            if p.functions[id.0 as usize].decl.generics.len() != args.len() {
                return Err(Error::unsupported("generic dynamic methods"));
            }
            cx.intern(id, args.clone())
        })
        .collect()
}
