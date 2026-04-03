#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LabelId(pub u32);

#[derive(Debug, Default)]
pub struct LabelGen {
    next: u32,
}

impl LabelGen {
    pub fn new() -> Self {
        Self { next: 0 }
    }

    pub fn fresh(&mut self) -> LabelId {
        let id = LabelId(self.next);
        self.next += 1;
        id
    }
}

/// LIR: Low-level IR
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LirInst {
    /// data pointer += n
    PtrAdd(isize),

    /// *ptr = (*ptr + n) mod 256
    CellAdd(i32),

    /// *ptr = value
    CellSet(u8),

    /// 与 [`crate::ir::hir::HirInst::LinearMul`] 相同语义。
    LinearMul(Vec<(isize, i32)>),

    /// 与 [`crate::ir::hir::HirInst::Scan`] 相同语义（`dir` 为 ±1）。
    Scan(isize),

    /// putchar
    PutByte,

    /// getchar
    GetByte,

    /// unique label
    Label(LabelId),

    /// jump to label if cell == 0
    JumpIfZero(LabelId),

    /// jump to label if cell != 0
    JumpIfNonZero(LabelId),

    /// for debugging
    Comment(String),
}

#[derive(Debug, Clone, Default)]
pub struct LirProgram {
    pub insts: Vec<LirInst>,
}

impl LirProgram {
    pub fn new() -> Self {
        Self { insts: Vec::new() }
    }

    pub fn push(&mut self, inst: LirInst) {
        self.insts.push(inst);
    }

    pub fn extend<I>(&mut self, iter: I)
    where
        I: IntoIterator<Item = LirInst>,
    {
        self.insts.extend(iter);
    }

    pub fn is_empty(&self) -> bool {
        self.insts.is_empty()
    }

    pub fn len(&self) -> usize {
        self.insts.len()
    }
}
