//! Brainfuck parser.
//!
//! Builds a nested `Vec<AstNode>` from a flat token stream, balancing `[` / `]`
//! into `AstNode::Loop` subtrees. The returned tree is consumed by HIR lowering
//! and is the last place where source positions (byte offsets) are tracked.

use crate::frontend::ast::AstNode;
use crate::frontend::lexer::Token;

/// Errors raised by [`parse`] when a Brainfuck token stream is structurally invalid.
#[derive(Debug)]
pub enum ParseError {
    /// A `]` appeared without a matching `[`.
    UnexpectedLoopEnd {
        /// Token index of the offending `]`.
        pos: usize,
    },
    /// A `[` was never closed by a matching `]`.
    UnclosedLoop {
        /// Token index at which parsing reached end-of-input while still inside a loop.
        pos: usize,
    },
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::UnexpectedLoopEnd { pos } => {
                write!(f, "unexpected loop end ']' at position {pos}")
            }
            ParseError::UnclosedLoop { pos } => {
                write!(f, "unclosed loop '[' starting at position {pos}")
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// Parse the Brainfuck token stream into the nested AST representation.
pub(crate) fn parse(tokens: &[Token]) -> Result<Vec<AstNode>, ParseError> {
    let mut pos = 0;
    let ast = parse_block(tokens, &mut pos, false)?;

    if pos != tokens.len() {
        return Err(ParseError::UnexpectedLoopEnd { pos });
    }

    Ok(ast)
}

/// Parse one block, optionally inside a `[...]` loop body.
fn parse_block(
    tokens: &[Token],
    pos: &mut usize,
    in_loop: bool,
) -> Result<Vec<AstNode>, ParseError> {
    let mut nodes = Vec::new();

    while *pos < tokens.len() {
        match tokens[*pos] {
            Token::MoveRight => nodes.push(AstNode::MoveRight),
            Token::MoveLeft => nodes.push(AstNode::MoveLeft),
            Token::Inc => nodes.push(AstNode::Inc),
            Token::Dec => nodes.push(AstNode::Dec),
            Token::Output => nodes.push(AstNode::Output),
            Token::Input => nodes.push(AstNode::Input),

            Token::LoopStart => {
                *pos += 1; // skip '['
                let body = parse_block(tokens, pos, true)?;
                nodes.push(AstNode::Loop(body));
                continue;
            }

            Token::LoopEnd => {
                if in_loop {
                    *pos += 1; // consume ']'
                    return Ok(nodes);
                } else {
                    return Err(ParseError::UnexpectedLoopEnd { pos: *pos });
                }
            }
        }

        *pos += 1;
    }

    if in_loop {
        Err(ParseError::UnclosedLoop { pos: *pos })
    } else {
        Ok(nodes)
    }
}
