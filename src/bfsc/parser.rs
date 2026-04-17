use super::ast::*;
use super::lexer::{SpannedToken, Token};
use super::BfscError;

struct Parser<'a> {
    tokens: &'a [SpannedToken],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(tokens: &'a [SpannedToken]) -> Self {
        Parser { tokens, pos: 0 }
    }

    fn cur(&self) -> &Token {
        &self.tokens[self.pos].token
    }

    fn cur_pos(&self) -> usize {
        self.tokens[self.pos].pos
    }

    fn peek(&self, tok: &Token) -> bool {
        self.cur() == tok
    }

    fn bump(&mut self) -> &Token {
        let t = &self.tokens[self.pos].token;
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        t
    }

    fn expect(&mut self, tok: &Token) -> Result<(), BfscError> {
        if self.cur() == tok {
            self.bump();
            Ok(())
        } else {
            Err(BfscError::Parse(format!(
                "expected {tok:?}, got {:?} at byte {}",
                self.cur(),
                self.cur_pos()
            )))
        }
    }

    fn match_tok(&mut self, tok: &Token) -> bool {
        if self.cur() == tok {
            self.bump();
            true
        } else {
            false
        }
    }

    fn parse_type(&mut self) -> Result<TypeAnn, BfscError> {
        let st = match self.cur() {
            Token::U8 => { self.bump(); ScalarType::U8 }
            Token::I8 => { self.bump(); ScalarType::I8 }
            Token::U16 => { self.bump(); ScalarType::U16 }
            Token::I16 => { self.bump(); ScalarType::I16 }
            Token::U32 => { self.bump(); ScalarType::U32 }
            Token::I32 => { self.bump(); ScalarType::I32 }
            Token::LBrack => {
                self.bump();
                let elem = self.parse_scalar_type()?;
                self.expect(&Token::Semi)?;
                let n = match self.cur() {
                    Token::Int(n) => { let n = *n; self.bump(); n }
                    _ => return Err(BfscError::Parse(format!(
                        "expected array size at byte {}", self.cur_pos()
                    ))),
                };
                self.expect(&Token::RBrack)?;
                return Ok(TypeAnn::Array(elem, n as u32));
            }
            _ => return Err(BfscError::Parse(format!(
                "expected type, got {:?} at byte {}", self.cur(), self.cur_pos()
            ))),
        };
        Ok(TypeAnn::Scalar(st))
    }

    fn parse_scalar_type(&mut self) -> Result<ScalarType, BfscError> {
        match self.cur() {
            Token::U8 => { self.bump(); Ok(ScalarType::U8) }
            Token::I8 => { self.bump(); Ok(ScalarType::I8) }
            Token::U16 => { self.bump(); Ok(ScalarType::U16) }
            Token::I16 => { self.bump(); Ok(ScalarType::I16) }
            Token::U32 => { self.bump(); Ok(ScalarType::U32) }
            Token::I32 => { self.bump(); Ok(ScalarType::I32) }
            _ => Err(BfscError::Parse(format!(
                "expected scalar type, got {:?} at byte {}", self.cur(), self.cur_pos()
            ))),
        }
    }

    fn parse_stmts(&mut self) -> Result<Vec<Stmt>, BfscError> {
        let mut stmts = Vec::new();
        loop {
            match self.cur() {
                Token::RBrace | Token::Eof => break,
                _ => stmts.push(self.parse_stmt()?),
            }
        }
        Ok(stmts)
    }

    fn parse_block(&mut self) -> Result<Vec<Stmt>, BfscError> {
        self.expect(&Token::LBrace)?;
        let stmts = self.parse_stmts()?;
        self.expect(&Token::RBrace)?;
        Ok(stmts)
    }

    fn parse_stmt(&mut self) -> Result<Stmt, BfscError> {
        match self.cur() {
            Token::Let => self.parse_let(),
            Token::While => self.parse_while(),
            Token::If => self.parse_if(),
            Token::Scan => self.parse_scan(),
            Token::Print => self.parse_print(),
            Token::Putchar => self.parse_putchar(),
            Token::Ident(_) => self.parse_assign(),
            tok => Err(BfscError::Parse(format!(
                "unexpected token {tok:?} at byte {}", self.cur_pos()
            ))),
        }
    }

    fn parse_let(&mut self) -> Result<Stmt, BfscError> {
        self.bump(); // let
        let name = match self.bump() {
            Token::Ident(s) => s.clone(),
            t => return Err(BfscError::Parse(format!("expected identifier, got {t:?}"))),
        };
        self.expect(&Token::Colon)?;
        let ty = self.parse_type()?;
        let init = if self.match_tok(&Token::Eq) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.expect(&Token::Semi)?;
        Ok(Stmt::Let { name, ty, init })
    }

    fn parse_while(&mut self) -> Result<Stmt, BfscError> {
        self.bump(); // while
        let cond = self.parse_expr()?;
        let body = self.parse_block()?;
        Ok(Stmt::While { cond, body })
    }

    fn parse_if(&mut self) -> Result<Stmt, BfscError> {
        self.bump(); // if
        let cond = self.parse_expr()?;
        let then_ = self.parse_block()?;
        let else_ = if self.match_tok(&Token::Else) {
            Some(self.parse_block()?)
        } else {
            None
        };
        Ok(Stmt::If { cond, then_, else_ })
    }

    fn parse_scan(&mut self) -> Result<Stmt, BfscError> {
        self.bump(); // scan
        self.expect(&Token::LParen)?;
        let lval = self.parse_lvalue()?;
        self.expect(&Token::RParen)?;
        self.expect(&Token::Semi)?;
        Ok(Stmt::Scan(lval))
    }

    fn parse_print(&mut self) -> Result<Stmt, BfscError> {
        self.bump(); // print
        self.expect(&Token::LParen)?;
        let expr = self.parse_expr()?;
        self.expect(&Token::RParen)?;
        self.expect(&Token::Semi)?;
        Ok(Stmt::Print(expr))
    }

    fn parse_putchar(&mut self) -> Result<Stmt, BfscError> {
        self.bump(); // putchar
        self.expect(&Token::LParen)?;
        let expr = self.parse_expr()?;
        self.expect(&Token::RParen)?;
        self.expect(&Token::Semi)?;
        Ok(Stmt::Putchar(expr))
    }

    fn parse_assign(&mut self) -> Result<Stmt, BfscError> {
        let lval = self.parse_lvalue()?;
        self.expect(&Token::Eq)?;
        let expr = self.parse_expr()?;
        self.expect(&Token::Semi)?;
        Ok(Stmt::Assign { lval, expr })
    }

    fn parse_lvalue(&mut self) -> Result<LValue, BfscError> {
        let name = match self.bump() {
            Token::Ident(s) => s.clone(),
            t => return Err(BfscError::Parse(format!("expected identifier, got {t:?}"))),
        };
        if self.match_tok(&Token::LBrack) {
            let idx = self.parse_expr()?;
            self.expect(&Token::RBrack)?;
            Ok(LValue::Index(name, Box::new(idx)))
        } else {
            Ok(LValue::Var(name))
        }
    }

    // Precedence: or < and < cmp < add < mul < unary < primary
    fn parse_expr(&mut self) -> Result<Expr, BfscError> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Expr, BfscError> {
        let mut left = self.parse_and()?;
        while self.peek(&Token::PipePipe) {
            self.bump();
            let right = self.parse_and()?;
            left = Expr::BinOp(BinOp::Or, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, BfscError> {
        let mut left = self.parse_cmp()?;
        while self.peek(&Token::AmpAmp) {
            self.bump();
            let right = self.parse_cmp()?;
            left = Expr::BinOp(BinOp::And, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_cmp(&mut self) -> Result<Expr, BfscError> {
        let left = self.parse_add()?;
        let op = match self.cur() {
            Token::Lt => BinOp::Lt,
            Token::Gt => BinOp::Gt,
            Token::Le => BinOp::Le,
            Token::Ge => BinOp::Ge,
            Token::EqEq => BinOp::EqEq,
            Token::BangEq => BinOp::Ne,
            _ => return Ok(left),
        };
        self.bump();
        let right = self.parse_add()?;
        Ok(Expr::BinOp(op, Box::new(left), Box::new(right)))
    }

    fn parse_add(&mut self) -> Result<Expr, BfscError> {
        let mut left = self.parse_mul()?;
        loop {
            let op = match self.cur() {
                Token::Plus => BinOp::Add,
                Token::Minus => BinOp::Sub,
                _ => break,
            };
            self.bump();
            let right = self.parse_mul()?;
            left = Expr::BinOp(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_mul(&mut self) -> Result<Expr, BfscError> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.cur() {
                Token::Star => BinOp::Mul,
                Token::Slash => BinOp::Div,
                Token::Percent => BinOp::Rem,
                _ => break,
            };
            self.bump();
            let right = self.parse_unary()?;
            left = Expr::BinOp(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, BfscError> {
        match self.cur() {
            Token::Minus => {
                self.bump();
                let e = self.parse_unary()?;
                Ok(Expr::UnOp(UnOp::Neg, Box::new(e)))
            }
            Token::Bang => {
                self.bump();
                let e = self.parse_unary()?;
                Ok(Expr::UnOp(UnOp::Not, Box::new(e)))
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> Result<Expr, BfscError> {
        match self.cur().clone() {
            Token::Int(n) => {
                self.bump();
                Ok(Expr::Int(n))
            }
            Token::Ident(name) => {
                self.bump();
                if self.match_tok(&Token::LBrack) {
                    let idx = self.parse_expr()?;
                    self.expect(&Token::RBrack)?;
                    Ok(Expr::Index(name, Box::new(idx)))
                } else {
                    Ok(Expr::Var(name))
                }
            }
            Token::LParen => {
                self.bump();
                let e = self.parse_expr()?;
                self.expect(&Token::RParen)?;
                Ok(e)
            }
            t => Err(BfscError::Parse(format!(
                "expected expression, got {t:?} at byte {}", self.cur_pos()
            ))),
        }
    }
}

pub(crate) fn parse(tokens: &[SpannedToken]) -> Result<Vec<Stmt>, BfscError> {
    let mut p = Parser::new(tokens);
    let stmts = p.parse_stmts()?;
    if !p.peek(&Token::Eof) {
        return Err(BfscError::Parse(format!(
            "unexpected token {:?} at byte {}",
            p.cur(),
            p.cur_pos()
        )));
    }
    Ok(stmts)
}
