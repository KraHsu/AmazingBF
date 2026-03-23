use crate::frontend::ast::AstNode;
use crate::frontend::lexer::Token;

#[derive(Debug)]
pub enum ParseError {
    UnexpectedLoopEnd,
    UnclosedLoop,
}

/// parse token list to ast
pub fn parse(tokens: &[Token]) -> Result<Vec<AstNode>, ParseError> {
    let mut pos = 0;
    let ast = parse_block(tokens, &mut pos, false)?;

    if pos != tokens.len() {
        return Err(ParseError::UnexpectedLoopEnd);
    }

    Ok(ast)
}

/// in_loop : Indicates whether parsing is currently inside a loop section
/// - false: at the outermost level
/// - true: inside the \[...\] section
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
                    return Err(ParseError::UnexpectedLoopEnd);
                }
            }
        }

        *pos += 1;
    }

    if in_loop {
        Err(ParseError::UnclosedLoop)
    } else {
        Ok(nodes)
    }
}
