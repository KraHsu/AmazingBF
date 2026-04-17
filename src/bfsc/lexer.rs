use super::BfscError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Token {
    // Literals
    Int(u64),
    Ident(String),
    // Keywords
    Let,
    While,
    If,
    Else,
    Scan,
    Print,
    Putchar,
    // Types
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    // Operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Lt,
    Gt,
    Le,
    Ge,
    EqEq,
    BangEq,
    Eq,
    Bang,
    AmpAmp,
    PipePipe,
    // Delimiters
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBrack,
    RBrack,
    Colon,
    Semi,
    Comma,
    // End of input
    Eof,
}

#[derive(Debug, Clone)]
pub(crate) struct SpannedToken {
    pub(crate) token: Token,
    pub(crate) pos: usize,
}

fn keyword(s: &str) -> Option<Token> {
    match s {
        "let" => Some(Token::Let),
        "while" => Some(Token::While),
        "if" => Some(Token::If),
        "else" => Some(Token::Else),
        "scan" => Some(Token::Scan),
        "print" => Some(Token::Print),
        "putchar" => Some(Token::Putchar),
        "u8" => Some(Token::U8),
        "i8" => Some(Token::I8),
        "u16" => Some(Token::U16),
        "i16" => Some(Token::I16),
        "u32" => Some(Token::U32),
        "i32" => Some(Token::I32),
        _ => None,
    }
}

pub(crate) fn tokenize(src: &str) -> Result<Vec<SpannedToken>, BfscError> {
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut pos = 0;
    let mut tokens = Vec::new();

    macro_rules! push {
        ($tok:expr) => {
            tokens.push(SpannedToken { token: $tok, pos })
        };
    }

    while pos < len {
        let start = pos;
        let b = bytes[pos];

        // Whitespace
        if b.is_ascii_whitespace() {
            pos += 1;
            continue;
        }

        // Line comment
        if b == b'/' && pos + 1 < len && bytes[pos + 1] == b'/' {
            while pos < len && bytes[pos] != b'\n' {
                pos += 1;
            }
            continue;
        }

        // Block comment
        if b == b'/' && pos + 1 < len && bytes[pos + 1] == b'*' {
            pos += 2;
            loop {
                if pos + 1 >= len {
                    return Err(BfscError::Lex(format!(
                        "unterminated block comment at byte {start}"
                    )));
                }
                if bytes[pos] == b'*' && bytes[pos + 1] == b'/' {
                    pos += 2;
                    break;
                }
                pos += 1;
            }
            continue;
        }

        // Integer literal
        if b.is_ascii_digit() {
            let mut n: u64 = 0;
            while pos < len && bytes[pos].is_ascii_digit() {
                n = n.wrapping_mul(10).wrapping_add((bytes[pos] - b'0') as u64);
                pos += 1;
            }
            tokens.push(SpannedToken { token: Token::Int(n), pos: start });
            continue;
        }

        // Identifier / keyword
        if b.is_ascii_alphabetic() || b == b'_' {
            while pos < len && (bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_') {
                pos += 1;
            }
            let word = &src[start..pos];
            let tok = keyword(word)
                .unwrap_or_else(|| Token::Ident(word.to_string()));
            tokens.push(SpannedToken { token: tok, pos: start });
            continue;
        }

        // Two-char operators
        if pos + 1 < len {
            let pair = (b, bytes[pos + 1]);
            let maybe = match pair {
                (b'<', b'=') => Some(Token::Le),
                (b'>', b'=') => Some(Token::Ge),
                (b'=', b'=') => Some(Token::EqEq),
                (b'!', b'=') => Some(Token::BangEq),
                (b'&', b'&') => Some(Token::AmpAmp),
                (b'|', b'|') => Some(Token::PipePipe),
                _ => None,
            };
            if let Some(tok) = maybe {
                push!(tok);
                pos += 2;
                continue;
            }
        }

        // Single-char tokens
        let tok = match b {
            b'+' => Token::Plus,
            b'-' => Token::Minus,
            b'*' => Token::Star,
            b'/' => Token::Slash,
            b'%' => Token::Percent,
            b'<' => Token::Lt,
            b'>' => Token::Gt,
            b'=' => Token::Eq,
            b'!' => Token::Bang,
            b'(' => Token::LParen,
            b')' => Token::RParen,
            b'{' => Token::LBrace,
            b'}' => Token::RBrace,
            b'[' => Token::LBrack,
            b']' => Token::RBrack,
            b':' => Token::Colon,
            b';' => Token::Semi,
            b',' => Token::Comma,
            other => {
                return Err(BfscError::Lex(format!(
                    "unexpected character {:?} at byte {start}",
                    other as char
                )))
            }
        };
        push!(tok);
        pos += 1;
    }

    tokens.push(SpannedToken { token: Token::Eof, pos: len });
    Ok(tokens)
}
