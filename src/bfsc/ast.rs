//! AST types for the BFS (Brainf Script) source language.
//!
//! Consumed by `bfsc::parser` and `bfsc::typeck`; defines the surface syntax
//! (scalar types, expressions, l-values, statements). The codegen layer lowers
//! these nodes to raw Brainfuck by way of `bfsc::layout`.

/// Primitive scalar type in BFS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScalarType {
    /// Unsigned 8-bit integer (1 tape cell).
    U8,
    /// Signed 8-bit integer (1 tape cell).
    I8,
    /// Unsigned 16-bit integer (2 tape cells, little-endian).
    U16,
    /// Signed 16-bit integer (2 tape cells, little-endian).
    I16,
    /// Unsigned 32-bit integer (4 tape cells, little-endian).
    U32,
    /// Signed 32-bit integer (4 tape cells, little-endian).
    I32,
}

impl ScalarType {
    /// Number of Brainfuck tape cells (bytes) used to represent this scalar.
    pub(crate) fn cell_width(self) -> usize {
        match self {
            ScalarType::U8 | ScalarType::I8 => 1,
            ScalarType::U16 | ScalarType::I16 => 2,
            ScalarType::U32 | ScalarType::I32 => 4,
        }
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

/// Source-level type annotation on a `let` binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TypeAnn {
    /// A single scalar cell of the given primitive type.
    Scalar(ScalarType),
    /// A fixed-size array of scalars; the `u32` is the element count.
    Array(ScalarType, u32),
}

/// Binary operators recognised in BFS expressions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BinOp {
    /// Wrapping addition (`+`).
    Add,
    /// Wrapping subtraction (`-`).
    Sub,
    /// Wrapping multiplication (`*`).
    Mul,
    /// Integer division (`/`).
    Div,
    /// Integer remainder (`%`).
    Rem,
    /// Less-than comparison (`<`); result is 0 or 1.
    Lt,
    /// Greater-than comparison (`>`); result is 0 or 1.
    Gt,
    /// Less-or-equal comparison (`<=`); result is 0 or 1.
    Le,
    /// Greater-or-equal comparison (`>=`); result is 0 or 1.
    Ge,
    /// Equality comparison (`==`); result is 0 or 1.
    EqEq,
    /// Inequality comparison (`!=`); result is 0 or 1.
    Ne,
    /// Logical AND (`&&`); operands coerced to 0/1.
    And,
    /// Logical OR (`||`); operands coerced to 0/1.
    Or,
}

/// Unary operators recognised in BFS expressions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnOp {
    /// Arithmetic negation (`-expr`).
    Neg,
    /// Logical NOT (`!expr`); maps 0 → 1 and non-zero → 0.
    Not,
}

/// Expression node in the BFS AST.
#[derive(Debug, Clone)]
pub(crate) enum Expr {
    /// Integer literal (widened to fit the inferred scalar type).
    Int(u64),
    /// Named variable reference.
    Var(String),
    /// Array subscript `name[index]`.
    Index(String, Box<Expr>),
    /// Binary operation between two subexpressions.
    BinOp(BinOp, Box<Expr>, Box<Expr>),
    /// Unary operation applied to a subexpression.
    UnOp(UnOp, Box<Expr>),
    /// Function call `name(args...)`; must resolve to a non-void function.
    Call(String, Vec<Expr>),
}

/// Left-hand side of an assignment or input statement.
#[derive(Debug, Clone)]
pub(crate) enum LValue {
    /// Scalar variable.
    Var(String),
    /// Array element `name[index]`.
    Index(String, Box<Expr>),
}

/// Statement node in the BFS AST.
#[derive(Debug, Clone)]
pub(crate) enum Stmt {
    /// `let name: ty = init?;` — introduce a new variable binding.
    Let {
        /// Variable name.
        name: String,
        /// Declared type annotation.
        ty: TypeAnn,
        /// Optional initialiser expression.
        init: Option<Expr>,
    },
    /// `lval = expr;` — store into a scalar or array element.
    Assign {
        /// Destination l-value (variable or array index).
        lval: LValue,
        /// Right-hand side expression whose value is written.
        expr: Expr,
    },
    /// `while (cond) { body }` — loop until the condition evaluates to zero.
    While {
        /// Loop condition; non-zero means continue.
        cond: Expr,
        /// Loop body statements.
        body: Vec<Stmt>,
    },
    /// `if (cond) { then_ } [else { else_ }]` conditional.
    If {
        /// Branch condition; non-zero selects the `then_` arm.
        cond: Expr,
        /// Statements run when `cond` is non-zero.
        then_: Vec<Stmt>,
        /// Optional `else` arm run when `cond` is zero.
        else_: Option<Vec<Stmt>>,
    },
    /// `scan lval;` — read a decimal integer from stdin into `lval`.
    Scan(LValue),
    /// `print(expr);` — write `expr` as a decimal integer to stdout.
    Print(Expr),
    /// `putchar(expr);` — write `expr` as a raw byte to stdout.
    Putchar(Expr),
    /// `setpixel(x, y, color);` — GUI primitive that writes a palette byte.
    Setpixel {
        /// Column coordinate (0..256).
        x: Expr,
        /// Row coordinate (0..256).
        y: Expr,
        /// RGB332 colour byte.
        color: Expr,
    },
    /// `getchar(lval);` — read one raw byte from stdin into `lval`.
    Getchar(LValue),
    /// `name(args...);` — call a function, discarding any return value.
    Call(String, Vec<Expr>),
    /// `return expr?;` — set the enclosing function's return slot and end its body.
    Return(Option<Expr>),
}

/// One formal parameter in a function definition.
#[derive(Debug, Clone)]
pub(crate) struct Param {
    /// Parameter name as used inside the body.
    pub(crate) name: String,
    /// Declared parameter type (scalar by value; array by reference).
    pub(crate) ty: TypeAnn,
}

/// Top-level function definition in a BFS program.
#[derive(Debug, Clone)]
pub(crate) struct FnDef {
    /// Function name (unique in the program's global namespace).
    pub(crate) name: String,
    /// Ordered parameter list.
    pub(crate) params: Vec<Param>,
    /// Optional scalar return type; `None` for void.
    pub(crate) ret_ty: Option<ScalarType>,
    /// Statements making up the body.
    pub(crate) body: Vec<Stmt>,
}

/// A whole BFS program: a set of top-level functions plus top-level statements.
#[derive(Debug, Clone)]
pub(crate) struct Program {
    /// Function definitions declared at the top level.
    pub(crate) fns: Vec<FnDef>,
    /// Top-level statements executed in the order written.
    pub(crate) top: Vec<Stmt>,
}
