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
pub struct HirProgram {
    pub insts: Vec<HirInst>,
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
    pub fn has_put_byte(&self) -> bool {
        block_has_put_byte(&self.insts)
    }

    /// True if the program contains at least one `,` (possibly nested in a loop).
    pub fn has_get_byte(&self) -> bool {
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
