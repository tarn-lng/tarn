//! Ownership analysis on the typed IR.
//!
//! Phase 6A: move/initialization checking — use after
//! move, possibly moved values at joins, uninitialized values, partial moves,
//! reinitialization — plus the information drop elaboration needs.
//! Phase 6B (borrow checking) builds on the same move paths.
//! Phase 6C elaborates executable drops after both analyses succeed.

mod borrows;
mod drops;
mod linear;
mod moves;
mod paths;
mod util;

pub use borrows::{BorrowResults, FnBorrows, Loan, LoanKind, Provenance, check_borrows, overlap};
pub use drops::elaborate_drops;
pub use paths::{Lookup, MovePathId, MovePaths, PathElem};

use std::collections::HashMap;
use tarn_diagnostics::Diagnostic;
use tarn_ir::{BlockId, FunctionId, Program};
use tarn_resolve::Resolved;
use tarn_types::Typed;

/// What drop elaboration must do with one `Drop(place)` (ADR 0024).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropDecision {
    /// Initialized on every path: drop unconditionally.
    Static,
    /// Moved or uninitialized on every path: remove the drop.
    Dead,
    /// Initialized on some paths only: needs a runtime drop flag.
    Conditional,
    /// The value exists but these fields were moved on every path: drop the
    /// remaining fields only.
    Partial(Vec<String>),
}

/// Initialization of a move path at an abstract drop site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitState { Live, Dead, Maybe }

#[derive(Debug, Default)]
pub struct FnMoves {
    /// `(block, statement index, decision)` for every `Drop` statement.
    pub drops: Vec<(BlockId, usize, DropDecision)>,
    /// Sparse move-path state at each drop, including the local root. Missing
    /// fields inherit their nearest tracked ancestor (paths are structural).
    pub drop_states: HashMap<(BlockId, usize), Vec<(tarn_ir::Place, InitState)>>,
    /// Move/init errors were reported in this function (6B skips it).
    pub has_errors: bool,
}

#[derive(Debug, Default)]
pub struct MoveResults {
    pub functions: HashMap<FunctionId, FnMoves>,
}

impl MoveResults {
    /// Per-statement notes for `tarn_ir::print_program_annotated`.
    pub fn drop_notes(&self) -> HashMap<(FunctionId, BlockId, usize), String> {
        let mut out = HashMap::new();
        for (f, m) in &self.functions {
            for (b, i, d) in &m.drops {
                let note = match d {
                    DropDecision::Static => "drop: static".to_string(),
                    DropDecision::Dead => "drop: dead (moved)".to_string(),
                    DropDecision::Conditional => "drop: conditional (flag)".to_string(),
                    DropDecision::Partial(fs) => format!("drop: partial, skip {}", fs.join(", ")),
                };
                out.insert((*f, *b, *i), note);
            }
        }
        out
    }
}

/// Functions with move/init errors.
impl MoveResults {
    pub fn failed(&self) -> std::collections::HashSet<FunctionId> {
        self.functions.iter().filter(|(_, m)| m.has_errors).map(|(f, _)| *f).collect()
    }
}

/// Run the move/init checker on every function of the program.
pub fn check_moves(p: &Program, r: &Resolved, t: &Typed) -> (MoveResults, Vec<Diagnostic>) {
    let mut results = MoveResults::default();
    let mut diags = Vec::new();
    let checked = util::par_map(p.functions.iter().collect(), |f| {
        (f.id, moves::check_function(f, r, t))
    });
    for (id, (mut m, mut d)) in checked {
        m.has_errors = !d.is_empty();
        diags.append(&mut d);
        results.functions.insert(id, m);
    }
    let instantiated = linear::check_instances(p, r, t);
    if !instantiated.is_empty() {
        // Generic obligation failures stop the pipeline just like ordinary
        // move errors; drop elaboration must never see an invalid instance.
        for m in results.functions.values_mut() {
            m.has_errors = true;
        }
        diags.extend(instantiated);
    }
    (results, diags)
}
