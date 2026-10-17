//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! psql meta-command arguments keep substituted values inside their lexical argument.

use crate::variables::Variables;

pub(crate) fn parse(source: &str, variables: &Variables) -> Result<Vec<String>, String> {
    let bytes = source.as_bytes();
    let mut arguments = Vec::new();
    let mut value = Vec::new();
    let mut started = false;
    let mut offset = 0;
    while let Some(&byte) = bytes.get(offset) {
        match byte {
            b'\'' => {
                single_quoted(bytes, &mut offset, &mut value)?;
                started = true;
            }
            b'"' => {
                // Ordinary psql arguments retain SQL identifier quotes, including doubled quotes.
                let begin = offset;
                offset += 1;
                loop {
                    let byte = *bytes
                        .get(offset)
                        .ok_or("unterminated double-quoted argument")?;
                    offset += 1;
                    if byte == b'"' {
                        if bytes.get(offset) != Some(&b'"') {
                            break;
                        }
                        offset += 1;
                    }
                }
                value.extend_from_slice(&bytes[begin..offset]);
                started = true;
            }
            b':' => {
                let length = variable_length(&source[offset..]);
                if length == 0 {
                    value.push(byte);
                    offset += 1;
                } else {
                    value.extend_from_slice(
                        variables
                            .expand(&source[offset..offset + length])
                            .as_bytes(),
                    );
                    offset += length;
                }
                started = true;
            }
            byte if byte.is_ascii_whitespace() => {
                if started {
                    arguments.push(
                        String::from_utf8(std::mem::take(&mut value))
                            .map_err(|_| "argument is not UTF-8")?,
                    );
                    started = false;
                }
                offset += 1;
            }
            _ => {
                value.push(byte);
                offset += 1;
                started = true;
            }
        }
    }
    if started {
        arguments.push(String::from_utf8(value).map_err(|_| "argument is not UTF-8")?);
    }
    Ok(arguments)
}

fn variable_length(source: &str) -> usize {
    let suffix = &source[1..];
    if let Some(name) = suffix.strip_prefix("{?") {
        let length = name
            .chars()
            .take_while(|character| crate::lexer::variable_character(*character))
            .map(char::len_utf8)
            .sum::<usize>();
        return if length != 0 && name.as_bytes().get(length) == Some(&b'}') {
            length + 4
        } else {
            0
        };
    }
    if suffix.starts_with(['\'', '"']) {
        let quote = suffix.as_bytes()[0];
        return suffix.as_bytes()[1..]
            .iter()
            .position(|byte| *byte == quote)
            .map_or(0, |end| end + 3);
    }
    let length = suffix
        .chars()
        .take_while(|character| crate::lexer::variable_character(*character))
        .map(char::len_utf8)
        .sum::<usize>();
    if length == 0 {
        0
    } else {
        length + 1
    }
}

fn single_quoted(bytes: &[u8], offset: &mut usize, output: &mut Vec<u8>) -> Result<(), String> {
    *offset += 1;
    loop {
        let byte = *bytes
            .get(*offset)
            .ok_or("unterminated single-quoted argument")?;
        *offset += 1;
        match byte {
            b'\'' => {
                if bytes.get(*offset) != Some(&b'\'') {
                    return Ok(());
                }
                output.push(byte);
                *offset += 1;
            }
            b'\\' => {
                let escaped = *bytes.get(*offset).ok_or("unterminated argument escape")?;
                *offset += 1;
                match escaped {
                    b'n' => output.push(b'\n'),
                    b'r' => output.push(b'\r'),
                    b't' => output.push(b'\t'),
                    b'b' => output.push(8),
                    b'f' => output.push(12),
                    b'0'..=b'7' => {
                        let mut value = escaped - b'0';
                        for _ in 0..2 {
                            let Some(&next @ b'0'..=b'7') = bytes.get(*offset) else {
                                break;
                            };
                            value = value.wrapping_mul(8).wrapping_add(next - b'0');
                            *offset += 1;
                        }
                        output.push(value);
                    }
                    b'x' => {
                        let mut value = None::<u8>;
                        for _ in 0..2 {
                            let Some(digit) = bytes
                                .get(*offset)
                                .and_then(|byte| char::from(*byte).to_digit(16))
                            else {
                                break;
                            };
                            value = Some(
                                value.unwrap_or(0) * 16 + u8::try_from(digit).expect("hex digit"),
                            );
                            *offset += 1;
                        }
                        output.push(value.unwrap_or(b'x'));
                    }
                    _ => output.push(escaped),
                }
            }
            _ => output.push(byte),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_and_substitution_match_independent_psql_arguments() {
        // PostgreSQL 18.4 psql: variables remain atomic; quoted variable forms keep SQL quotes.
        let mut variables = Variables::default();
        variables.set("value", "two words".into()).unwrap();
        assert_eq!(
            parse(":value :'value' :\"value\"", &variables).unwrap(),
            ["two words", "'two words'", "\"two words\""]
        );
        assert_eq!(
            parse(
                r#"'it''s' 'it\'s' 'one\ttwo' 'one\141\x62two' "two words""#,
                &variables
            )
            .unwrap(),
            ["it's", "it's", "one\ttwo", "oneabtwo", "\"two words\""]
        );
        assert_eq!(
            parse("left:value:right :{?value} ''", &variables).unwrap(),
            ["lefttwo words:right", "TRUE", ""]
        );
        assert!(parse("'unfinished", &variables).is_err());
    }
}
