//! HIR (High-level IR) for Brainfuck programs.
//!
//! Flattens the AST into a minimal instruction set that supports peephole
//! fusion (`Move`, `Add`), loop-shape recognition (`Zero`, `Scan`,
//! `LinearMul`), and whole-program queries such as `has_put_byte`. HIR is the
//! input to `ir::optimize` and both the interpreter and LIR lowering.

/// Individual HIR instruction after AST lowering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HirInst {
    /// Moves the data pointer. Positive values move it to the right,
    /// while negative values move it to the left.
    Move(isize),

    /// Adds to or subtracts from the current cell. Positive values
    /// increment it, and negative values decrement it.
    Add(i32),

    /// Writes the value of the current cell as a byte.
    PutByte,

    /// Reads one byte into the current cell.
    GetByte,

    /// Set the current cell to 0 (strength-reduced from a `[-]`-style clear loop at `-O1`).
    Zero,

    /// `v = *p; *p = 0;` then for each `(off, f)` in order: `*(p+off) += v * f` (8-bit wrapping).
    /// Produced from simple affine loops like `[->+<]` / `[->+>+<<]` at `-O1`.
    LinearMul(Vec<(isize, i32)>),

    /// While the current cell is non-zero, move the pointer by `dir` (−1 or +1). Matches `[<]` / `[>]`.
    Scan(isize),

    /// A loop containing nested instructions.
    Loop(Vec<HirInst>),
}

/// Complete HIR program as a flat instruction sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HirProgram {
    /// Top-level instruction list; `Loop` variants carry their own nested bodies.
    pub(crate) insts: Vec<HirInst>,
}

fn block_has_put_byte(insts: &[HirInst]) -> bool {
    for inst in insts {
        match inst {
            HirInst::PutByte => return true,
            HirInst::Loop(body) => {
                if block_has_put_byte(body) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn block_has_get_byte(insts: &[HirInst]) -> bool {
    for inst in insts {
        match inst {
            HirInst::GetByte => return true,
            HirInst::Loop(body) => {
                if block_has_get_byte(body) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

impl HirProgram {
    /// True if the program contains at least one `.` (possibly nested in a loop).
    pub(crate) fn has_put_byte(&self) -> bool {
        block_has_put_byte(&self.insts)
    }

    /// True if the program contains at least one `,` (possibly nested in a loop).
    pub(crate) fn has_get_byte(&self) -> bool {
        block_has_get_byte(&self.insts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_flags_ignore_comment_only_ops() {
        let p = HirProgram {
            insts: vec![HirInst::Add(1), HirInst::Move(1)],
        };
        assert!(!p.has_put_byte());
        assert!(!p.has_get_byte());
    }

    #[test]
    fn io_flags_see_nested_io() {
        let p = HirProgram {
            insts: vec![HirInst::Loop(vec![HirInst::PutByte, HirInst::GetByte])],
        };
        assert!(p.has_put_byte());
        assert!(p.has_get_byte());
    }
}
