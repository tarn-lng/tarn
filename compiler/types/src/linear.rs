//! Compile-time containment of must-consume obligations (ADR 0108).
use crate::{Decls, ParamId, Ty};
use std::collections::HashSet;

impl Decls {
    pub fn is_must_consume(&self, ty: &Ty) -> bool {
        self.is_must_consume_with(ty, &HashSet::new())
    }

    /// Generic ownership analysis substitutes obligation bits, not layouts.
    /// References and raw pointers do not own their referents.
    pub fn is_must_consume_with(&self, ty: &Ty, linear: &HashSet<ParamId>) -> bool {
        fn visit(
            d: &Decls,
            ty: &Ty,
            linear: &HashSet<ParamId>,
            seen: &mut HashSet<Ty>,
            budget: &mut usize,
        ) -> bool {
            if *budget == 0 {
                return true;
            }
            *budget -= 1;
            if !seen.insert(ty.clone()) {
                return false;
            }
            match ty {
                Ty::Param(p) => linear.contains(p),
                Ty::Array(e, n) => *n != 0 && visit(d, e, linear, seen, budget),
                Ty::Async(e) => visit(d, e, linear, seen, budget),
                Ty::Adt(s, args) => {
                    if d.must_consume.contains(s) {
                        return true;
                    }
                    // Generic arguments preserve obligations even for opaque,
                    // native containers whose stored elements are not fields.
                    if args.iter().any(|a| visit(d, a, linear, seen, budget)) {
                        return true;
                    }
                    if let Some(def) = d.structs.get(s) {
                        let map = def
                            .generics
                            .iter()
                            .copied()
                            .zip(args.iter().cloned())
                            .collect();
                        def.fields
                            .iter()
                            .any(|f| visit(d, &crate::subst(&f.ty, &map), linear, seen, budget))
                    } else if let Some(def) = d.enums.get(s) {
                        let map = def
                            .generics
                            .iter()
                            .copied()
                            .zip(args.iter().cloned())
                            .collect();
                        def.variants
                            .iter()
                            .flat_map(|v| &v.fields)
                            .any(|f| visit(d, &crate::subst(f, &map), linear, seen, budget))
                    } else {
                        false
                    }
                }
                _ => false,
            }
        }
        if self.must_consume.is_empty() {
            return false;
        }
        visit(self, ty, linear, &mut HashSet::new(), &mut 1024)
    }
}
