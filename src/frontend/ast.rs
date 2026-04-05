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
