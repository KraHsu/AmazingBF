//! Brainfuck AST.
//!
//! Minimal nested tree produced by `frontend::parser` from a token stream.
//! Loops recurse via `AstNode::Loop`; the tree is the sole input to
//! `ir::lower::lower_to_hir`.

/// Single Brainfuck AST node produced by `frontend::parser`.
#[derive(Debug, Clone)]
pub(crate) enum AstNode {
    /// >
    MoveRight,
    /// <
    MoveLeft,
    /// +
    Inc,
    /// -
    Dec,
    /// .
    Output,
    /// ,
    Input,
    /// [ ... ]
    Loop(Vec<AstNode>),
}
