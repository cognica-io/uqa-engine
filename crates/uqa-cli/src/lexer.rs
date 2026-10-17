//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lossless lexical boundaries for editing incomplete SQL; this is not an SQL validator.

use std::ops::Range;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TokenKind {
    Space,
    Word,
    Identifier,
    String,
    Number,
    Comment,
    Parameter,
    Variable,
    Meta,
    Operator,
}

/// A UTF-8-aligned slice of the original input, including incomplete quoted input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Token {
    pub(crate) kind: TokenKind,
    pub(crate) span: Range<usize>,
    pub(crate) closed: bool,
}

impl Token {
    pub(crate) fn text<'a>(&self, source: &'a str) -> &'a str {
        &source[self.span.clone()]
    }
}

pub(crate) fn tokenize_sql(source: &str) -> Vec<Token> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut position = 0;
    while position < bytes.len() {
        let start = position;
        let character = source[position..].chars().next().expect("remaining input");
        let (kind, end, closed) = if character.is_whitespace() {
            (
                TokenKind::Space,
                word_end(source, position, char::is_whitespace),
                true,
            )
        } else if bytes[position..].starts_with(b"--") {
            let newline = source[position..].find('\n');
            (
                TokenKind::Comment,
                newline.map_or(bytes.len(), |n| position + n),
                true,
            )
        } else if bytes[position..].starts_with(b"/*") {
            let (end, closed) = block_end(bytes, position);
            (TokenKind::Comment, end, closed)
        } else if character == '\'' || character == '"' {
            let (end, closed) = quoted_end(bytes, position, character as u8, false);
            let kind = if character == '\'' {
                TokenKind::String
            } else {
                TokenKind::Identifier
            };
            (kind, end, closed)
        } else if matches!(character, 'e' | 'E' | 'b' | 'B' | 'x' | 'X')
            && bytes.get(position + 1) == Some(&b'\'')
        {
            let (end, closed) =
                quoted_end(bytes, position + 1, b'\'', matches!(character, 'e' | 'E'));
            (TokenKind::String, end, closed)
        } else if matches!(character, 'u' | 'U')
            && bytes.get(position + 1) == Some(&b'&')
            && bytes
                .get(position + 2)
                .is_some_and(|b| matches!(b, b'\'' | b'"'))
        {
            let quote = bytes[position + 2];
            let (end, closed) = quoted_end(bytes, position + 2, quote, false);
            let kind = if quote == b'\'' {
                TokenKind::String
            } else {
                TokenKind::Identifier
            };
            (kind, end, closed)
        } else if character == '$' {
            if let Some(delimiter) = dollar_delimiter(&source[position..]) {
                let body = position + delimiter.len();
                let close = source[body..].find(delimiter);
                (
                    TokenKind::String,
                    close.map_or(bytes.len(), |n| body + n + delimiter.len()),
                    close.is_some(),
                )
            } else if bytes.get(position + 1).is_some_and(u8::is_ascii_digit) {
                (
                    TokenKind::Parameter,
                    word_end(source, position + 1, |c| c.is_ascii_digit()),
                    true,
                )
            } else {
                (TokenKind::Operator, position + 1, true)
            }
        } else if bytes[position..].starts_with(b"::") {
            (TokenKind::Operator, position + 2, true)
        } else if bytes[position..].starts_with(b":{?") {
            let key_end = word_end(source, position + 3, variable_character);
            let closed = key_end > position + 3 && bytes.get(key_end) == Some(&b'}');
            (TokenKind::Variable, key_end + usize::from(closed), closed)
        } else if character == ':'
            && bytes
                .get(position + 1)
                .is_some_and(|b| matches!(b, b'\'' | b'"'))
        {
            let (end, closed) = quoted_end(bytes, position + 1, bytes[position + 1], false);
            (TokenKind::Variable, end, closed)
        } else if character == ':'
            && source[position + 1..]
                .chars()
                .next()
                .is_some_and(variable_character)
        {
            (
                TokenKind::Variable,
                word_end(source, position + 1, variable_character),
                true,
            )
        } else if character == '\\' {
            let end = word_end(source, position + 1, |c| {
                c.is_ascii_alphabetic() || matches!(c, '?' | '+')
            });
            (TokenKind::Meta, end, true)
        } else if character.is_ascii_digit()
            || (character == '.' && bytes.get(position + 1).is_some_and(u8::is_ascii_digit))
        {
            (TokenKind::Number, number_end(bytes, position), true)
        } else if character.is_alphabetic() || character == '_' {
            (
                TokenKind::Word,
                word_end(source, position, |c| {
                    c.is_alphanumeric() || matches!(c, '_' | '$')
                }),
                true,
            )
        } else {
            (TokenKind::Operator, position + character.len_utf8(), true)
        };
        position = end;
        tokens.push(Token {
            kind,
            span: start..end,
            closed,
        });
    }
    tokens
}

fn variable_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

fn word_end(source: &str, start: usize, accepts: impl Fn(char) -> bool) -> usize {
    source[start..]
        .char_indices()
        .find_map(|(i, c)| (!accepts(c)).then_some(start + i))
        .unwrap_or(source.len())
}

fn quoted_end(bytes: &[u8], start: usize, quote: u8, escapes: bool) -> (usize, bool) {
    let mut position = start + 1;
    while position < bytes.len() {
        if escapes && bytes[position] == b'\\' {
            position = (position + 2).min(bytes.len());
        } else if bytes[position] == quote {
            if bytes.get(position + 1) == Some(&quote) {
                position += 2;
            } else {
                return (position + 1, true);
            }
        } else {
            position += 1;
        }
    }
    (bytes.len(), false)
}

fn block_end(bytes: &[u8], start: usize) -> (usize, bool) {
    let mut depth = 1;
    let mut position = start + 2;
    while position < bytes.len() {
        if bytes[position..].starts_with(b"/*") {
            depth += 1;
            position += 2;
        } else if bytes[position..].starts_with(b"*/") {
            depth -= 1;
            position += 2;
            if depth == 0 {
                return (position, true);
            }
        } else {
            position += 1;
        }
    }
    (bytes.len(), false)
}

fn dollar_delimiter(source: &str) -> Option<&str> {
    let end = source[1..].find('$')? + 1;
    let tag = &source[1..end];
    (tag.is_empty()
        || (tag
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
            && tag.chars().all(|c| c.is_alphanumeric() || c == '_')))
    .then_some(&source[..=end])
}

fn number_end(bytes: &[u8], start: usize) -> usize {
    let mut position = start;
    if bytes[start..].starts_with(b"0x") || bytes[start..].starts_with(b"0X") {
        position += 2;
        while bytes.get(position).is_some_and(u8::is_ascii_hexdigit) {
            position += 1;
        }
        return position;
    }
    while bytes
        .get(position)
        .is_some_and(|b| b.is_ascii_digit() || *b == b'.' || *b == b'_')
    {
        position += 1;
    }
    if bytes
        .get(position)
        .is_some_and(|b| matches!(b, b'e' | b'E'))
    {
        position += 1;
        if bytes
            .get(position)
            .is_some_and(|b| matches!(b, b'+' | b'-'))
        {
            position += 1;
        }
        while bytes.get(position).is_some_and(u8::is_ascii_digit) {
            position += 1;
        }
    }
    position
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incomplete_quoted_input_remains_one_literal() {
        for source in [
            "'unfinished",
            "E'escaped\\'",
            "\"identifier",
            "$tag$body",
            "/* nested /* comment */",
        ] {
            let tokens = tokenize_sql(source);
            assert_eq!(tokens.len(), 1);
            assert!(!tokens[0].closed);
            assert_eq!(tokens[0].span, 0..source.len());
        }
    }

    #[test]
    fn parameters_variables_casts_and_exponents_have_distinct_boundaries() {
        let source = "$1 :'value' :\"name\" :raw ::int 1.2e-3";
        let tokens = tokenize_sql(source)
            .into_iter()
            .filter(|t| t.kind != TokenKind::Space)
            .collect::<Vec<_>>();
        assert_eq!(
            tokens.iter().map(|t| t.text(source)).collect::<Vec<_>>(),
            ["$1", ":'value'", ":\"name\"", ":raw", "::", "int", "1.2e-3"]
        );
        assert_eq!(tokens[0].kind, TokenKind::Parameter);
        assert_eq!(tokens[1].kind, TokenKind::Variable);
        assert_eq!(tokens[4].kind, TokenKind::Operator);
        assert_eq!(tokens[6].kind, TokenKind::Number);
    }

    #[test]
    fn every_utf8_prefix_has_lossless_contiguous_token_boundaries() {
        for source in [
            "SELECT '\u{d55c}\u{ae00};' AS \"\u{540d}\u{524d}\";",
            "/* nested /* x */ tail */ SELECT E'\\'';",
            "DO $tag$ BEGIN SELECT ';'; END $tag$;",
            "SELECT :\"name\", :'value', $12::int, 1e+2;",
        ] {
            for end in (0..=source.len()).filter(|i| source.is_char_boundary(*i)) {
                let input = &source[..end];
                let tokens = tokenize_sql(input);
                let mut position = 0;
                for token in &tokens {
                    assert_eq!(token.span.start, position);
                    assert!(input.is_char_boundary(token.span.end));
                    assert!(token.span.end > position);
                    position = token.span.end;
                }
                assert_eq!(position, end);
                assert_eq!(
                    tokens.iter().map(|t| t.text(input)).collect::<String>(),
                    input
                );
            }
        }
    }
}
