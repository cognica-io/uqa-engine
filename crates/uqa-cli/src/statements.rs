//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL statement boundary detection and splitting.

/// Byte offsets of statement-terminating semicolons: top-level `;`
/// outside single-quoted strings, double-quoted identifiers,
/// `$tag$ ... $tag$` dollar quoting, `--` line comments, and
/// (nested) `/* ... */` block comments - the same lexical rules psql
/// applies when splitting input into statements.
pub(super) fn statement_terminator_offsets(text: &str) -> Vec<usize> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let (offset, ch) = chars[i];
        match ch {
            '\'' => {
                i += 1;
                while i < chars.len() {
                    if chars[i].1 == '\'' {
                        // '' is an escaped quote inside the string.
                        if i + 1 < chars.len() && chars[i + 1].1 == '\'' {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
            }
            '"' => {
                i += 1;
                while i < chars.len() && chars[i].1 != '"' {
                    i += 1;
                }
            }
            '-' if i + 1 < chars.len() && chars[i + 1].1 == '-' => {
                while i < chars.len() && chars[i].1 != '\n' {
                    i += 1;
                }
                continue;
            }
            '/' if i + 1 < chars.len() && chars[i + 1].1 == '*' => {
                let mut depth = 1u32;
                i += 2;
                while i < chars.len() && depth > 0 {
                    if chars[i].1 == '/' && i + 1 < chars.len() && chars[i + 1].1 == '*' {
                        depth += 1;
                        i += 2;
                    } else if chars[i].1 == '*' && i + 1 < chars.len() && chars[i + 1].1 == '/' {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                continue;
            }
            '$' => {
                // Dollar quoting: `$tag$` where tag is empty or an
                // identifier. `$1` (a parameter) has a digit after the
                // dollar and is not a quote delimiter.
                let mut j = i + 1;
                while j < chars.len()
                    && (chars[j].1 == '_'
                        || chars[j].1.is_ascii_alphabetic()
                        || (j > i + 1 && chars[j].1.is_ascii_digit()))
                {
                    j += 1;
                }
                if j < chars.len() && chars[j].1 == '$' {
                    let tag: String = chars[i..=j].iter().map(|(_, c)| *c).collect();
                    // Scan forward for the identical closing tag; an
                    // unterminated dollar quote consumes the rest.
                    let mut k = j + 1;
                    let tag_chars: Vec<char> = tag.chars().collect();
                    let mut close = chars.len();
                    'scan: while k < chars.len() {
                        if chars[k].1 == '$' && k + tag_chars.len() <= chars.len() {
                            for (t, tag_ch) in tag_chars.iter().enumerate() {
                                if chars[k + t].1 != *tag_ch {
                                    k += 1;
                                    continue 'scan;
                                }
                            }
                            close = k + tag_chars.len();
                            break;
                        }
                        k += 1;
                    }
                    i = close;
                    continue;
                }
            }
            ';' => out.push(offset),
            _ => {}
        }
        i += 1;
    }
    out
}

pub(super) fn split_statements(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut start = 0;
    for offset in statement_terminator_offsets(text) {
        let statement = text[start..offset].trim();
        if !statement.is_empty() {
            out.push(statement.to_string());
        }
        start = offset + 1;
    }
    let trailing = text[start..].trim();
    if !trailing.is_empty() {
        out.push(trailing.to_string());
    }
    out
}

pub(super) fn contains_statement_terminator(text: &str) -> bool {
    !statement_terminator_offsets(text).is_empty()
}

pub(super) fn statement_is_pure_comment(statement: &str) -> bool {
    statement
        .lines()
        .all(|line| line.trim().is_empty() || line.trim().starts_with("--"))
}
