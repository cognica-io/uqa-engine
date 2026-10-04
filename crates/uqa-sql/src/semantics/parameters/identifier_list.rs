//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Identifier lists in parameter values, such as `search_path`, split as `PostgreSQL`'s `SplitIdentifierString` does.

use super::units::is_c_space;

/// The longest identifier, `NAMEDATALEN - 1` bytes.
const MAX_IDENTIFIER_BYTES: usize = 63;

/// Split `text` into identifiers separated by `separator`: a double-quoted name keeps its case and collapses doubled quotes, an unquoted name runs to the separator or white space and is downcased, and every name is truncated to the length of an identifier. An empty or blank text is an empty list; `None` for invalid list syntax.
pub fn split_identifier_list(text: &str, separator: u8) -> Option<Vec<String>> {
    let bytes = text.as_bytes();
    let mut index = skip_space(bytes, 0);
    let mut names = Vec::new();
    if index == bytes.len() {
        return Some(names);
    }
    loop {
        let name = if bytes.get(index) == Some(&b'"') {
            let mut name = Vec::new();
            index += 1;
            loop {
                let quote = index + bytes[index..].iter().position(|byte| *byte == b'"')?;
                name.extend_from_slice(&bytes[index..quote]);
                if bytes.get(quote + 1) == Some(&b'"') {
                    name.push(b'"');
                    index = quote + 2;
                } else {
                    index = quote + 1;
                    break;
                }
            }
            String::from_utf8(name).expect("split at ASCII quotes")
        } else {
            let start = index;
            while index < bytes.len() && bytes[index] != separator && !is_c_space(bytes[index]) {
                index += 1;
            }
            if index == start {
                return None;
            }
            text[start..index].to_ascii_lowercase()
        };
        index = skip_space(bytes, index);
        let done = match bytes.get(index) {
            Some(byte) if *byte == separator => {
                index = skip_space(bytes, index + 1);
                false
            }
            None => true,
            Some(_) => return None,
        };
        names.push(truncate(name));
        if done {
            return Some(names);
        }
    }
}

fn skip_space(bytes: &[u8], mut index: usize) -> usize {
    while bytes.get(index).copied().is_some_and(is_c_space) {
        index += 1;
    }
    index
}

fn truncate(mut name: String) -> String {
    if name.len() > MAX_IDENTIFIER_BYTES {
        let mut end = MAX_IDENTIFIER_BYTES;
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        name.truncate(end);
    }
    name
}

#[cfg(test)]
mod tests;
