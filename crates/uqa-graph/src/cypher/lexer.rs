//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cypher tokenizer. Produces a flat [`Token`] vector consumed by the
//! recursive-descent parser. Keywords are case-insensitive — they
//! arrive as `Identifier` tokens whose uppercased text the parser
//! matches against keyword strings.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Integer,
    Float,
    String,
    Identifier,
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Colon,
    Comma,
    Dot,
    DotDot,
    Pipe,
    Dollar,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Caret,
    Eq,
    Neq,
    Lt,
    Gt,
    Lte,
    Gte,
    PlusEq,
    /// `=~` regular-expression match operator.
    RegexMatch,
    ArrowRight,
    ArrowLeft,
    Eof,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub value: String,
    pub pos: usize,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LexError {
    #[error("unexpected character {ch:?} at position {position}")]
    UnexpectedChar { ch: char, position: usize },
    #[error("unterminated string starting at position {position}")]
    UnterminatedString { position: usize },
    #[error("unterminated backtick identifier at position {position}")]
    UnterminatedBacktick { position: usize },
}

pub fn tokenize(source: &str) -> Result<Vec<Token>, LexError> {
    let bytes = source.as_bytes();
    let n = bytes.len();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < n {
        let ch = bytes[i] as char;
        if ch.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if let Some(next) = skip_comment(bytes, i) {
            i = next;
            continue;
        }
        if ch == '\'' || ch == '"' {
            let (tok, next) = scan_string(source, i, ch)?;
            tokens.push(tok);
            i = next;
            continue;
        }
        if ch.is_ascii_digit()
            || (ch == '.' && i + 1 < n && (bytes[i + 1] as char).is_ascii_digit())
        {
            let (tok, next) = scan_number(source, i);
            tokens.push(tok);
            i = next;
            continue;
        }
        if ch.is_ascii_alphabetic() || ch == '_' {
            let (tok, next) = scan_ident(source, i);
            tokens.push(tok);
            i = next;
            continue;
        }
        if ch == '`' {
            let (tok, next) = scan_backtick_ident(source, i)?;
            tokens.push(tok);
            i = next;
            continue;
        }
        if let Some((tok, next)) = scan_two_char_symbol(source, i) {
            tokens.push(tok);
            i = next;
            continue;
        }
        let kind = single_char_kind(ch).ok_or(LexError::UnexpectedChar { ch, position: i })?;
        tokens.push(Token {
            kind,
            value: ch.to_string(),
            pos: i,
        });
        i += 1;
    }
    tokens.push(Token {
        kind: TokenKind::Eof,
        value: String::new(),
        pos: n,
    });
    Ok(tokens)
}

/// If `bytes[i..]` starts a `//` or `/* */` comment, return the index
/// just past the comment; otherwise `None`.
fn skip_comment(bytes: &[u8], i: usize) -> Option<usize> {
    let n = bytes.len();
    if !(i + 1 < n && bytes[i] == b'/') {
        return None;
    }
    match bytes[i + 1] {
        b'/' => {
            let mut j = i;
            while j < n && bytes[j] != b'\n' {
                j += 1;
            }
            Some(j)
        }
        b'*' => {
            let mut j = i + 2;
            while j + 1 < n && !(bytes[j] == b'*' && bytes[j + 1] == b'/') {
                j += 1;
            }
            Some((j + 2).min(n))
        }
        _ => None,
    }
}

fn scan_two_char_symbol(source: &str, i: usize) -> Option<(Token, usize)> {
    if i + 2 > source.len() {
        return None;
    }
    let bytes = source.as_bytes();
    let pair = (bytes[i], bytes[i + 1]);
    let kind = match pair {
        (b'<', b'>') => TokenKind::Neq,
        (b'<', b'=') => TokenKind::Lte,
        (b'>', b'=') => TokenKind::Gte,
        (b'-', b'>') => TokenKind::ArrowRight,
        (b'<', b'-') => TokenKind::ArrowLeft,
        (b'+', b'=') => TokenKind::PlusEq,
        (b'=', b'~') => TokenKind::RegexMatch,
        (b'.', b'.') => TokenKind::DotDot,
        _ => return None,
    };
    // The match above only succeeds when both bytes are ASCII, so
    // building a 2-char ASCII string from them is safe.
    let value = String::from_utf8(vec![pair.0, pair.1]).expect("ASCII bytes");
    Some((
        Token {
            kind,
            value,
            pos: i,
        },
        i + 2,
    ))
}

fn single_char_kind(ch: char) -> Option<TokenKind> {
    Some(match ch {
        '(' => TokenKind::LParen,
        ')' => TokenKind::RParen,
        '[' => TokenKind::LBracket,
        ']' => TokenKind::RBracket,
        '{' => TokenKind::LBrace,
        '}' => TokenKind::RBrace,
        ':' => TokenKind::Colon,
        ',' => TokenKind::Comma,
        '.' => TokenKind::Dot,
        '|' => TokenKind::Pipe,
        '$' => TokenKind::Dollar,
        '+' => TokenKind::Plus,
        '-' => TokenKind::Minus,
        '*' => TokenKind::Star,
        '/' => TokenKind::Slash,
        '%' => TokenKind::Percent,
        '^' => TokenKind::Caret,
        '=' => TokenKind::Eq,
        '<' => TokenKind::Lt,
        '>' => TokenKind::Gt,
        _ => return None,
    })
}

fn scan_string(source: &str, start: usize, quote: char) -> Result<(Token, usize), LexError> {
    // Iterate proper UTF-8 chars over the body of the string literal,
    // not raw bytes — a previous bytewise loop silently corrupted any
    // non-ASCII codepoint by reinterpreting each byte as Latin-1.
    let body_start = start + quote.len_utf8();
    let mut buf = String::new();
    let mut iter = source[body_start..].char_indices().peekable();
    while let Some((rel, ch)) = iter.next() {
        let abs = body_start + rel;
        if ch == '\\' {
            if let Some((_, esc)) = iter.next() {
                let mapped = match esc {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    '\\' => '\\',
                    _ if esc == quote => quote,
                    _ => esc,
                };
                buf.push(mapped);
                continue;
            }
            return Err(LexError::UnterminatedString { position: start });
        }
        if ch == quote {
            // Doubled quote escapes the quote inside the same quote.
            if let Some((_, next)) = iter.peek() {
                if *next == quote {
                    iter.next();
                    buf.push(quote);
                    continue;
                }
            }
            return Ok((
                Token {
                    kind: TokenKind::String,
                    value: buf,
                    pos: start,
                },
                abs + ch.len_utf8(),
            ));
        }
        buf.push(ch);
    }
    Err(LexError::UnterminatedString { position: start })
}

fn scan_number(source: &str, start: usize) -> (Token, usize) {
    let bytes = source.as_bytes();
    let n = bytes.len();
    let mut i = start;
    let mut has_dot = false;
    while i < n {
        let ch = bytes[i] as char;
        if ch.is_ascii_digit() {
            i += 1;
            continue;
        }
        if ch == '.' {
            if has_dot {
                break;
            }
            // Stop on `..` (range) or `.<non-digit>` (method/property access).
            if i + 1 < n && bytes[i + 1] as char == '.' {
                break;
            }
            if i + 1 < n && !(bytes[i + 1] as char).is_ascii_digit() {
                break;
            }
            has_dot = true;
            i += 1;
            continue;
        }
        break;
    }
    if i < n && (bytes[i] as char == 'e' || bytes[i] as char == 'E') {
        i += 1;
        if i < n && (bytes[i] as char == '+' || bytes[i] as char == '-') {
            i += 1;
        }
        while i < n && (bytes[i] as char).is_ascii_digit() {
            i += 1;
        }
        has_dot = true;
    }
    let text = &source[start..i];
    let kind = if has_dot {
        TokenKind::Float
    } else {
        TokenKind::Integer
    };
    (
        Token {
            kind,
            value: text.to_string(),
            pos: start,
        },
        i,
    )
}

fn scan_ident(source: &str, start: usize) -> (Token, usize) {
    let bytes = source.as_bytes();
    let n = bytes.len();
    let mut i = start;
    while i < n {
        let ch = bytes[i] as char;
        if ch.is_ascii_alphanumeric() || ch == '_' {
            i += 1;
        } else {
            break;
        }
    }
    let text = &source[start..i];
    (
        Token {
            kind: TokenKind::Identifier,
            value: text.to_string(),
            pos: start,
        },
        i,
    )
}

fn scan_backtick_ident(source: &str, start: usize) -> Result<(Token, usize), LexError> {
    let bytes = source.as_bytes();
    let n = bytes.len();
    let mut i = start + 1;
    while i < n && bytes[i] as char != '`' {
        i += 1;
    }
    if i >= n {
        return Err(LexError::UnterminatedBacktick { position: start });
    }
    let text = &source[start + 1..i];
    Ok((
        Token {
            kind: TokenKind::Identifier,
            value: text.to_string(),
            pos: start,
        },
        i + 1,
    ))
}

/// `true` if the token is an identifier whose uppercased text matches.
pub fn is_keyword(token: &Token, keyword: &str) -> bool {
    token.kind == TokenKind::Identifier && token.value.eq_ignore_ascii_case(keyword)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        tokenize(src).unwrap().iter().map(|t| t.kind).collect()
    }

    #[test]
    fn tokenize_basic_query() {
        let ks = kinds("MATCH (n:Person) RETURN n.name");
        assert_eq!(
            ks,
            vec![
                TokenKind::Identifier,
                TokenKind::LParen,
                TokenKind::Identifier,
                TokenKind::Colon,
                TokenKind::Identifier,
                TokenKind::RParen,
                TokenKind::Identifier,
                TokenKind::Identifier,
                TokenKind::Dot,
                TokenKind::Identifier,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn tokenize_arrow_and_range() {
        let toks = tokenize("[*1..3]->").unwrap();
        let kinds: Vec<TokenKind> = toks.iter().map(|t| t.kind).collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::LBracket,
                TokenKind::Star,
                TokenKind::Integer,
                TokenKind::DotDot,
                TokenKind::Integer,
                TokenKind::RBracket,
                TokenKind::ArrowRight,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn tokenize_strings_and_escapes() {
        let toks = tokenize(r#"'a''b' "c\n""#).unwrap();
        assert_eq!(toks[0].kind, TokenKind::String);
        assert_eq!(toks[0].value, "a'b");
        assert_eq!(toks[1].kind, TokenKind::String);
        assert_eq!(toks[1].value, "c\n");
    }

    #[test]
    fn tokenize_numbers() {
        let toks = tokenize("1 2.5 1e3 1.2e-4").unwrap();
        assert_eq!(toks[0].kind, TokenKind::Integer);
        assert_eq!(toks[1].kind, TokenKind::Float);
        assert_eq!(toks[2].kind, TokenKind::Float);
        assert_eq!(toks[3].kind, TokenKind::Float);
    }

    #[test]
    fn tokenize_skips_comments() {
        let toks = tokenize("MATCH // comment\n n /* block */ RETURN n").unwrap();
        let kinds: Vec<TokenKind> = toks.iter().map(|t| t.kind).collect();
        assert!(kinds.iter().all(|k| !matches!(k, TokenKind::Slash)));
    }

    #[test]
    fn keyword_match_case_insensitive() {
        let toks = tokenize("match").unwrap();
        assert!(is_keyword(&toks[0], "MATCH"));
    }

    #[test]
    fn backtick_identifier_preserves_inner_text() {
        let toks = tokenize("`spaced name`").unwrap();
        assert_eq!(toks[0].kind, TokenKind::Identifier);
        assert_eq!(toks[0].value, "spaced name");
    }
}
