//! Text dump of resolution results, used by `tarn resolve` and snapshot tests.
//!
//! ```text
//! == main
//! 6:14 add -> fn add (main:1:4)
//! 7:11 value -> local value (main:6:5)
//! ```

use crate::*;
use tarn_diagnostics::SourceMap;

fn kind(k: &SymbolKind) -> &'static str {
    match k {
        SymbolKind::Primitive => "primitive",
        SymbolKind::Builtin => "builtin",
        SymbolKind::PreludeType => "prelude-type",
        SymbolKind::Module(ModuleTarget::Std(_)) => "std-module",
        SymbolKind::Module(_) => "module",
        SymbolKind::Function => "fn",
        SymbolKind::Const => "const",
        SymbolKind::Struct => "struct",
        SymbolKind::Enum => "enum",
        SymbolKind::Interface => "interface",
        SymbolKind::Variant { .. } => "variant",
        SymbolKind::Method { .. } => "method",
        SymbolKind::InterfaceMethod { .. } => "interface-method",
        SymbolKind::ImplMethod { .. } => "impl-method",
        SymbolKind::GenericParam => "generic",
        SymbolKind::Param => "param",
        SymbolKind::SelfParam => "self",
        SymbolKind::Local { mutable: true } => "var",
        SymbolKind::Local { mutable: false } => "local",
        SymbolKind::PatternBinding => "binding",
        SymbolKind::LoopBinding => "loop-var",
        SymbolKind::ClosureParam => "closure-param",
    }
}

fn qualified(r: &Resolved, id: SymbolId) -> String {
    let s = r.symbol(id);
    let owner = match &s.kind {
        SymbolKind::Variant { parent } => Some(*parent),
        SymbolKind::Method { owner } => Some(*owner),
        SymbolKind::InterfaceMethod { interface } => Some(*interface),
        SymbolKind::ImplMethod { target, .. } => *target,
        _ => None,
    };
    match owner {
        Some(o) => format!("{}.{}", r.symbol(o).name, s.name),
        None => s.name.clone(),
    }
}

fn location(r: &Resolved, sources: &SourceMap, id: SymbolId) -> String {
    let s = r.symbol(id);
    match (s.module, s.span) {
        (Some(m), Some(span)) => {
            let lc = sources.line_col(span);
            format!(" ({}:{}:{})", r.modules[m.0 as usize].name, lc.line, lc.column)
        }
        _ => " (prelude)".to_string(),
    }
}

pub fn describe(r: &Resolved, sources: &SourceMap, res: &Res) -> String {
    match res {
        Res::Symbol(id) => format!("{} {}{}", kind(&r.symbol(*id).kind), qualified(r, *id), location(r, sources, *id)),
        Res::External { module, path } => format!("std {module}.{}", path.join(".")),
        Res::ScrutineeVariant(n) => format!("scrutinee-variant {n}"),
    }
}

pub fn dump_resolution(r: &Resolved, sources: &SourceMap) -> String {
    let mut out = String::new();
    for (i, info) in r.modules.iter().enumerate() {
        if info.name == crate::prelude::CORE {
            continue;
        }
        out.push_str(&format!("== {}\n", info.name));
        let t = &r.tables[i];
        // One line per distinct (span, resolution): a type path and its
        // `Type` node share both.
        let mut rows: Vec<(u32, String)> = t
            .uses
            .values()
            .map(|u| {
                let lc = sources.line_col(u.span);
                let text = sources.snippet(u.span);
                let text = text.lines().next().unwrap_or("");
                (u.span.start, format!("{}:{} {text} -> {}", lc.line, lc.column, describe(r, sources, &u.res)))
            })
            .collect();
        rows.sort();
        rows.dedup();
        for (_, row) in rows {
            out.push_str(&row);
            out.push('\n');
        }
        let mut caps: Vec<_> = t.captures.values().collect();
        caps.sort_by_key(|c| c.span.start);
        for c in caps {
            let lc = sources.line_col(c.span);
            let names: Vec<_> = c.symbols.iter().map(|&s| r.symbol(s).name.clone()).collect();
            out.push_str(&format!("{}:{} closure captures {}\n", lc.line, lc.column, names.join(", ")));
        }
    }
    out
}
