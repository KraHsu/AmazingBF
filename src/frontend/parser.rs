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
            Token::MoveRight | Token::MoveLeft => {
                let delta = parse_move_run(tokens, pos);
                if delta != 0 {
                    nodes.push(AstNode::Move(delta));
                }
                continue;
            }
            Token::Inc | Token::Dec => {
                let delta = parse_add_run(tokens, pos);
                if delta != 0 {
                    nodes.push(AstNode::Add(delta));
                }
                continue;
            }
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

fn parse_move_run(tokens: &[Token], pos: &mut usize) -> isize {
    let mut delta = 0isize;
    while *pos < tokens.len() {
        match tokens[*pos] {
            Token::MoveRight => delta += 1,
            Token::MoveLeft => delta -= 1,
            _ => break,
        }
        *pos += 1;
    }
    delta
}

fn parse_add_run(tokens: &[Token], pos: &mut usize) -> i32 {
    let mut delta = 0i32;
    while *pos < tokens.len() {
        match tokens[*pos] {
            Token::Inc => delta += 1,
            Token::Dec => delta -= 1,
            _ => break,
        }
        *pos += 1;
    }
    delta
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_tokens(tokens: &[Token]) -> Vec<AstNode> {
        parse(tokens).expect("parse should succeed")
    }

    #[test]
    fn compresses_add_runs_and_drops_zero_net_result() {
        let tokens = [Token::Inc, Token::Inc, Token::Dec, Token::Dec, Token::Inc];
        assert!(matches!(
            parse_tokens(&tokens).as_slice(),
            [AstNode::Add(1)]
        ));
    }

    #[test]
    fn compresses_move_runs_and_drops_zero_net_result() {
        let tokens = [
            Token::MoveRight,
            Token::MoveRight,
            Token::MoveLeft,
            Token::MoveLeft,
            Token::MoveRight,
            Token::MoveRight,
        ];
        assert!(matches!(
            parse_tokens(&tokens).as_slice(),
            [AstNode::Move(2)]
        ));
    }

    #[test]
    fn does_not_merge_across_io_barriers() {
        let tokens = [Token::Inc, Token::Inc, Token::Output, Token::Inc];
        let ast = parse_tokens(&tokens);
        assert!(matches!(
            ast.as_slice(),
            [AstNode::Add(2), AstNode::Output, AstNode::Add(1)]
        ));
    }

    #[test]
    fn compresses_inside_loops() {
        let tokens = [
            Token::LoopStart,
            Token::Dec,
            Token::MoveRight,
            Token::Inc,
            Token::MoveLeft,
            Token::LoopEnd,
        ];
        let ast = parse_tokens(&tokens);
        assert!(matches!(
            ast.as_slice(),
            [AstNode::Loop(body)]
                if matches!(
                    body.as_slice(),
                    [AstNode::Add(-1), AstNode::Move(1), AstNode::Add(1), AstNode::Move(-1)]
                )
        ));
    }

    #[test]
    fn unexpected_loop_end_reports_same_token_index() {
        let err = parse(&[Token::Inc, Token::LoopEnd]).expect_err("should fail");
        assert!(matches!(err, ParseError::UnexpectedLoopEnd { pos: 1 }));
    }

    #[test]
    fn unclosed_loop_reports_end_position() {
        let err = parse(&[Token::LoopStart, Token::Inc]).expect_err("should fail");
        assert!(matches!(err, ParseError::UnclosedLoop { pos: 2 }));
    }
}
