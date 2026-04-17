#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScalarType {
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
}

impl ScalarType {
    pub(crate) fn cell_width(self) -> usize {
        match self {
            ScalarType::U8 | ScalarType::I8 => 1,
            ScalarType::U16 | ScalarType::I16 => 2,
            ScalarType::U32 | ScalarType::I32 => 4,
        }
    }

    pub(crate) fn is_signed(self) -> bool {
        matches!(self, ScalarType::I8 | ScalarType::I16 | ScalarType::I32)
    }
}

impl std::fmt::Display for ScalarType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScalarType::U8 => write!(f, "u8"),
            ScalarType::I8 => write!(f, "i8"),
            ScalarType::U16 => write!(f, "u16"),
            ScalarType::I16 => write!(f, "i16"),
            ScalarType::U32 => write!(f, "u32"),
            ScalarType::I32 => write!(f, "i32"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TypeAnn {
    Scalar(ScalarType),
    Array(ScalarType, u32),
}

impl TypeAnn {
    pub(crate) fn scalar_type(&self) -> ScalarType {
        match self {
            TypeAnn::Scalar(t) | TypeAnn::Array(t, _) => *t,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Lt,
    Gt,
    Le,
    Ge,
    EqEq,
    Ne,
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnOp {
    Neg,
    Not,
}

#[derive(Debug, Clone)]
pub(crate) enum Expr {
    Int(u64),
    Var(String),
    Index(String, Box<Expr>),
    BinOp(BinOp, Box<Expr>, Box<Expr>),
    UnOp(UnOp, Box<Expr>),
}

#[derive(Debug, Clone)]
pub(crate) enum LValue {
    Var(String),
    Index(String, Box<Expr>),
}

#[derive(Debug, Clone)]
pub(crate) enum Stmt {
    Let {
        name: String,
        ty: TypeAnn,
        init: Option<Expr>,
    },
    Assign {
        lval: LValue,
        expr: Expr,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
    If {
        cond: Expr,
        then_: Vec<Stmt>,
        else_: Option<Vec<Stmt>>,
    },
    Scan(LValue),
    Print(Expr),
    Putchar(Expr),
}
