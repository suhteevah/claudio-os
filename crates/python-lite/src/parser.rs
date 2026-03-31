//! Parser for python-lite.
//!
//! Converts a token stream into an AST (Vec<Stmt>). Uses recursive descent
//! with Pratt-style precedence for expressions.

use alloc::string::String;
use alloc::vec::Vec;
use alloc::boxed::Box;

use crate::tokenizer::Token;

// ---------------------------------------------------------------------------
// AST types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum Stmt {
    /// Expression statement (including bare function calls).
    Expr(Expr),
    /// Variable assignment: `name = expr`
    Assign {
        target: AssignTarget,
        value: Expr,
    },
    /// Augmented assignment: `name += expr`, etc.
    AugAssign {
        target: AssignTarget,
        op: BinOp,
        value: Expr,
    },
    /// if / elif / else
    If {
        condition: Expr,
        body: Vec<Stmt>,
        elif_clauses: Vec<(Expr, Vec<Stmt>)>,
        else_body: Option<Vec<Stmt>>,
    },
    /// while loop
    While {
        condition: Expr,
        body: Vec<Stmt>,
    },
    /// for loop: `for var in iterable: body`
    For {
        var: String,
        iterable: Expr,
        body: Vec<Stmt>,
    },
    /// Function definition
    FuncDef {
        name: String,
        params: Vec<String>,
        body: Vec<Stmt>,
    },
    /// return statement
    Return(Option<Expr>),
    /// break
    Break,
    /// continue
    Continue,
}

#[derive(Debug, Clone)]
pub enum AssignTarget {
    Name(String),
    Index { obj: Expr, index: Expr },
}

#[derive(Debug, Clone)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    None,
    Name(String),
    List(Vec<Expr>),
    BinOp {
        left: Box<Expr>,
        op: BinOp,
        right: Box<Expr>,
    },
    UnaryOp {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Compare {
        left: Box<Expr>,
        op: CmpOp,
        right: Box<Expr>,
    },
    BoolOp {
        left: Box<Expr>,
        op: BoolOpKind,
        right: Box<Expr>,
    },
    Call {
        func: Box<Expr>,
        args: Vec<Expr>,
    },
    Index {
        obj: Box<Expr>,
        index: Box<Expr>,
    },
    Attribute {
        obj: Box<Expr>,
        attr: String,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Mod,
    Pow,
}

#[derive(Debug, Clone, Copy)]
pub enum UnaryOp {
    Neg,
    Not,
}

#[derive(Debug, Clone, Copy)]
pub enum CmpOp {
    Eq,
    NotEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
}

#[derive(Debug, Clone, Copy)]
pub enum BoolOpKind {
    And,
    Or,
}

// ---------------------------------------------------------------------------
// Parser state
// ---------------------------------------------------------------------------

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

pub fn parse(tokens: Vec<Token>) -> Result<Vec<Stmt>, String> {
    let mut p = Parser { tokens, pos: 0 };
    p.parse_block_top()
}

impl Parser {
    fn peek(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or(&Token::Eof)
    }

    fn advance(&mut self) -> Token {
        let tok = self.tokens.get(self.pos).cloned().unwrap_or(Token::Eof);
        self.pos += 1;
        tok
    }

    fn expect(&mut self, expected: &Token) -> Result<(), String> {
        let got = self.advance();
        if core::mem::discriminant(&got) == core::mem::discriminant(expected) {
            Ok(())
        } else {
            Err(alloc::format!("expected {:?}, got {:?}", expected, got))
        }
    }

    fn at(&self, tok: &Token) -> bool {
        core::mem::discriminant(self.peek()) == core::mem::discriminant(tok)
    }

    fn skip_newlines(&mut self) {
        while self.at(&Token::Newline) {
            self.advance();
        }
    }

    // -----------------------------------------------------------------------
    // Top-level block (no leading INDENT)
    // -----------------------------------------------------------------------

    fn parse_block_top(&mut self) -> Result<Vec<Stmt>, String> {
        let mut stmts = Vec::new();
        self.skip_newlines();
        while !self.at(&Token::Eof) {
            stmts.push(self.parse_stmt()?);
            self.skip_newlines();
        }
        Ok(stmts)
    }

    // -----------------------------------------------------------------------
    // Indented block (after INDENT, until DEDENT)
    // -----------------------------------------------------------------------

    fn parse_indented_block(&mut self) -> Result<Vec<Stmt>, String> {
        self.skip_newlines();
        self.expect(&Token::Indent)?;
        let mut stmts = Vec::new();
        self.skip_newlines();
        while !self.at(&Token::Dedent) && !self.at(&Token::Eof) {
            stmts.push(self.parse_stmt()?);
            self.skip_newlines();
        }
        if self.at(&Token::Dedent) {
            self.advance();
        }
        Ok(stmts)
    }

    // -----------------------------------------------------------------------
    // Statement
    // -----------------------------------------------------------------------

    fn parse_stmt(&mut self) -> Result<Stmt, String> {
        match self.peek().clone() {
            Token::If => self.parse_if(),
            Token::While => self.parse_while(),
            Token::For => self.parse_for(),
            Token::Def => self.parse_def(),
            Token::Return => self.parse_return(),
            Token::Break => { self.advance(); Ok(Stmt::Break) }
            Token::Continue => { self.advance(); Ok(Stmt::Continue) }
            _ => self.parse_expr_or_assign(),
        }
    }

    fn parse_expr_or_assign(&mut self) -> Result<Stmt, String> {
        let expr = self.parse_expression()?;

        // Check for assignment or augmented assignment.
        match self.peek() {
            Token::Assign => {
                self.advance();
                let value = self.parse_expression()?;
                let target = expr_to_assign_target(expr)?;
                Ok(Stmt::Assign { target, value })
            }
            Token::PlusEq => {
                self.advance();
                let value = self.parse_expression()?;
                let target = expr_to_assign_target(expr)?;
                Ok(Stmt::AugAssign { target, op: BinOp::Add, value })
            }
            Token::MinusEq => {
                self.advance();
                let value = self.parse_expression()?;
                let target = expr_to_assign_target(expr)?;
                Ok(Stmt::AugAssign { target, op: BinOp::Sub, value })
            }
            Token::StarEq => {
                self.advance();
                let value = self.parse_expression()?;
                let target = expr_to_assign_target(expr)?;
                Ok(Stmt::AugAssign { target, op: BinOp::Mul, value })
            }
            Token::SlashEq => {
                self.advance();
                let value = self.parse_expression()?;
                let target = expr_to_assign_target(expr)?;
                Ok(Stmt::AugAssign { target, op: BinOp::Div, value })
            }
            _ => Ok(Stmt::Expr(expr)),
        }
    }

    // -----------------------------------------------------------------------
    // if / elif / else
    // -----------------------------------------------------------------------

    fn parse_if(&mut self) -> Result<Stmt, String> {
        self.expect(&Token::If)?;
        let condition = self.parse_expression()?;
        self.expect(&Token::Colon)?;
        let body = self.parse_indented_block()?;

        let mut elif_clauses = Vec::new();
        let mut else_body = Option::None;

        loop {
            self.skip_newlines();
            if self.at(&Token::Elif) {
                self.advance();
                let cond = self.parse_expression()?;
                self.expect(&Token::Colon)?;
                let block = self.parse_indented_block()?;
                elif_clauses.push((cond, block));
            } else if self.at(&Token::Else) {
                self.advance();
                self.expect(&Token::Colon)?;
                else_body = Some(self.parse_indented_block()?);
                break;
            } else {
                break;
            }
        }

        Ok(Stmt::If {
            condition,
            body,
            elif_clauses,
            else_body,
        })
    }

    // -----------------------------------------------------------------------
    // while
    // -----------------------------------------------------------------------

    fn parse_while(&mut self) -> Result<Stmt, String> {
        self.expect(&Token::While)?;
        let condition = self.parse_expression()?;
        self.expect(&Token::Colon)?;
        let body = self.parse_indented_block()?;
        Ok(Stmt::While { condition, body })
    }

    // -----------------------------------------------------------------------
    // for
    // -----------------------------------------------------------------------

    fn parse_for(&mut self) -> Result<Stmt, String> {
        self.expect(&Token::For)?;
        let var = match self.advance() {
            Token::Ident(name) => name,
            other => return Err(alloc::format!("expected identifier after 'for', got {:?}", other)),
        };
        self.expect(&Token::In)?;
        let iterable = self.parse_expression()?;
        self.expect(&Token::Colon)?;
        let body = self.parse_indented_block()?;
        Ok(Stmt::For { var, iterable, body })
    }

    // -----------------------------------------------------------------------
    // def
    // -----------------------------------------------------------------------

    fn parse_def(&mut self) -> Result<Stmt, String> {
        self.expect(&Token::Def)?;
        let name = match self.advance() {
            Token::Ident(n) => n,
            other => return Err(alloc::format!("expected function name, got {:?}", other)),
        };
        self.expect(&Token::LParen)?;
        let mut params = Vec::new();
        while !self.at(&Token::RParen) && !self.at(&Token::Eof) {
            match self.advance() {
                Token::Ident(p) => params.push(p),
                other => return Err(alloc::format!("expected parameter name, got {:?}", other)),
            }
            if self.at(&Token::Comma) {
                self.advance();
            }
        }
        self.expect(&Token::RParen)?;
        self.expect(&Token::Colon)?;
        let body = self.parse_indented_block()?;
        Ok(Stmt::FuncDef { name, params, body })
    }

    // -----------------------------------------------------------------------
    // return
    // -----------------------------------------------------------------------

    fn parse_return(&mut self) -> Result<Stmt, String> {
        self.expect(&Token::Return)?;
        if self.at(&Token::Newline) || self.at(&Token::Eof) || self.at(&Token::Dedent) {
            Ok(Stmt::Return(Option::None))
        } else {
            let expr = self.parse_expression()?;
            Ok(Stmt::Return(Some(expr)))
        }
    }

    // -----------------------------------------------------------------------
    // Expression parsing (precedence climbing)
    // -----------------------------------------------------------------------

    fn parse_expression(&mut self) -> Result<Expr, String> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_and()?;
        while self.at(&Token::Or) {
            self.advance();
            let right = self.parse_and()?;
            left = Expr::BoolOp {
                left: Box::new(left),
                op: BoolOpKind::Or,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_not()?;
        while self.at(&Token::And) {
            self.advance();
            let right = self.parse_not()?;
            left = Expr::BoolOp {
                left: Box::new(left),
                op: BoolOpKind::And,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<Expr, String> {
        if self.at(&Token::Not) {
            self.advance();
            let operand = self.parse_not()?;
            Ok(Expr::UnaryOp {
                op: UnaryOp::Not,
                operand: Box::new(operand),
            })
        } else {
            self.parse_comparison()
        }
    }

    fn parse_comparison(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_add_sub()?;

        loop {
            let op = match self.peek() {
                Token::Eq => CmpOp::Eq,
                Token::NotEq => CmpOp::NotEq,
                Token::Lt => CmpOp::Lt,
                Token::Gt => CmpOp::Gt,
                Token::LtEq => CmpOp::LtEq,
                Token::GtEq => CmpOp::GtEq,
                _ => break,
            };
            self.advance();
            let right = self.parse_add_sub()?;
            left = Expr::Compare {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }

        Ok(left)
    }

    fn parse_add_sub(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_mul_div()?;

        loop {
            let op = match self.peek() {
                Token::Plus => BinOp::Add,
                Token::Minus => BinOp::Sub,
                _ => break,
            };
            self.advance();
            let right = self.parse_mul_div()?;
            left = Expr::BinOp {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }

        Ok(left)
    }

    fn parse_mul_div(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_power()?;

        loop {
            let op = match self.peek() {
                Token::Star => BinOp::Mul,
                Token::Slash => BinOp::Div,
                Token::DoubleSlash => BinOp::FloorDiv,
                Token::Percent => BinOp::Mod,
                _ => break,
            };
            self.advance();
            let right = self.parse_power()?;
            left = Expr::BinOp {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }

        Ok(left)
    }

    fn parse_power(&mut self) -> Result<Expr, String> {
        let base = self.parse_unary()?;
        if self.at(&Token::DoubleStar) {
            self.advance();
            // Right-associative: recurse into parse_power.
            let exp = self.parse_power()?;
            Ok(Expr::BinOp {
                left: Box::new(base),
                op: BinOp::Pow,
                right: Box::new(exp),
            })
        } else {
            Ok(base)
        }
    }

    fn parse_unary(&mut self) -> Result<Expr, String> {
        if self.at(&Token::Minus) {
            self.advance();
            let operand = self.parse_unary()?;
            Ok(Expr::UnaryOp {
                op: UnaryOp::Neg,
                operand: Box::new(operand),
            })
        } else {
            self.parse_postfix()
        }
    }

    fn parse_postfix(&mut self) -> Result<Expr, String> {
        let mut expr = self.parse_atom()?;

        loop {
            match self.peek() {
                Token::LParen => {
                    self.advance();
                    let args = self.parse_arg_list()?;
                    self.expect(&Token::RParen)?;
                    expr = Expr::Call {
                        func: Box::new(expr),
                        args,
                    };
                }
                Token::LBracket => {
                    self.advance();
                    let index = self.parse_expression()?;
                    self.expect(&Token::RBracket)?;
                    expr = Expr::Index {
                        obj: Box::new(expr),
                        index: Box::new(index),
                    };
                }
                Token::Dot => {
                    self.advance();
                    let attr = match self.advance() {
                        Token::Ident(name) => name,
                        other => return Err(alloc::format!("expected attribute name, got {:?}", other)),
                    };
                    expr = Expr::Attribute {
                        obj: Box::new(expr),
                        attr,
                    };
                }
                _ => break,
            }
        }

        Ok(expr)
    }

    fn parse_atom(&mut self) -> Result<Expr, String> {
        match self.peek().clone() {
            Token::Int(n) => { self.advance(); Ok(Expr::Int(n)) }
            Token::Float(f) => { self.advance(); Ok(Expr::Float(f)) }
            Token::Str(s) => { self.advance(); Ok(Expr::Str(s)) }
            Token::True => { self.advance(); Ok(Expr::Bool(true)) }
            Token::False => { self.advance(); Ok(Expr::Bool(false)) }
            Token::None => { self.advance(); Ok(Expr::None) }
            Token::Ident(name) => { self.advance(); Ok(Expr::Name(name)) }
            Token::LParen => {
                self.advance();
                let expr = self.parse_expression()?;
                self.expect(&Token::RParen)?;
                Ok(expr)
            }
            Token::LBracket => {
                self.advance();
                let mut elements = Vec::new();
                while !self.at(&Token::RBracket) && !self.at(&Token::Eof) {
                    elements.push(self.parse_expression()?);
                    if self.at(&Token::Comma) {
                        self.advance();
                    }
                }
                self.expect(&Token::RBracket)?;
                Ok(Expr::List(elements))
            }
            other => Err(alloc::format!("unexpected token: {:?}", other)),
        }
    }

    fn parse_arg_list(&mut self) -> Result<Vec<Expr>, String> {
        let mut args = Vec::new();
        while !self.at(&Token::RParen) && !self.at(&Token::Eof) {
            args.push(self.parse_expression()?);
            if self.at(&Token::Comma) {
                self.advance();
            }
        }
        Ok(args)
    }
}

fn expr_to_assign_target(expr: Expr) -> Result<AssignTarget, String> {
    match expr {
        Expr::Name(name) => Ok(AssignTarget::Name(name)),
        Expr::Index { obj, index } => Ok(AssignTarget::Index { obj: *obj, index: *index }),
        _ => Err(String::from("invalid assignment target")),
    }
}
