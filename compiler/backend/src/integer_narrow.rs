//! Machine-level width reduction for unsigned division and remainder.
//!
//! Only facts explicit in the current CLIF value definition are used. Unknown
//! values (including loop block parameters) retain their original width. This
//! does not change the language type or remove any arithmetic fault checks.
use cranelift_codegen::ir::{self as cl, types, InstBuilder};
use cranelift_frontend::FunctionBuilder;

fn constant(dfg: &cl::DataFlowGraph, value: cl::Value) -> Option<u64> {
    let value = dfg.resolve_aliases(value);
    let cl::ValueDef::Result(inst, _) = dfg.value_def(value) else {
        return None;
    };
    match dfg.insts[inst] {
        cl::InstructionData::UnaryImm {
            opcode: cl::Opcode::Iconst,
            imm,
        } => {
            let bits = dfg.value_type(value).bits();
            let mask = if bits < 64 {
                (1u64 << bits) - 1
            } else {
                u64::MAX
            };
            Some(imm.bits() as u64 & mask)
        }
        _ => None,
    }
}

/// A definition-local proof, with no speculative reasoning through memory,
/// arithmetic with overflow, or cyclic SSA block parameters.
fn fits_u32(dfg: &cl::DataFlowGraph, value: cl::Value) -> bool {
    let value = dfg.resolve_aliases(value);
    if dfg.value_type(value).is_int() && dfg.value_type(value).bits() <= 32 {
        return true;
    }
    if let Some(value) = constant(dfg, value) {
        return value <= u64::from(u32::MAX);
    }
    let cl::ValueDef::Result(inst, _) = dfg.value_def(value) else {
        return false;
    };
    match dfg.insts[inst] {
        cl::InstructionData::Unary {
            opcode: cl::Opcode::Uextend,
            arg,
        } => dfg.value_type(arg).bits() <= 32,
        cl::InstructionData::Binary {
            opcode: cl::Opcode::Urem,
            args,
        } => {
            // For every successful remainder, 0 <= result < divisor.
            constant(dfg, args[1]).is_some_and(|d| d != 0 && d <= 1u64 << 32)
        }
        cl::InstructionData::Binary {
            opcode: cl::Opcode::Band,
            args,
        } => args
            .iter()
            .any(|arg| constant(dfg, *arg).is_some_and(|mask| mask <= u64::from(u32::MAX))),
        _ => false,
    }
}

/// Called only after the ordinary divide-by-zero check has been emitted.
/// Narrow both operands only if neither can lose a bit, and zero-extend the
/// result so its original i64 representation and all consumers stay unchanged.
pub(crate) fn unsigned_divrem(
    b: &mut FunctionBuilder<'_>,
    x: cl::Value,
    y: cl::Value,
    remainder: bool,
) -> cl::Value {
    // Modulo 2^32 is exactly truncation followed by zero extension. Keeping
    // that fact explicit (instead of waiting for a later i64 mask rewrite)
    // lets the optimizer share narrow arithmetic with other i32 consumers.
    if remainder
        && b.func.dfg.value_type(x) == types::I64
        && constant(&b.func.dfg, y) == Some(1u64 << 32)
    {
        let low = b.ins().ireduce(types::I32, x);
        return b.ins().uextend(types::I64, low);
    }
    if b.func.dfg.value_type(x) == types::I64
        && fits_u32(&b.func.dfg, x)
        && fits_u32(&b.func.dfg, y)
    {
        let x = b.ins().ireduce(types::I32, x);
        let y = b.ins().ireduce(types::I32, y);
        let value = if remainder {
            b.ins().urem(x, y)
        } else {
            b.ins().udiv(x, y)
        };
        b.ins().uextend(types::I64, value)
    } else if remainder {
        b.ins().urem(x, y)
    } else {
        b.ins().udiv(x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranelift_frontend::FunctionBuilderContext;

    #[test]
    fn unsigned_bounds_keep_high_bits_and_zero_divisors_conservative() {
        let mut f = cl::Function::new();
        let mut context = FunctionBuilderContext::new();
        let mut b = FunctionBuilder::new(&mut f, &mut context);
        let block = b.create_block();
        let unknown = b.append_block_param(block, types::I64);
        b.switch_to_block(block);
        assert!(!fits_u32(&b.func.dfg, unknown));
        for (value, fits) in [
            (0, true),
            (u32::MAX as i64, true),
            (1i64 << 32, false),
            (-1, false),
            (i64::MIN, false),
        ] {
            let value = b.ins().iconst(types::I64, value);
            assert_eq!(fits_u32(&b.func.dfg, value), fits);
        }
        for (divisor, fits) in [
            (0, false),
            (1, true),
            (1i64 << 32, true),
            ((1i64 << 32) + 1, false),
            (-1, false),
        ] {
            let divisor = b.ins().iconst(types::I64, divisor);
            let value = b.ins().urem(unknown, divisor);
            assert_eq!(fits_u32(&b.func.dfg, value), fits);
        }
        let small_mask = b.ins().iconst(types::I64, u32::MAX as i64);
        let masked = b.ins().band(unknown, small_mask);
        assert!(fits_u32(&b.func.dfg, masked));
        let large_mask = b.ins().iconst(types::I64, (1i64 << 32) | 7);
        let masked = b.ins().band(large_mask, unknown);
        assert!(!fits_u32(&b.func.dfg, masked));
        let narrow = b.ins().ireduce(types::I32, unknown);
        let unsigned = b.ins().uextend(types::I64, narrow);
        let signed = b.ins().sextend(types::I64, narrow);
        assert!(fits_u32(&b.func.dfg, unsigned));
        assert!(!fits_u32(&b.func.dfg, signed));
    }

    #[test]
    fn narrowing_requires_both_operands_and_preserves_the_result_width() {
        let mut f = cl::Function::new();
        let mut context = FunctionBuilderContext::new();
        let mut b = FunctionBuilder::new(&mut f, &mut context);
        let block = b.create_block();
        let unknown = b.append_block_param(block, types::I64);
        b.switch_to_block(block);
        let boundary = b.ins().iconst(types::I64, 1i64 << 32);
        let x = b.ins().urem(unknown, boundary);
        let wide_divisor = b.ins().iconst(types::I64, (1i64 << 32) + 1);
        let y = b.ins().iconst(types::I64, 1000);
        for remainder in [false, true] {
            let result = unsigned_divrem(&mut b, x, y, remainder);
            assert_eq!(b.func.dfg.value_type(result), types::I64);
            let cl::ValueDef::Result(inst, _) = b.func.dfg.value_def(result) else {
                panic!("not an instruction")
            };
            assert_eq!(b.func.dfg.insts[inst].opcode(), cl::Opcode::Uextend);
            let wide = unsigned_divrem(&mut b, x, wide_divisor, remainder);
            let cl::ValueDef::Result(inst, _) = b.func.dfg.value_def(wide) else {
                panic!("not an instruction")
            };
            assert_eq!(
                b.func.dfg.insts[inst].opcode(),
                if remainder {
                    cl::Opcode::Urem
                } else {
                    cl::Opcode::Udiv
                }
            );
            let wide = unsigned_divrem(&mut b, unknown, y, remainder);
            let cl::ValueDef::Result(inst, _) = b.func.dfg.value_def(wide) else {
                panic!("not an instruction")
            };
            assert_eq!(
                b.func.dfg.insts[inst].opcode(),
                if remainder {
                    cl::Opcode::Urem
                } else {
                    cl::Opcode::Udiv
                }
            );
        }
    }
}
