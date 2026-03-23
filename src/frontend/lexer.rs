#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    MoveRight,
    MoveLeft,
    Inc,
    Dec,
    Output,
    Input,
    LoopStart,
    LoopEnd,
}

pub fn lex(input: &str) -> Vec<Token> {
    input
        .chars()
        .filter_map(|ch| match ch {
            '>' => Some(Token::MoveRight),
            '<' => Some(Token::MoveLeft),
            '+' => Some(Token::Inc),
            '-' => Some(Token::Dec),
            '.' => Some(Token::Output),
            ',' => Some(Token::Input),
            '[' => Some(Token::LoopStart),
            ']' => Some(Token::LoopEnd),
            _ => None, // ignore other characters
        })
        .collect()
}
