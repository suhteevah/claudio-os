//! Assembly tokenizer: labels, mnemonics, registers, immediates, memory operands, directives.

use alloc::string::String;
use alloc::vec::Vec;

/// A token from the assembly source.
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    /// A label definition (e.g., `start:`)
    Label(String),
    /// An identifier — could be a mnemonic, register, or symbol reference
    Ident(String),
    /// Integer literal (decimal, hex 0x, octal 0o, binary 0b)
    IntLiteral(i64),
    /// String literal (for db directives)
    StringLiteral(Vec<u8>),
    /// Comma separator
    Comma,
    /// Colon (label definition)
    Colon,
    /// Open bracket [
    LBracket,
    /// Close bracket ]
    RBracket,
    /// Plus operator (in memory operands)
    Plus,
    /// Minus operator
    Minus,
    /// Multiply (for SIB scale)
    Star,
    /// Newline — statement separator
    Newline,
    /// Size prefix keyword: BYTE, WORD, DWORD, QWORD
    SizePrefix(SizeKind),
    /// PTR keyword (used after size prefix)
    Ptr,
    /// Dot for section directives (e.g., .text)
    Dot,
    /// Dollar sign (current address)
    Dollar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeKind {
    Byte,  // 8-bit
    Word,  // 16-bit
    Dword, // 32-bit
    Qword, // 64-bit
}

impl SizeKind {
    pub fn bits(self) -> u8 {
        match self {
            SizeKind::Byte => 8,
            SizeKind::Word => 16,
            SizeKind::Dword => 32,
            SizeKind::Qword => 64,
        }
    }
}

/// Tokenize assembly source into a list of tokens.
pub fn tokenize(source: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars: &[u8] = source.as_bytes();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        match c {
            // Skip spaces and tabs
            b' ' | b'\t' => {
                i += 1;
            }
            // Comments: ; to end of line
            b';' => {
                while i < chars.len() && chars[i] != b'\n' {
                    i += 1;
                }
            }
            // Newline
            b'\n' => {
                tokens.push(Token::Newline);
                i += 1;
            }
            b'\r' => {
                i += 1;
                if i < chars.len() && chars[i] == b'\n' {
                    i += 1;
                }
                tokens.push(Token::Newline);
            }
            b',' => {
                tokens.push(Token::Comma);
                i += 1;
            }
            b':' => {
                tokens.push(Token::Colon);
                i += 1;
            }
            b'[' => {
                tokens.push(Token::LBracket);
                i += 1;
            }
            b']' => {
                tokens.push(Token::RBracket);
                i += 1;
            }
            b'+' => {
                tokens.push(Token::Plus);
                i += 1;
            }
            b'-' => {
                // Could be negative number or minus operator
                // Check if next char is digit and previous token suggests operand context
                if i + 1 < chars.len() && chars[i + 1].is_ascii_digit() {
                    // Parse negative number
                    i += 1;
                    let (val, adv) = parse_number(&chars[i..]);
                    tokens.push(Token::IntLiteral(-val));
                    i += adv;
                } else {
                    tokens.push(Token::Minus);
                    i += 1;
                }
            }
            b'*' => {
                tokens.push(Token::Star);
                i += 1;
            }
            b'.' => {
                tokens.push(Token::Dot);
                i += 1;
            }
            b'$' => {
                tokens.push(Token::Dollar);
                i += 1;
            }
            // String literal
            b'"' | b'\'' => {
                let quote = c;
                i += 1;
                let mut bytes = Vec::new();
                while i < chars.len() && chars[i] != quote {
                    if chars[i] == b'\\' && i + 1 < chars.len() {
                        i += 1;
                        match chars[i] {
                            b'n' => bytes.push(b'\n'),
                            b'r' => bytes.push(b'\r'),
                            b't' => bytes.push(b'\t'),
                            b'0' => bytes.push(0),
                            b'\\' => bytes.push(b'\\'),
                            b'\'' => bytes.push(b'\''),
                            b'"' => bytes.push(b'"'),
                            other => bytes.push(other),
                        }
                    } else {
                        bytes.push(chars[i]);
                    }
                    i += 1;
                }
                if i >= chars.len() {
                    return Err(String::from("unterminated string literal"));
                }
                i += 1; // skip closing quote
                tokens.push(Token::StringLiteral(bytes));
            }
            // Number
            b'0'..=b'9' => {
                let (val, adv) = parse_number(&chars[i..]);
                tokens.push(Token::IntLiteral(val));
                i += adv;
            }
            // Identifier or keyword
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                let start = i;
                while i < chars.len()
                    && (chars[i].is_ascii_alphanumeric() || chars[i] == b'_')
                {
                    i += 1;
                }
                let word = core::str::from_utf8(&chars[start..i]).unwrap();
                let upper = {
                    let mut s = String::new();
                    for ch in word.chars() {
                        s.push(ch.to_ascii_uppercase());
                    }
                    s
                };

                match upper.as_str() {
                    "BYTE" => tokens.push(Token::SizePrefix(SizeKind::Byte)),
                    "WORD" => tokens.push(Token::SizePrefix(SizeKind::Word)),
                    "DWORD" => tokens.push(Token::SizePrefix(SizeKind::Dword)),
                    "QWORD" => tokens.push(Token::SizePrefix(SizeKind::Qword)),
                    "PTR" => tokens.push(Token::Ptr),
                    _ => {
                        // Check if next non-space char is colon -> label
                        let mut j = i;
                        while j < chars.len() && chars[j] == b' ' {
                            j += 1;
                        }
                        if j < chars.len() && chars[j] == b':' {
                            tokens.push(Token::Label(String::from(word)));
                            i = j + 1; // consume the colon
                        } else {
                            tokens.push(Token::Ident(String::from(word)));
                        }
                    }
                }
            }
            // Hash for preprocessor-like comments or section aliases
            b'#' => {
                // Treat as line comment
                while i < chars.len() && chars[i] != b'\n' {
                    i += 1;
                }
            }
            other => {
                return Err(alloc::format!("unexpected character: '{}' (0x{:02x})", other as char, other));
            }
        }
    }

    Ok(tokens)
}

fn parse_number(bytes: &[u8]) -> (i64, usize) {
    let mut i = 0;
    if bytes.len() >= 2 && bytes[0] == b'0' {
        match bytes[1] {
            b'x' | b'X' => {
                // Hex
                i = 2;
                let mut val: i64 = 0;
                while i < bytes.len() && bytes[i].is_ascii_hexdigit() {
                    let d = match bytes[i] {
                        b'0'..=b'9' => (bytes[i] - b'0') as i64,
                        b'a'..=b'f' => (bytes[i] - b'a' + 10) as i64,
                        b'A'..=b'F' => (bytes[i] - b'A' + 10) as i64,
                        _ => break,
                    };
                    val = val * 16 + d;
                    i += 1;
                }
                return (val, i);
            }
            b'b' | b'B' => {
                // Binary
                i = 2;
                let mut val: i64 = 0;
                while i < bytes.len() && (bytes[i] == b'0' || bytes[i] == b'1') {
                    val = val * 2 + (bytes[i] - b'0') as i64;
                    i += 1;
                }
                return (val, i);
            }
            b'o' | b'O' => {
                // Octal
                i = 2;
                let mut val: i64 = 0;
                while i < bytes.len() && bytes[i] >= b'0' && bytes[i] <= b'7' {
                    val = val * 8 + (bytes[i] - b'0') as i64;
                    i += 1;
                }
                return (val, i);
            }
            _ => {}
        }
    }
    // Decimal
    let mut val: i64 = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        val = val * 10 + (bytes[i] - b'0') as i64;
        i += 1;
    }
    (val, i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_tokens() {
        let tokens = tokenize("mov rax, 42\n").unwrap();
        assert!(tokens.len() >= 3);
        assert_eq!(tokens[0], Token::Ident(String::from("mov")));
        assert_eq!(tokens[1], Token::Ident(String::from("rax")));
        assert_eq!(tokens[2], Token::Comma);
        assert_eq!(tokens[3], Token::IntLiteral(42));
    }

    #[test]
    fn test_label() {
        let tokens = tokenize("start:\n  nop\n").unwrap();
        assert_eq!(tokens[0], Token::Label(String::from("start")));
    }

    #[test]
    fn test_hex_literal() {
        let tokens = tokenize("mov rax, 0xFF\n").unwrap();
        assert_eq!(tokens[3], Token::IntLiteral(255));
    }

    #[test]
    fn test_memory_operand() {
        let tokens = tokenize("mov [rbp-8], rax\n").unwrap();
        assert_eq!(tokens[1], Token::LBracket);
        assert_eq!(tokens[2], Token::Ident(String::from("rbp")));
        assert_eq!(tokens[3], Token::IntLiteral(-8));
        assert_eq!(tokens[4], Token::RBracket);
    }

    #[test]
    fn test_size_prefix() {
        let tokens = tokenize("QWORD PTR [rsp]\n").unwrap();
        assert_eq!(tokens[0], Token::SizePrefix(SizeKind::Qword));
        assert_eq!(tokens[1], Token::Ptr);
    }
}
