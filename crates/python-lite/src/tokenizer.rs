//! Tokenizer for python-lite.
//!
//! Converts source text into a flat stream of tokens, with explicit INDENT /
//! DEDENT tokens derived from leading whitespace (Python-style significant
//! indentation).

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Literals
    Int(i64),
    Float(f64),
    Str(String),
    True,
    False,
    None,

    // Identifier
    Ident(String),

    // Keywords
    If,
    Elif,
    Else,
    For,
    While,
    In,
    Def,
    Return,
    And,
    Or,
    Not,
    Break,
    Continue,

    // Operators
    Plus,
    Minus,
    Star,
    Slash,
    DoubleSlash,
    Percent,
    DoubleStar,
    Eq,        // ==
    NotEq,     // !=
    Lt,
    Gt,
    LtEq,
    GtEq,
    Assign,    // =
    PlusEq,    // +=
    MinusEq,   // -=
    StarEq,    // *=
    SlashEq,   // /=

    // Delimiters
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Colon,
    Dot,

    // Structure
    Newline,
    Indent,
    Dedent,
    Eof,
}

pub fn tokenize(source: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut indent_stack: Vec<usize> = Vec::new();
    indent_stack.push(0);

    let lines = split_logical_lines(source);

    for line in &lines {
        // Skip blank lines and comment-only lines.
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Compute indentation level (number of leading spaces).
        let indent = line.len() - line.trim_start().len();
        let current = *indent_stack.last().unwrap();

        if indent > current {
            indent_stack.push(indent);
            tokens.push(Token::Indent);
        } else {
            while indent < *indent_stack.last().unwrap() {
                indent_stack.pop();
                tokens.push(Token::Dedent);
            }
            if indent != *indent_stack.last().unwrap() {
                return Err(String::from("inconsistent indentation"));
            }
        }

        // Tokenize the content of this line.
        tokenize_line(trimmed, &mut tokens)?;
        tokens.push(Token::Newline);
    }

    // Emit remaining DEDENTs.
    while indent_stack.len() > 1 {
        indent_stack.pop();
        tokens.push(Token::Dedent);
    }

    tokens.push(Token::Eof);
    Ok(tokens)
}

fn split_logical_lines(source: &str) -> Vec<String> {
    // For now, each physical line is a logical line.
    // TODO: handle backslash continuation, open parens.
    source.lines().map(|l| String::from(l)).collect()
}

fn tokenize_line(line: &str, tokens: &mut Vec<Token>) -> Result<(), String> {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];

        // Skip whitespace (already handled indentation).
        if c == ' ' || c == '\t' {
            i += 1;
            continue;
        }

        // Comment — skip rest of line.
        if c == '#' {
            break;
        }

        // String literals.
        if c == '"' || c == '\'' {
            let (s, end) = read_string(&chars, i)?;
            tokens.push(Token::Str(s));
            i = end;
            continue;
        }

        // Numbers.
        if c.is_ascii_digit() || (c == '.' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit()) {
            let (tok, end) = read_number(&chars, i)?;
            tokens.push(tok);
            i = end;
            continue;
        }

        // Identifiers and keywords.
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let tok = match word.as_str() {
                "if" => Token::If,
                "elif" => Token::Elif,
                "else" => Token::Else,
                "for" => Token::For,
                "while" => Token::While,
                "in" => Token::In,
                "def" => Token::Def,
                "return" => Token::Return,
                "and" => Token::And,
                "or" => Token::Or,
                "not" => Token::Not,
                "True" => Token::True,
                "False" => Token::False,
                "None" => Token::None,
                "break" => Token::Break,
                "continue" => Token::Continue,
                _ => Token::Ident(word),
            };
            tokens.push(tok);
            continue;
        }

        // Multi-character operators.
        let next = if i + 1 < chars.len() { Some(chars[i + 1]) } else { Option::None };

        match (c, next) {
            ('*', Some('*')) => { tokens.push(Token::DoubleStar); i += 2; }
            ('*', Some('=')) => { tokens.push(Token::StarEq); i += 2; }
            ('/', Some('/')) => { tokens.push(Token::DoubleSlash); i += 2; }
            ('/', Some('=')) => { tokens.push(Token::SlashEq); i += 2; }
            ('+', Some('=')) => { tokens.push(Token::PlusEq); i += 2; }
            ('-', Some('=')) => { tokens.push(Token::MinusEq); i += 2; }
            ('=', Some('=')) => { tokens.push(Token::Eq); i += 2; }
            ('!', Some('=')) => { tokens.push(Token::NotEq); i += 2; }
            ('<', Some('=')) => { tokens.push(Token::LtEq); i += 2; }
            ('>', Some('=')) => { tokens.push(Token::GtEq); i += 2; }
            _ => {
                // Single-character tokens.
                let tok = match c {
                    '+' => Token::Plus,
                    '-' => Token::Minus,
                    '*' => Token::Star,
                    '/' => Token::Slash,
                    '%' => Token::Percent,
                    '=' => Token::Assign,
                    '<' => Token::Lt,
                    '>' => Token::Gt,
                    '(' => Token::LParen,
                    ')' => Token::RParen,
                    '[' => Token::LBracket,
                    ']' => Token::RBracket,
                    ',' => Token::Comma,
                    ':' => Token::Colon,
                    '.' => Token::Dot,
                    _ => return Err(alloc::format!("unexpected character: '{}'", c)),
                };
                tokens.push(tok);
                i += 1;
            }
        }
    }

    Ok(())
}

fn read_string(chars: &[char], start: usize) -> Result<(String, usize), String> {
    let quote = chars[start];
    let mut s = String::new();
    let mut i = start + 1;

    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            i += 1;
            let esc = match chars[i] {
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                '\\' => '\\',
                '\'' => '\'',
                '"' => '"',
                '0' => '\0',
                other => other,
            };
            s.push(esc);
            i += 1;
        } else if c == quote {
            i += 1;
            return Ok((s, i));
        } else {
            s.push(c);
            i += 1;
        }
    }

    Err(String::from("unterminated string literal"))
}

fn read_number(chars: &[char], start: usize) -> Result<(Token, usize), String> {
    let mut i = start;
    let mut has_dot = false;

    while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
        if chars[i] == '.' {
            if has_dot {
                break;
            }
            // Check it's not a method call on an int (e.g. we won't hit this in practice)
            has_dot = true;
        }
        i += 1;
    }

    let num_str: String = chars[start..i].iter().collect();

    if has_dot {
        let val: f64 = num_str
            .parse()
            .map_err(|_| alloc::format!("invalid float: {}", num_str))?;
        Ok((Token::Float(val), i))
    } else {
        let val: i64 = num_str
            .parse()
            .map_err(|_| alloc::format!("invalid integer: {}", num_str))?;
        Ok((Token::Int(val), i))
    }
}
