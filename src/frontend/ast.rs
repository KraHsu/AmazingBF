#[derive(Debug, Clone)]
pub enum AstNode {
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
