//! The single 64-bit Linux (x86_64/AArch64) layout authority for native code and runtime ABI.
//! It lives beside the IR so the frontend can report the same limits the
//! backend enforces (E3075) without a second copy of the layout rules.
use tarn_types::{Ty, Typed};

#[derive(Clone, Debug, PartialEq)]
pub struct Error {
    pub message: String,
    /// A deliberate v0 size/shape limit, as opposed to an unsupported type.
    pub limit: bool,
}
impl Error {
    fn unsupported(s: impl Into<String>) -> Self {
        Self { message: s.into(), limit: false }
    }
    fn limit(s: impl Into<String>) -> Self {
        Self { message: s.into(), limit: true }
    }
}
pub type Result<T> = std::result::Result<T, Error>;

/// Largest value the backend copies and destroys field by field.
pub const MAX_VALUE_SIZE: u32 = 65536;
/// Largest fixed array element count.
pub const MAX_ARRAY_LEN: u64 = 4096;
/// Deepest by-value aggregate nesting.
pub const MAX_NESTING: usize = 64;

#[derive(Clone, Debug)]
pub struct Layout {
    pub size: u32,
    pub align: u32,
    pub fields: Vec<(u32, Ty)>,
    pub variants: Vec<Vec<(u32, Ty)>>,
}
fn align(n: u32, a: u32) -> Result<u32> {
    n.checked_add(a - 1).map(|x| x & !(a - 1)).ok_or_else(|| Error::limit("layout exceeds 32-bit object size"))
}
/// Layout of a `static` (ADR 0101, P6): the same field order and alignment, but
/// no per-value size limits, since statics are never copied or dropped field by
/// field. Kernel stacks and page tables are far larger than 64 KiB.
pub fn static_layout(t: &Typed, ty: &Ty) -> Result<Layout> {
    layout_inner(t, ty, &mut Vec::new(), u64::MAX)
}
pub fn layout(t: &Typed, ty: &Ty) -> Result<Layout> {
    let l = layout_inner(t, ty, &mut Vec::new(), MAX_ARRAY_LEN)?;
    // Correctness-first explicit copies/drop glue are bounded, not silently
    // expanded into millions of instructions for huge fixed-size objects.
    if l.size > MAX_VALUE_SIZE {
        return Err(Error::limit("native value size exceeds v0 limit of 64 KiB"));
    }
    Ok(l)
}
fn layout_inner(t: &Typed, ty: &Ty, seen: &mut Vec<Ty>, max_array: u64) -> Result<Layout> {
    if seen.len() >= MAX_NESTING {
        return Err(Error::limit("native aggregate nesting exceeds 64 levels"));
    }
    if seen.contains(ty) {
        return Err(Error::unsupported("recursive by-value layout"));
    }
    let scalar = |size| Layout { size, align: size.max(1), fields: Vec::new(), variants: Vec::new() };
    Ok(match ty {
        Ty::Void | Ty::Never => scalar(0),
        Ty::Bool => scalar(1),
        Ty::Int(i) => scalar((int_bytes(*i)) as u32),
        Ty::Float(tarn_types::FloatTy::F32) => scalar(4),
        Ty::Float(_) => scalar(8),
        Ty::Str | Ty::Ptr(..) => scalar(8),
        Ty::Ref(_, inner) => {
            if matches!(inner.as_ref(), Ty::Slice(_) | Ty::Any(_)) {
                Layout { size: 16, align: 8, fields: Vec::new(), variants: Vec::new() }
            } else {
                scalar(8)
            }
        }
        Ty::Fn(..) | Ty::Async(_) => Layout { size: 16, align: 8, fields: Vec::new(), variants: Vec::new() },
        // Zero-length markers (`Vec<T>.elements [0]T`, `AsyncTask<R>.result
        // [0]R`) store nothing inline: elements live behind the header's
        // pointer. Laying out their element would report false by-value
        // recursion for `enum Value { List(Vec<Value>) }`, directly or through
        // another aggregate. Every marker sits beside usize header fields, and
        // no native type aligns beyond 8, so alignment 1 changes no layout.
        Ty::Array(_, 0) => scalar(0),
        Ty::Array(elem, n) => {
            if *n > max_array {
                return Err(Error::limit("fixed arrays exceed v0 limit of 4096 elements"));
            }
            let el = layout_inner(t, elem, seen, max_array)?;
            let size = u64::from(el.size).checked_mul(*n).and_then(|n| u32::try_from(n).ok()).ok_or_else(|| Error::limit("array layout too large"))?;
            Layout { size, align: el.align, fields: Vec::new(), variants: Vec::new() }
        }
        Ty::Adt(s, args) => {
            seen.push(ty.clone());
            let result = if let Some(def) = t.decls.structs.get(s) {
                let map = def.generics.iter().copied().zip(args.iter().cloned()).collect();
                let ts: Vec<_> = def.fields.iter().map(|f| tarn_types::subst(&f.ty, &map)).collect();
                record(t, &ts, seen, max_array)?
            } else if let Some(def) = t.decls.enums.get(s) {
                let map = def.generics.iter().copied().zip(args.iter().cloned()).collect();
                let mut variants = Vec::new();
                let mut size = 4;
                let mut alignment = 4;
                for v in &def.variants {
                    let ts: Vec<_> = v.fields.iter().map(|ty| tarn_types::subst(ty, &map)).collect();
                    let payload = record(t, &ts, seen, max_array)?;
                    alignment = alignment.max(payload.align);
                    variants.push(payload);
                }
                // u32 declaration-index tag, followed by an aligned payload union.
                let offset = align(4, alignment)?;
                let fields = variants
                    .into_iter()
                    .map(|v| {
                        size = size.max(offset + v.size);
                        v.fields.into_iter().map(|(o, ty)| (offset + o, ty)).collect()
                    })
                    .collect();
                Layout { size: align(size, alignment)?, align: alignment, fields: Vec::new(), variants: fields }
            } else {
                return Err(Error::unsupported("opaque/prelude resource layout"));
            };
            seen.pop();
            result
        }
        other => return Err(Error::unsupported(format!("type {other:?}"))),
    })
}
fn record(t: &Typed, ts: &[Ty], seen: &mut Vec<Ty>, max_array: u64) -> Result<Layout> {
    let mut fields = Vec::new();
    let mut size = 0u32;
    let mut alignment = 1;
    for ty in ts {
        let l = layout_inner(t, ty, seen, max_array)?;
        size = align(size, l.align)?;
        fields.push((size, ty.clone()));
        size = size.checked_add(l.size).ok_or_else(|| Error::limit("struct layout too large"))?;
        alignment = alignment.max(l.align);
    }
    Ok(Layout { size: align(size, alignment)?, align: alignment, fields, variants: Vec::new() })
}

fn int_bytes(i: tarn_types::IntTy) -> u32 {
    use tarn_types::IntTy::*;
    match i {
        I8 | U8 => 1,
        I16 | U16 => 2,
        I32 | U32 => 4,
        _ => 8,
    }
}
