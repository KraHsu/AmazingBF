/// HIR: High-level IR
///
/// Only a minimal instruction set for now.
#[derive(Debug, Clone)]
pub enum HirInst {
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

    /// A loop containing nested instructions.
    Loop(Vec<HirInst>),
}

#[derive(Debug, Clone)]
pub struct Program {
    pub insts: Vec<HirInst>,
}
