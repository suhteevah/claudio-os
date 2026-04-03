//! C tokenizer: keywords, operators, literals, preprocessor directives.

use alloc::string::String;
use alloc::vec::Vec;

/// Source location for error reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub line: u32,
    pub col: u32,
}

/// A C token with its source location.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // === Keywords ===
    Auto, Break, Case, Char, Const, Continue, Default, Do, Double, Else,
    Enum, Extern, Float, For, Goto, If, Inline, Int, Long, Register,
    Return, Short, Signed, Sizeof, Static, Struct, Switch, Typedef,
    Union, Unsigned, Void, Volatile, While, Bool,

    // === Identifiers & Literals ===
    Ident(String),
    IntLit(i64),
    FloatLit(f64),
    CharLit(u8),
    StringLit(Vec<u8>),

    // === Operators ===
    Plus,         // +
    Minus,        // -
    Star,         // *
    Slash,        // /
    Percent,      // %
    Amp,          // &
    Pipe,         // |
    Caret,        // ^
    Tilde,        // ~
    Bang,         // !
    Assign,       // =
    Lt,           // <
    Gt,           // >
    Question,     // ?
    Dot,          // .
    Arrow,        // ->
    PlusPlus,     // ++
    MinusMinus,   // --
    Shl,          // <<
    Shr,          // >>
    Le,           // <=
    Ge,           // >=
    EqEq,         // ==
    Ne,           // !=
    AmpAmp,       // &&
    PipePipe,     // ||
    PlusAssign,   // +=
    MinusAssign,  // -=
    StarAssign,   // *=
    SlashAssign,  // /=
    PercentAssign,// %=
    AmpAssign,    // &=
    PipeAssign,   // |=
    CaretAssign,  // ^=
    ShlAssign,    // <<=
    ShrAssign,    // >>=

    // === Delimiters ===
    LParen,       // (
    RParen,       // )
    LBrace,       // {
    RBrace,       // }
    LBracket,     // [
    RBracket,     // ]
    Semicolon,    // ;
    Comma,        // ,
    Colon,        // :
    Ellipsis,     // ...
    Hash,         // #

    // === Preprocessor (simplified) ===
    PpInclude(String),
    PpDefine(String, String),
    PpIfdef(String),
    PpIfndef(String),
    PpEndif,
    PpIf,
    PpElif,
    PpElse,
    PpPragma(String),
    PpError(String),

    // === Special ===
    MacroFile,    // __FILE__
    MacroLine,    // __LINE__
    MacroFunc,    // __func__

    Eof,
}

/// Tokenize C source code.
pub fn tokenize(source: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let bytes = source.as_bytes();
    let mut i = 0;
    let mut line: u32 = 1;
    let mut col: u32 = 1;

    while i < bytes.len() {
        let start_line = line;
        let start_col = col;

        match bytes[i] {
            // Whitespace
            b' ' | b'\t' => {
                col += 1;
                i += 1;
            }
            b'\r' => {
                i += 1;
                if i < bytes.len() && bytes[i] == b'\n' {
                    i += 1;
                }
                line += 1;
                col = 1;
            }
            b'\n' => {
                i += 1;
                line += 1;
                col = 1;
            }
            // Line comment
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            // Block comment
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                col += 2;
                while i + 1 < bytes.len() {
                    if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                        i += 2;
                        col += 2;
                        break;
                    }
                    if bytes[i] == b'\n' {
                        line += 1;
                        col = 1;
                    } else {
                        col += 1;
                    }
                    i += 1;
                }
            }
            // Preprocessor directive
            b'#' => {
                let span = Span { line: start_line, col: start_col };
                i += 1;
                col += 1;
                // Skip whitespace
                while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
                    i += 1;
                    col += 1;
                }
                // Read directive name
                let dir_start = i;
                while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                    i += 1;
                    col += 1;
                }
                let directive = core::str::from_utf8(&bytes[dir_start..i]).unwrap_or("");
                // Skip whitespace
                while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
                    i += 1;
                    col += 1;
                }
                // Read rest of line
                let rest_start = i;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                let rest = core::str::from_utf8(&bytes[rest_start..i]).unwrap_or("").trim_end();
                let rest_str = String::from(rest);

                match directive {
                    "include" => tokens.push(Token { kind: TokenKind::PpInclude(rest_str), span }),
                    "define" => {
                        let (name, val) = split_define(rest);
                        tokens.push(Token { kind: TokenKind::PpDefine(name, val), span });
                    }
                    "ifdef" => tokens.push(Token { kind: TokenKind::PpIfdef(rest_str), span }),
                    "ifndef" => tokens.push(Token { kind: TokenKind::PpIfndef(rest_str), span }),
                    "endif" => tokens.push(Token { kind: TokenKind::PpEndif, span }),
                    "if" => tokens.push(Token { kind: TokenKind::PpIf, span }),
                    "elif" => tokens.push(Token { kind: TokenKind::PpElif, span }),
                    "else" => tokens.push(Token { kind: TokenKind::PpElse, span }),
                    "pragma" => tokens.push(Token { kind: TokenKind::PpPragma(rest_str), span }),
                    "error" => tokens.push(Token { kind: TokenKind::PpError(rest_str), span }),
                    _ => {} // Ignore unknown preprocessor directives
                }
            }
            // String literal
            b'"' => {
                let span = Span { line: start_line, col: start_col };
                i += 1;
                col += 1;
                let mut bytes_vec = Vec::new();
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        i += 1;
                        col += 1;
                        match bytes[i] {
                            b'n' => bytes_vec.push(b'\n'),
                            b'r' => bytes_vec.push(b'\r'),
                            b't' => bytes_vec.push(b'\t'),
                            b'0' => bytes_vec.push(0),
                            b'\\' => bytes_vec.push(b'\\'),
                            b'"' => bytes_vec.push(b'"'),
                            b'\'' => bytes_vec.push(b'\''),
                            b'a' => bytes_vec.push(0x07),
                            b'b' => bytes_vec.push(0x08),
                            b'f' => bytes_vec.push(0x0C),
                            b'v' => bytes_vec.push(0x0B),
                            b'x' => {
                                // Hex escape
                                i += 1;
                                let mut val = 0u8;
                                for _ in 0..2 {
                                    if i < bytes.len() && bytes[i].is_ascii_hexdigit() {
                                        val = val * 16 + hex_digit(bytes[i]);
                                        i += 1;
                                        col += 1;
                                    }
                                }
                                bytes_vec.push(val);
                                continue;
                            }
                            other => bytes_vec.push(other),
                        }
                    } else {
                        bytes_vec.push(bytes[i]);
                    }
                    i += 1;
                    col += 1;
                }
                if i < bytes.len() {
                    i += 1;
                    col += 1;
                }
                tokens.push(Token { kind: TokenKind::StringLit(bytes_vec), span });
            }
            // Char literal
            b'\'' => {
                let span = Span { line: start_line, col: start_col };
                i += 1;
                col += 1;
                let ch = if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 1;
                    col += 1;
                    match bytes[i] {
                        b'n' => b'\n',
                        b'r' => b'\r',
                        b't' => b'\t',
                        b'0' => 0,
                        b'\\' => b'\\',
                        b'\'' => b'\'',
                        other => other,
                    }
                } else {
                    bytes[i]
                };
                i += 1;
                col += 1;
                if i < bytes.len() && bytes[i] == b'\'' {
                    i += 1;
                    col += 1;
                }
                tokens.push(Token { kind: TokenKind::CharLit(ch), span });
            }
            // Number
            b'0'..=b'9' => {
                let span = Span { line: start_line, col: start_col };
                let (tok, adv) = lex_number(&bytes[i..]);
                tokens.push(Token { kind: tok, span });
                col += adv as u32;
                i += adv;
            }
            // Identifier or keyword
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                let span = Span { line: start_line, col: start_col };
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                    col += 1;
                }
                let word = core::str::from_utf8(&bytes[start..i]).unwrap();
                let kind = match word {
                    "auto" => TokenKind::Auto,
                    "break" => TokenKind::Break,
                    "case" => TokenKind::Case,
                    "char" => TokenKind::Char,
                    "const" => TokenKind::Const,
                    "continue" => TokenKind::Continue,
                    "default" => TokenKind::Default,
                    "do" => TokenKind::Do,
                    "double" => TokenKind::Double,
                    "else" => TokenKind::Else,
                    "enum" => TokenKind::Enum,
                    "extern" => TokenKind::Extern,
                    "float" => TokenKind::Float,
                    "for" => TokenKind::For,
                    "goto" => TokenKind::Goto,
                    "if" => TokenKind::If,
                    "inline" => TokenKind::Inline,
                    "int" => TokenKind::Int,
                    "long" => TokenKind::Long,
                    "register" => TokenKind::Register,
                    "return" => TokenKind::Return,
                    "short" => TokenKind::Short,
                    "signed" => TokenKind::Signed,
                    "sizeof" => TokenKind::Sizeof,
                    "static" => TokenKind::Static,
                    "struct" => TokenKind::Struct,
                    "switch" => TokenKind::Switch,
                    "typedef" => TokenKind::Typedef,
                    "union" => TokenKind::Union,
                    "unsigned" => TokenKind::Unsigned,
                    "void" => TokenKind::Void,
                    "volatile" => TokenKind::Volatile,
                    "while" => TokenKind::While,
                    "_Bool" => TokenKind::Bool,
                    "__FILE__" => TokenKind::MacroFile,
                    "__LINE__" => TokenKind::MacroLine,
                    "__func__" => TokenKind::MacroFunc,
                    _ => TokenKind::Ident(String::from(word)),
                };
                tokens.push(Token { kind, span });
            }
            // Operators and delimiters
            b'+' => {
                let span = Span { line: start_line, col: start_col };
                i += 1; col += 1;
                if i < bytes.len() && bytes[i] == b'+' {
                    tokens.push(Token { kind: TokenKind::PlusPlus, span });
                    i += 1; col += 1;
                } else if i < bytes.len() && bytes[i] == b'=' {
                    tokens.push(Token { kind: TokenKind::PlusAssign, span });
                    i += 1; col += 1;
                } else {
                    tokens.push(Token { kind: TokenKind::Plus, span });
                }
            }
            b'-' => {
                let span = Span { line: start_line, col: start_col };
                i += 1; col += 1;
                if i < bytes.len() && bytes[i] == b'-' {
                    tokens.push(Token { kind: TokenKind::MinusMinus, span });
                    i += 1; col += 1;
                } else if i < bytes.len() && bytes[i] == b'=' {
                    tokens.push(Token { kind: TokenKind::MinusAssign, span });
                    i += 1; col += 1;
                } else if i < bytes.len() && bytes[i] == b'>' {
                    tokens.push(Token { kind: TokenKind::Arrow, span });
                    i += 1; col += 1;
                } else {
                    tokens.push(Token { kind: TokenKind::Minus, span });
                }
            }
            b'*' => {
                let span = Span { line: start_line, col: start_col };
                i += 1; col += 1;
                if i < bytes.len() && bytes[i] == b'=' {
                    tokens.push(Token { kind: TokenKind::StarAssign, span });
                    i += 1; col += 1;
                } else {
                    tokens.push(Token { kind: TokenKind::Star, span });
                }
            }
            b'/' => {
                let span = Span { line: start_line, col: start_col };
                i += 1; col += 1;
                if i < bytes.len() && bytes[i] == b'=' {
                    tokens.push(Token { kind: TokenKind::SlashAssign, span });
                    i += 1; col += 1;
                } else {
                    tokens.push(Token { kind: TokenKind::Slash, span });
                }
            }
            b'%' => {
                let span = Span { line: start_line, col: start_col };
                i += 1; col += 1;
                if i < bytes.len() && bytes[i] == b'=' {
                    tokens.push(Token { kind: TokenKind::PercentAssign, span });
                    i += 1; col += 1;
                } else {
                    tokens.push(Token { kind: TokenKind::Percent, span });
                }
            }
            b'&' => {
                let span = Span { line: start_line, col: start_col };
                i += 1; col += 1;
                if i < bytes.len() && bytes[i] == b'&' {
                    tokens.push(Token { kind: TokenKind::AmpAmp, span });
                    i += 1; col += 1;
                } else if i < bytes.len() && bytes[i] == b'=' {
                    tokens.push(Token { kind: TokenKind::AmpAssign, span });
                    i += 1; col += 1;
                } else {
                    tokens.push(Token { kind: TokenKind::Amp, span });
                }
            }
            b'|' => {
                let span = Span { line: start_line, col: start_col };
                i += 1; col += 1;
                if i < bytes.len() && bytes[i] == b'|' {
                    tokens.push(Token { kind: TokenKind::PipePipe, span });
                    i += 1; col += 1;
                } else if i < bytes.len() && bytes[i] == b'=' {
                    tokens.push(Token { kind: TokenKind::PipeAssign, span });
                    i += 1; col += 1;
                } else {
                    tokens.push(Token { kind: TokenKind::Pipe, span });
                }
            }
            b'^' => {
                let span = Span { line: start_line, col: start_col };
                i += 1; col += 1;
                if i < bytes.len() && bytes[i] == b'=' {
                    tokens.push(Token { kind: TokenKind::CaretAssign, span });
                    i += 1; col += 1;
                } else {
                    tokens.push(Token { kind: TokenKind::Caret, span });
                }
            }
            b'~' => {
                tokens.push(Token { kind: TokenKind::Tilde, span: Span { line: start_line, col: start_col } });
                i += 1; col += 1;
            }
            b'!' => {
                let span = Span { line: start_line, col: start_col };
                i += 1; col += 1;
                if i < bytes.len() && bytes[i] == b'=' {
                    tokens.push(Token { kind: TokenKind::Ne, span });
                    i += 1; col += 1;
                } else {
                    tokens.push(Token { kind: TokenKind::Bang, span });
                }
            }
            b'=' => {
                let span = Span { line: start_line, col: start_col };
                i += 1; col += 1;
                if i < bytes.len() && bytes[i] == b'=' {
                    tokens.push(Token { kind: TokenKind::EqEq, span });
                    i += 1; col += 1;
                } else {
                    tokens.push(Token { kind: TokenKind::Assign, span });
                }
            }
            b'<' => {
                let span = Span { line: start_line, col: start_col };
                i += 1; col += 1;
                if i < bytes.len() && bytes[i] == b'<' {
                    i += 1; col += 1;
                    if i < bytes.len() && bytes[i] == b'=' {
                        tokens.push(Token { kind: TokenKind::ShlAssign, span });
                        i += 1; col += 1;
                    } else {
                        tokens.push(Token { kind: TokenKind::Shl, span });
                    }
                } else if i < bytes.len() && bytes[i] == b'=' {
                    tokens.push(Token { kind: TokenKind::Le, span });
                    i += 1; col += 1;
                } else {
                    tokens.push(Token { kind: TokenKind::Lt, span });
                }
            }
            b'>' => {
                let span = Span { line: start_line, col: start_col };
                i += 1; col += 1;
                if i < bytes.len() && bytes[i] == b'>' {
                    i += 1; col += 1;
                    if i < bytes.len() && bytes[i] == b'=' {
                        tokens.push(Token { kind: TokenKind::ShrAssign, span });
                        i += 1; col += 1;
                    } else {
                        tokens.push(Token { kind: TokenKind::Shr, span });
                    }
                } else if i < bytes.len() && bytes[i] == b'=' {
                    tokens.push(Token { kind: TokenKind::Ge, span });
                    i += 1; col += 1;
                } else {
                    tokens.push(Token { kind: TokenKind::Gt, span });
                }
            }
            b'?' => {
                tokens.push(Token { kind: TokenKind::Question, span: Span { line: start_line, col: start_col } });
                i += 1; col += 1;
            }
            b'.' => {
                let span = Span { line: start_line, col: start_col };
                if i + 2 < bytes.len() && bytes[i + 1] == b'.' && bytes[i + 2] == b'.' {
                    tokens.push(Token { kind: TokenKind::Ellipsis, span });
                    i += 3; col += 3;
                } else {
                    tokens.push(Token { kind: TokenKind::Dot, span });
                    i += 1; col += 1;
                }
            }
            b'(' => { tokens.push(Token { kind: TokenKind::LParen, span: Span { line: start_line, col: start_col } }); i += 1; col += 1; }
            b')' => { tokens.push(Token { kind: TokenKind::RParen, span: Span { line: start_line, col: start_col } }); i += 1; col += 1; }
            b'{' => { tokens.push(Token { kind: TokenKind::LBrace, span: Span { line: start_line, col: start_col } }); i += 1; col += 1; }
            b'}' => { tokens.push(Token { kind: TokenKind::RBrace, span: Span { line: start_line, col: start_col } }); i += 1; col += 1; }
            b'[' => { tokens.push(Token { kind: TokenKind::LBracket, span: Span { line: start_line, col: start_col } }); i += 1; col += 1; }
            b']' => { tokens.push(Token { kind: TokenKind::RBracket, span: Span { line: start_line, col: start_col } }); i += 1; col += 1; }
            b';' => { tokens.push(Token { kind: TokenKind::Semicolon, span: Span { line: start_line, col: start_col } }); i += 1; col += 1; }
            b',' => { tokens.push(Token { kind: TokenKind::Comma, span: Span { line: start_line, col: start_col } }); i += 1; col += 1; }
            b':' => { tokens.push(Token { kind: TokenKind::Colon, span: Span { line: start_line, col: start_col } }); i += 1; col += 1; }

            other => {
                return Err(alloc::format!(
                    "{}:{}: unexpected character '{}' (0x{:02x})",
                    line, col, other as char, other
                ));
            }
        }
    }

    tokens.push(Token {
        kind: TokenKind::Eof,
        span: Span { line, col },
    });
    Ok(tokens)
}

fn hex_digit(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

fn lex_number(bytes: &[u8]) -> (TokenKind, usize) {
    let mut i = 0;
    let mut is_float = false;

    // Hex, octal, binary
    if bytes.len() >= 2 && bytes[0] == b'0' {
        match bytes[1] {
            b'x' | b'X' => {
                i = 2;
                let mut val: i64 = 0;
                while i < bytes.len() && bytes[i].is_ascii_hexdigit() {
                    val = val.wrapping_mul(16).wrapping_add(hex_digit(bytes[i]) as i64);
                    i += 1;
                }
                // Skip suffixes (U, L, LL, etc.)
                while i < bytes.len() && matches!(bytes[i], b'u' | b'U' | b'l' | b'L') {
                    i += 1;
                }
                return (TokenKind::IntLit(val), i);
            }
            b'b' | b'B' => {
                i = 2;
                let mut val: i64 = 0;
                while i < bytes.len() && (bytes[i] == b'0' || bytes[i] == b'1') {
                    val = val * 2 + (bytes[i] - b'0') as i64;
                    i += 1;
                }
                return (TokenKind::IntLit(val), i);
            }
            b'0'..=b'7' => {
                // Octal
                i = 1;
                let mut val: i64 = 0;
                while i < bytes.len() && bytes[i] >= b'0' && bytes[i] <= b'7' {
                    val = val * 8 + (bytes[i] - b'0') as i64;
                    i += 1;
                }
                return (TokenKind::IntLit(val), i);
            }
            b'.' => {
                is_float = true;
            }
            _ => {}
        }
    }

    // Decimal integer or float
    if !is_float {
        i = 0;
    }
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'.' {
        is_float = true;
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        is_float = true;
        i += 1;
        if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
            i += 1;
        }
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
    }

    let text = core::str::from_utf8(&bytes[..i]).unwrap_or("0");

    if is_float {
        // Simple float parse (no std)
        let val = parse_float(text);
        // Skip float suffix
        if i < bytes.len() && (bytes[i] == b'f' || bytes[i] == b'F' || bytes[i] == b'l' || bytes[i] == b'L') {
            i += 1;
        }
        (TokenKind::FloatLit(val), i)
    } else {
        let mut val: i64 = 0;
        for b in text.bytes() {
            if b.is_ascii_digit() {
                val = val.wrapping_mul(10).wrapping_add((b - b'0') as i64);
            }
        }
        // Skip integer suffixes
        while i < bytes.len() && matches!(bytes[i], b'u' | b'U' | b'l' | b'L') {
            i += 1;
        }
        (TokenKind::IntLit(val), i)
    }
}

fn parse_float(s: &str) -> f64 {
    // Minimal float parser for no_std
    let mut result: f64 = 0.0;
    let mut frac: f64 = 0.0;
    let mut frac_div: f64 = 1.0;
    let mut exp: i32 = 0;
    let mut exp_neg = false;
    let mut in_frac = false;
    let mut in_exp = false;

    for b in s.bytes() {
        if in_exp {
            if b == b'-' {
                exp_neg = true;
            } else if b == b'+' {
                // skip
            } else if b.is_ascii_digit() {
                exp = exp * 10 + (b - b'0') as i32;
            }
        } else if b == b'.' {
            in_frac = true;
        } else if b == b'e' || b == b'E' {
            in_exp = true;
        } else if b.is_ascii_digit() {
            if in_frac {
                frac_div *= 10.0;
                frac += (b - b'0') as f64 / frac_div;
            } else {
                result = result * 10.0 + (b - b'0') as f64;
            }
        }
    }

    result += frac;
    if exp_neg {
        exp = -exp;
    }
    // Apply exponent
    if exp > 0 {
        for _ in 0..exp {
            result *= 10.0;
        }
    } else if exp < 0 {
        for _ in 0..(-exp) {
            result /= 10.0;
        }
    }
    result
}

fn split_define(rest: &str) -> (String, String) {
    let trimmed = rest.trim();
    if let Some(pos) = trimmed.find(|c: char| c == ' ' || c == '\t') {
        let name = String::from(&trimmed[..pos]);
        let val = String::from(trimmed[pos..].trim());
        (name, val)
    } else {
        (String::from(trimmed), String::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_tokens() {
        let tokens = tokenize("int main() { return 0; }").unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Int));
        assert!(matches!(tokens[1].kind, TokenKind::Ident(_)));
        assert!(matches!(tokens[2].kind, TokenKind::LParen));
    }

    #[test]
    fn test_operators() {
        let tokens = tokenize("a += b->c").unwrap();
        assert!(matches!(tokens[1].kind, TokenKind::PlusAssign));
        assert!(matches!(tokens[3].kind, TokenKind::Arrow));
    }

    #[test]
    fn test_string_literal() {
        let tokens = tokenize("\"hello\\n\"").unwrap();
        if let TokenKind::StringLit(ref bytes) = tokens[0].kind {
            assert_eq!(bytes, &[b'h', b'e', b'l', b'l', b'o', b'\n']);
        } else {
            panic!("expected string literal");
        }
    }

    #[test]
    fn test_hex_literal() {
        let tokens = tokenize("0xFF").unwrap();
        assert_eq!(tokens[0].kind, TokenKind::IntLit(255));
    }

    #[test]
    fn test_preprocessor() {
        let tokens = tokenize("#include <stdio.h>\n").unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::PpInclude(_)));
    }
}
