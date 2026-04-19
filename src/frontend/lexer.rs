//! Brainfuck tokenizer.
//!
//! Maps the eight BF operators onto `Token` values; every other character is
//! silently discarded, matching the convention that any non-operator text is
//! a comment.

/// One of the eight canonical Brainfuck operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Token {
    /// `>` — shift the data pointer one cell to the right.
    MoveRight,
    /// `<` — shift the data pointer one cell to the left.
    MoveLeft,
    /// `+` — increment the current cell (wrapping).
    Inc,
    /// `-` — decrement the current cell (wrapping).
    Dec,
    /// `.` — write the current cell as a byte to stdout.
    Output,
    /// `,` — read one byte from stdin into the current cell.
    Input,
    /// `[` — begin a loop (skip body if current cell is zero).
    LoopStart,
    /// `]` — end a loop (jump back if current cell is non-zero).
    LoopEnd,
}

/// Tokenise a Brainfuck source string, silently dropping non-operator characters.
pub(crate) fn lex(input: &str) -> Vec<Token> {
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
