//! Move paths: the places whose initialization is tracked.
//!
//! A tree per local: the local itself, and one node per `Field(i)` and
//! `Downcast(v)` projection that occurs in the function. Places that go
//! through a `Deref` or an `Index` are not tracked below that point: they
//! resolve to their tracked prefix plus the kind of projection that stopped
//! the walk.

use std::collections::HashMap;
use tarn_ir::{Function, LocalId, Operand, Place, Proj, Rvalue, StatementKind, Terminator};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MovePathId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PathElem {
    Field(u32),
    Downcast(u32),
}

#[derive(Clone, Debug)]
pub struct MovePath {
    pub local: LocalId,
    pub parent: Option<MovePathId>,
    pub elem: Option<PathElem>,
    pub children: Vec<MovePathId>,
}

/// Where a place lands in the move-path tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lookup {
    /// The place is exactly this path.
    Exact(MovePathId),
    /// The place is behind a reference reached from this path.
    Deref(MovePathId),
    /// The place is an element (by dynamic index) of this path.
    Index(MovePathId),
}

#[derive(Debug, Default)]
pub struct MovePaths {
    pub paths: Vec<MovePath>,
    roots: Vec<MovePathId>,
    index: HashMap<(MovePathId, PathElem), MovePathId>,
    /// Path and all its descendants, precomputed.
    subtree: Vec<Vec<MovePathId>>,
}

impl MovePaths {
    pub fn build(f: &Function) -> MovePaths {
        let mut mp = MovePaths::default();
        for i in 0..f.locals.len() {
            let id = MovePathId(mp.paths.len() as u32);
            mp.paths.push(MovePath { local: LocalId(i as u32), parent: None, elem: None, children: Vec::new() });
            mp.roots.push(id);
        }
        let mut places: Vec<&Place> = Vec::new();
        fn op<'p>(o: &'p Operand, out: &mut Vec<&'p Place>) {
            if let Operand::Copy(p) | Operand::Move(p) = o {
                out.push(p);
            }
        }
        for b in &f.blocks {
            for s in &b.stmts {
                match &s.kind {
                    StatementKind::Assign(p, rv) => {
                        places.push(p);
                        match rv {
                            Rvalue::Use(o) | Rvalue::Unary(_, o) | Rvalue::Cast(o, _) | Rvalue::Coerce(_, o, _) => op(o, &mut places),
                            Rvalue::Binary(_, a, b) => {
                                op(a, &mut places);
                                op(b, &mut places);
                            }
                            Rvalue::Aggregate(kind, os) => {
                                os.iter().for_each(|o| op(o, &mut places));
                                if let tarn_ir::Aggregate::Closure(_, Some(storage)) = kind { places.push(storage); }
                            },
                            Rvalue::Ref(_, p) | Rvalue::Discriminant(p) | Rvalue::Len(p) => places.push(p),
                            Rvalue::SliceRef { base, start, end, .. } => {
                                places.push(base);
                                start.iter().chain(end.iter()).for_each(|o| op(o, &mut places));
                            }
                        }
                    }
                    StatementKind::Drop(p) => places.push(p),
                    _ => {}
                }
            }
            match &b.term {
                Terminator::Call { args, dest, callee, .. } => {
                    args.iter().for_each(|o| op(o, &mut places));
                    places.push(dest);
                    if let tarn_ir::Callee::Value(o) = callee {
                        op(o, &mut places);
                    }
                }
                Terminator::Switch { discr, .. } => op(discr, &mut places),
                _ => {}
            }
        }
        for p in places {
            mp.intern(p);
        }
        mp.subtree = (0..mp.paths.len()).map(|i| mp.collect_subtree(MovePathId(i as u32))).collect();
        mp
    }

    /// Complete linear aggregate paths, including unmentioned siblings and
    /// inactive enum variants. Ordinary programs retain sparse move paths.
    pub fn complete_linear(
        &mut self,
        f: &Function,
        t: &tarn_types::Typed,
        linear: &std::collections::HashSet<tarn_types::ParamId>,
    ) {
        fn expand(
            mp: &mut MovePaths,
            place: Place,
            ty: tarn_types::Ty,
            t: &tarn_types::Typed,
            linear: &std::collections::HashSet<tarn_types::ParamId>,
            depth: usize,
        ) {
            if depth == 64 || !t.decls.is_must_consume_with(&ty, linear) {
                return;
            }
            mp.intern(&place);
            if let tarn_types::Ty::Adt(s, args) = ty {
                if let Some(d) = t.decls.structs.get(&s) {
                    let map = d.generics.iter().copied().zip(args).collect();
                    for (i, field) in d.fields.iter().enumerate() {
                        expand(
                            mp,
                            place.project(Proj::Field(i as u32)),
                            tarn_types::subst(&field.ty, &map),
                            t,
                            linear,
                            depth + 1,
                        );
                    }
                } else if let Some(d) = t.decls.enums.get(&s) {
                    let map = d.generics.iter().copied().zip(args).collect();
                    for (v, variant) in d.variants.iter().enumerate() {
                        let down = place.project(Proj::Downcast(v as u32));
                        mp.intern(&down);
                        for (i, field) in variant.fields.iter().enumerate() {
                            expand(
                                mp,
                                down.project(Proj::Field(i as u32)),
                                tarn_types::subst(field, &map),
                                t,
                                linear,
                                depth + 1,
                            );
                        }
                    }
                }
            }
        }
        for (i, l) in f.locals.iter().enumerate() {
            expand(
                self,
                Place::local(LocalId(i as u32)),
                l.ty.clone(),
                t,
                linear,
                0,
            );
        }
        self.subtree = (0..self.paths.len())
            .map(|i| self.collect_subtree(MovePathId(i as u32)))
            .collect();
    }

    fn intern(&mut self, p: &Place) {
        let mut cur = self.roots[p.local.0 as usize];
        for proj in &p.proj {
            let elem = match proj {
                Proj::Field(i) => PathElem::Field(*i),
                Proj::Downcast(v) => PathElem::Downcast(*v),
                Proj::Deref | Proj::Index(_) => return,
            };
            cur = match self.index.get(&(cur, elem)) {
                Some(c) => *c,
                None => {
                    let id = MovePathId(self.paths.len() as u32);
                    let local = self.paths[cur.0 as usize].local;
                    self.paths.push(MovePath { local, parent: Some(cur), elem: Some(elem), children: Vec::new() });
                    self.paths[cur.0 as usize].children.push(id);
                    self.index.insert((cur, elem), id);
                    id
                }
            };
        }
    }

    fn collect_subtree(&self, p: MovePathId) -> Vec<MovePathId> {
        let mut out = vec![p];
        let mut i = 0;
        while i < out.len() {
            out.extend(self.paths[out[i].0 as usize].children.iter().copied());
            i += 1;
        }
        out
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    pub fn root(&self, l: LocalId) -> MovePathId {
        self.roots[l.0 as usize]
    }

    /// The path and all its descendants.
    pub fn subtree(&self, p: MovePathId) -> &[MovePathId] {
        &self.subtree[p.0 as usize]
    }

    /// Strict ancestors, nearest first.
    pub fn ancestors(&self, p: MovePathId) -> Vec<MovePathId> {
        let mut out = Vec::new();
        let mut cur = self.paths[p.0 as usize].parent;
        while let Some(c) = cur {
            out.push(c);
            cur = self.paths[c.0 as usize].parent;
        }
        out
    }

    pub fn lookup(&self, p: &Place) -> Lookup {
        let mut cur = self.roots[p.local.0 as usize];
        for proj in &p.proj {
            let elem = match proj {
                Proj::Field(i) => PathElem::Field(*i),
                Proj::Downcast(v) => PathElem::Downcast(*v),
                Proj::Deref => return Lookup::Deref(cur),
                Proj::Index(_) => return Lookup::Index(cur),
            };
            match self.index.get(&(cur, elem)) {
                Some(c) => cur = *c,
                // Interned for every place in the function: unreachable.
                None => return Lookup::Exact(cur),
            }
        }
        Lookup::Exact(cur)
    }

    pub fn path(&self, p: MovePathId) -> &MovePath {
        &self.paths[p.0 as usize]
    }
}
