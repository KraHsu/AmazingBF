//! Brainfuck AST.
//!
//! Minimal nested tree produced by `frontend::parser` from a token stream.
//! Loops recurse via `AstNode::Loop`; the tree is the sole input to
//! `ir::lower::lower_to_hir`.

/// Single Brainfuck AST node produced by `frontend::parser`.
#[derive(Debug, Clone)]
pub(crate) enum AstNode {
    /// Net pointer shift from one or more consecutive `>` / `<`.
    Move(isize),
    /// Net cell delta from one or more consecutive `+` / `-`.
    Add(i32),
    /// .
    Output,
    /// ,
    Input,
    /// [ ... ]
    Loop(Vec<AstNode>),
}
