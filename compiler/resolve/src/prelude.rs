//! The prelude scope and the list of standard-library modules.
//!
//! The prelude is primitives + the few names that still live in the compiler
//! (below) + every `pub` item of the `core` module, which is ordinary Tarn
//! (`stdlib/core/core.tarn`, ADR 0020).

use crate::{Resolved, ScopeId, Symbol, SymbolKind};

pub const PRIMITIVES: &[&str] = &[
    "bool", "i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64", "isize", "usize", "f32", "f64", "string", "void",
    "never",
];

pub const BUILTINS: &[&str] = &["print", "panic", "channel"];

/// Provisional compiler-defined types: `Error` is a placeholder until the
/// stdlib defines it; `Channel`/`Sender` until the concurrency runtime exists.
pub const PRELUDE_TYPES: &[&str] = &["Error", "Channel", "Sender"];

/// Name of the always-loaded module whose `pub` items form the prelude.
pub const CORE: &str = "core";

/// Enums of `core` whose variants are usable unqualified everywhere.
pub const PRELUDE_ENUMS: &[&str] = &["Option", "Result"];

/// Standard-library modules that `import` may name (spec phase 16 list).
/// Their members are not checked yet.
pub const STD_MODULES: &[&str] = &[
    "core", "string", "runtime", "collections", "fs", "path", "process", "io", "time", "json", "http", "net", "tls", "testing", "ffi", "arch_x86_64", "arch_aarch64", "os",
    "logging", "crypto", "encoding", "compression", "cli", "os",
];

pub fn build(r: &mut Resolved) -> ScopeId {
    let scope = r.add_scope(crate::ScopeKind::Prelude, None, None);
    let mk = |name: &str, kind: SymbolKind| Symbol {
        name: name.to_string(),
        kind,
        module: None,
        def: None,
        span: None,
        scope,
        is_pub: true,
        body: None,
    };
    for p in PRIMITIVES {
        r.declare(scope, mk(p, SymbolKind::Primitive));
    }
    for b in BUILTINS {
        r.declare(scope, mk(b, SymbolKind::Builtin));
    }
    for t in PRELUDE_TYPES {
        let (id, _) = r.declare(scope, mk(t, SymbolKind::PreludeType));
        let arity = match *t {
            "Channel" | "Sender" => 1,
            _ => 0,
        };
        r.type_arity.insert(id, arity);
    }
    scope
}
