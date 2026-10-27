//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow canonical relation-reference components without copying their decoded names.

use std::{cmp::Ordering, fmt, str::Chars};

/// One decoded SQL identifier, borrowed from a decoded name or a validated quoted reference.
#[derive(Debug, Clone, Copy)]
pub struct RelationName<'a> {
    text: &'a str,
    escaped: bool,
}

impl<'a> RelationName<'a> {
    /// Borrow an already decoded identifier; punctuation remains part of the name.
    pub fn decoded(text: &'a str) -> Self {
        Self {
            text,
            escaped: false,
        }
    }

    /// Iterate decoded Unicode characters without allocating a temporary string.
    pub fn chars(self) -> impl Iterator<Item = char> + 'a {
        DecodedChars {
            chars: self.text.chars(),
            escaped: self.escaped,
        }
    }
}

struct DecodedChars<'a> {
    chars: Chars<'a>,
    escaped: bool,
}

impl Iterator for DecodedChars<'_> {
    type Item = char;
    fn next(&mut self) -> Option<char> {
        let character = self.chars.next()?;
        if self.escaped && character == '"' {
            self.chars.next();
        }
        Some(character)
    }
}

impl PartialEq for RelationName<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.chars().eq(other.chars())
    }
}
impl Eq for RelationName<'_> {}
impl PartialOrd for RelationName<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for RelationName<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.chars().cmp(other.chars())
    }
}
impl fmt::Display for RelationName<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        use fmt::Write as _;
        for character in self.chars() {
            formatter.write_char(character)?;
        }
        Ok(())
    }
}

/// A borrowed local or schema-qualified SQL relation reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelationReference<'a> {
    pub schema: Option<RelationName<'a>>,
    pub name: RelationName<'a>,
}

pub(super) enum InvalidReference {
    Empty,
    Unterminated,
    Invalid,
}

impl InvalidReference {
    pub(super) fn message(self, value: &str) -> String {
        match self {
            Self::Empty => "persisted relation name is empty".to_owned(),
            Self::Unterminated => format!("unterminated quoted relation name `{value}`"),
            Self::Invalid => format!("invalid persisted relation name `{value}`"),
        }
    }
}

pub(super) fn parse(value: &str) -> Result<RelationReference<'_>, InvalidReference> {
    if value.is_empty() {
        return Err(InvalidReference::Empty);
    }
    let mut remaining = value;
    let first = component(&mut remaining)?;
    if remaining.is_empty() {
        return Ok(RelationReference {
            schema: None,
            name: first,
        });
    }
    let name = component(&mut remaining)?;
    if !remaining.is_empty() {
        return Err(InvalidReference::Invalid);
    }
    Ok(RelationReference {
        schema: Some(first),
        name,
    })
}

fn component<'a>(remaining: &mut &'a str) -> Result<RelationName<'a>, InvalidReference> {
    let input = *remaining;
    let quoted = input.starts_with('"');
    let start = usize::from(quoted);
    let bytes = input.as_bytes();
    let mut index = start;
    let end;
    if quoted {
        loop {
            match bytes.get(index) {
                None => return Err(InvalidReference::Unterminated),
                Some(b'"') if bytes.get(index + 1) == Some(&b'"') => index += 2,
                Some(b'"') => {
                    end = index;
                    index += 1;
                    break;
                }
                Some(_) => index += 1,
            }
        }
    } else {
        while let Some(byte) = bytes.get(index) {
            match byte {
                b'.' => break,
                b'"' => return Err(InvalidReference::Invalid),
                _ => index += 1,
            }
        }
        end = index;
    }
    if end == start {
        return Err(InvalidReference::Invalid);
    }
    *remaining = match bytes.get(index) {
        None => "",
        Some(b'.') if index + 1 < bytes.len() => &input[index + 1..],
        _ => return Err(InvalidReference::Invalid),
    };
    Ok(RelationName {
        text: &input[start..end],
        escaped: quoted,
    })
}
