//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary and admitted regobject names share identifier clipping, quoting and scanner rules.

use super::scanner_isspace;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString, ProductionVec},
    ValueRetentionError,
};

const IDENTIFIER_MAX_BYTES: usize = 63;

/// Parse the dotted identifier strings consumed by `PostgreSQL`'s `reg*` input functions. This follows `SplitIdentifierString`: surrounding component whitespace is ignored, quoted components collapse doubled quotes without case folding, unquoted components extend to a dot or whitespace and use ASCII case folding, and every component is clipped to `PostgreSQL`'s 63-byte identifier limit.
#[must_use]
pub fn parse_regobject_name(input: &str) -> Option<Vec<String>> {
    parse_regobject_name_with_control(input, &ProductionControl::uncontrolled())
        .expect("ordinary identifier parsing cannot be cancelled or limited")
        .map(|names| {
            names
                .into_uncontrolled()
                .expect("ordinary identifier parsing has no reservation")
        })
}

pub fn parse_regobject_name_with_control(
    input: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<String>>>, ValueRetentionError> {
    let mut scanner = Scanner {
        input,
        offset: 0,
        control: *control,
    };
    let mut names = ProductionVec::new(*control);
    loop {
        scanner.skip_while(scanner_isspace)?;
        if scanner.offset == input.len() {
            return Ok(None);
        }
        let Some(component) = scanner.component()? else {
            return Ok(None);
        };
        names.push_produced(component)?;
        scanner.skip_while(scanner_isspace)?;
        match input.as_bytes().get(scanner.offset) {
            None => return names.finish().map(Some),
            Some(b'.') => scanner.offset += 1,
            Some(_) => return Ok(None),
        }
    }
}

struct Scanner<'a, 'b> {
    input: &'a str,
    offset: usize,
    control: ProductionControl<'b>,
}

impl Scanner<'_, '_> {
    fn skip_while(&mut self, accepted: impl Fn(u8) -> bool) -> Result<(), ValueRetentionError> {
        self.control.check()?;
        for chunk in self.input.as_bytes()[self.offset..].chunks(4096) {
            self.control.check()?;
            let count = chunk.iter().position(|byte| !accepted(*byte));
            self.offset += count.unwrap_or(chunk.len());
            if count.is_some() {
                break;
            }
        }
        self.control.check()
    }

    fn component(&mut self) -> Result<Option<Produced<String>>, ValueRetentionError> {
        let mut value = ProductionString::new(self.control);
        let mut clipped = false;
        if self.input.as_bytes()[self.offset] == b'"' {
            self.offset += 1;
            loop {
                let start = self.offset;
                self.skip_while(|byte| byte != b'"')?;
                if self.offset == self.input.len() {
                    return Ok(None);
                }
                append_prefix(&mut value, &self.input[start..self.offset], &mut clipped)?;
                self.offset += 1;
                if self.input.as_bytes().get(self.offset) != Some(&b'"') {
                    break;
                }
                append_prefix(&mut value, "\"", &mut clipped)?;
                self.offset += 1;
            }
            value.finish().map(Some)
        } else {
            let start = self.offset;
            self.skip_while(|byte| byte != b'.' && !scanner_isspace(byte))?;
            if self.offset == start {
                return Ok(None);
            }
            append_prefix(&mut value, &self.input[start..self.offset], &mut clipped)?;
            let (mut value, memory) = value.finish()?.into_parts();
            value.make_ascii_lowercase();
            self.control.finish(value, memory).map(Some)
        }
    }
}

fn append_prefix(
    value: &mut ProductionString<'_>,
    text: &str,
    clipped: &mut bool,
) -> Result<(), ValueRetentionError> {
    if !*clipped {
        let mut end = text.len().min(IDENTIFIER_MAX_BYTES - value.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        value.push_str(&text[..end])?;
        *clipped = end < text.len();
    }
    Ok(())
}

#[cfg(test)]
mod tests;
