//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! LIKE compilation and wildcard matching share admitted and ordinary owners.

use super::casing::CaseMapping;
use crate::{
    error::{Result, SQLError},
    expr::conversion::value_to_string_with_control,
};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};

/// A LIKE/ILIKE pattern compiled once for repeated evaluation. ASCII values use a byte matcher; Unicode retains character-oriented `_` semantics. ILIKE matches the input collation's lowercase forms of the input and of the whole pattern, as `PostgreSQL`'s `Generic_Text_IC_like` does.
pub struct CompiledLikePattern {
    lowercase: Option<CaseMapping>,
    binary: bool,
    pattern_chars: Produced<Vec<LikePatternToken<char>>>,
    pattern_ascii: Option<Produced<Vec<LikePatternToken<u8>>>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LikePatternToken<T> {
    Literal(T),
    AnyOne,
    AnyMany,
    DanglingEscape,
}

impl CompiledLikePattern {
    /// Compile the selected text or bytea pattern without converting binary data to display text. `collation` is the call's input collation: a text pattern requires a determinate one, and ILIKE maps case through its provider. Bytea patterns have no collation.
    pub fn from_value_with_escape_with_control(
        pattern: &Value,
        case_insensitive: bool,
        escape: Option<&Value>,
        collation: u32,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        if let Some(pattern) = crate::expr::datums::binary_payload(pattern, control)? {
            if case_insensitive {
                return Err(SQLError::Internal("bytea has no ILIKE operator".into()));
            }
            let escape = escape
                .map(|value| crate::expr::datums::binary_payload(value, control))
                .transpose()?;
            let escape = match escape.as_ref() {
                None => Some(b'\\'),
                Some(Some(bytes)) => match bytes.bytes() {
                    [] => None,
                    [byte] => Some(*byte),
                    _ => return Err(invalid_escape()),
                },
                Some(None) => {
                    return Err(SQLError::Internal(
                        "bytea escape has an invalid carrier".into(),
                    ))
                }
            };
            let mut tokens = ProductionVec::new(*control);
            let mut bytes = pattern.bytes().iter().copied();
            while let Some(byte) = bytes.next() {
                control.check()?;
                let token = if escape == Some(byte) {
                    bytes
                        .next()
                        .map_or(LikePatternToken::DanglingEscape, LikePatternToken::Literal)
                } else {
                    match byte {
                        b'%' => LikePatternToken::AnyMany,
                        b'_' => LikePatternToken::AnyOne,
                        other => LikePatternToken::Literal(other),
                    }
                };
                tokens.push_copy(token)?;
            }
            return Ok(Self {
                lowercase: None,
                binary: true,
                pattern_chars: ProductionVec::new(*control).finish()?,
                pattern_ascii: Some(tokens.finish()?),
            });
        }
        let pattern = value_to_string_with_control(pattern, control)?;
        let escape = escape
            .map(|value| value_to_string_with_control(value, control))
            .transpose()?;
        Self::with_escape_with_control(
            &pattern,
            case_insensitive,
            escape.as_deref().map(String::as_str),
            collation,
            control,
        )
    }

    /// Report an escape that is neither empty nor one character (one byte for bytea) without compiling the pattern.
    pub(in crate::expr) fn validate_escape(
        pattern: &Value,
        escape: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<()> {
        if crate::expr::datums::binary_payload(pattern, control)?.is_some() {
            let escape = crate::expr::datums::binary_payload(escape, control)?
                .ok_or_else(|| SQLError::Internal("bytea escape has an invalid carrier".into()))?;
            return if escape.bytes().len() <= 1 {
                Ok(())
            } else {
                Err(invalid_escape())
            };
        }
        let escape = value_to_string_with_control(escape, control)?;
        escape_character(Some(&escape)).map(|_| ())
    }

    pub fn with_escape(
        pattern: &str,
        case_insensitive: bool,
        escape: Option<&str>,
        collation: u32,
    ) -> Result<Self> {
        Self::with_escape_with_control(
            pattern,
            case_insensitive,
            escape,
            collation,
            &ProductionControl::uncontrolled(),
        )
    }

    /// Compile while keeping each native token buffer under the supplied owner. ILIKE lowercases the pattern after rewriting it to the backslash escape, as `PostgreSQL` lowercases the result of `like_escape`.
    pub fn with_escape_with_control(
        pattern: &str,
        case_insensitive: bool,
        escape: Option<&str>,
        collation: u32,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        control.check()?;
        let mapping =
            CaseMapping::select(collation, if case_insensitive { "ILIKE" } else { "LIKE" })?;
        let escape = escape_character(escape)?;
        let lowercase = case_insensitive.then_some(mapping);
        let pattern_chars = match lowercase {
            Some(mapping) => {
                let normalized = if escape == Some('\\') {
                    None
                } else {
                    Some(crate::expr::pattern::normalize_escape(
                        pattern, escape, control,
                    )?)
                };
                let lowered = mapping.lowercase(
                    normalized.as_deref().map_or(pattern, String::as_str),
                    control,
                )?;
                compile(&lowered, Some('\\'), control)?
            }
            None => compile(pattern, escape, control)?,
        };
        let mut ascii = ProductionVec::new(*control);
        let mut all_ascii = true;
        for token in pattern_chars.iter() {
            control.check()?;
            let token = match token {
                LikePatternToken::Literal(character) if character.is_ascii() => {
                    LikePatternToken::Literal(*character as u8)
                }
                LikePatternToken::Literal(_) => {
                    all_ascii = false;
                    break;
                }
                LikePatternToken::AnyOne => LikePatternToken::AnyOne,
                LikePatternToken::AnyMany => LikePatternToken::AnyMany,
                LikePatternToken::DanglingEscape => LikePatternToken::DanglingEscape,
            };
            ascii.push_copy(token)?;
        }
        let pattern_ascii = if all_ascii {
            Some(ascii.finish()?)
        } else {
            None
        };
        Ok(Self {
            lowercase,
            binary: false,
            pattern_chars,
            pattern_ascii,
        })
    }

    pub fn try_is_match(&self, haystack: &str) -> Result<bool> {
        self.try_is_match_with_control(haystack, &ProductionControl::uncontrolled())
    }

    pub fn try_is_match_with_control(
        &self,
        haystack: &str,
        control: &ProductionControl<'_>,
    ) -> Result<bool> {
        control.check()?;
        let normalized = self
            .lowercase
            .map(|mapping| mapping.lowercase(haystack, control))
            .transpose()?;
        let haystack = normalized.as_deref().map_or(haystack, String::as_str);
        if let Some(pattern) = self
            .pattern_ascii
            .as_deref()
            .filter(|_| self.binary || haystack.is_ascii())
        {
            return wildcard_match(haystack.as_bytes(), pattern, control);
        }
        let mut characters = ProductionVec::new(*control);
        for character in haystack.chars() {
            characters.push_copy(character)?;
        }
        wildcard_match(&characters, &self.pattern_chars, control)
    }

    pub fn try_matches_value(&self, haystack: &Value) -> Result<bool> {
        self.try_matches_value_with_control(haystack, &ProductionControl::uncontrolled())
    }

    pub fn try_matches_value_with_control(
        &self,
        haystack: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<bool> {
        if self.binary {
            let bytes =
                crate::expr::datums::binary_payload(haystack, control)?.ok_or_else(|| {
                    SQLError::Internal("bytea LIKE source has an invalid carrier".into())
                })?;
            return wildcard_match(
                bytes.bytes(),
                self.pattern_ascii
                    .as_deref()
                    .expect("binary pattern tokens"),
                control,
            );
        }
        match haystack {
            Value::Datum(datum) => self.try_matches_value_with_control(
                &*crate::expr::datums::read_with_control(datum, control)?,
                control,
            ),
            Value::Str(text) | Value::FixedChar(text) => {
                self.try_is_match_with_control(text, control)
            }
            Value::Null => self.try_is_match_with_control("", control),
            other => self
                .try_is_match_with_control(&value_to_string_with_control(other, control)?, control),
        }
    }
}

pub(in crate::expr) fn escape_character(escape: Option<&str>) -> Result<Option<char>> {
    let Some(escape) = escape else {
        return Ok(Some('\\'));
    };
    let mut characters = escape.chars();
    let first = characters.next();
    if characters.next().is_some() {
        return Err(invalid_escape());
    }
    Ok(first)
}

pub(in crate::expr) fn invalid_escape() -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "22025".into(),
        message: "invalid escape string".into(),
        detail: None,
        hint: Some("Escape string must be empty or one character.".into()),
    }
}

fn compile(
    pattern: &str,
    escape: Option<char>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<LikePatternToken<char>>>> {
    let mut output = ProductionVec::new(*control);
    let mut characters = pattern.chars();
    while let Some(character) = characters.next() {
        control.check()?;
        if escape == Some(character) {
            let Some(literal) = characters.next() else {
                output.push_copy(LikePatternToken::DanglingEscape)?;
                break;
            };
            output.push_copy(LikePatternToken::Literal(literal))?;
            continue;
        }
        output.push_copy(match character {
            '%' => LikePatternToken::AnyMany,
            '_' => LikePatternToken::AnyOne,
            literal => LikePatternToken::Literal(literal),
        })?;
    }
    Ok(output.finish()?)
}

fn wildcard_match<T: Copy + Eq>(
    haystack: &[T],
    pattern: &[LikePatternToken<T>],
    control: &ProductionControl<'_>,
) -> Result<bool> {
    let mut haystack_index = 0;
    let mut pattern_index = 0;
    let mut star: Option<(usize, usize)> = None;
    while haystack_index < haystack.len() {
        control.check()?;
        match pattern.get(pattern_index) {
            Some(LikePatternToken::Literal(literal)) if *literal == haystack[haystack_index] => {
                haystack_index += 1;
                pattern_index += 1;
            }
            Some(LikePatternToken::AnyOne) => {
                haystack_index += 1;
                pattern_index += 1;
            }
            Some(LikePatternToken::AnyMany) => {
                star = Some((pattern_index, haystack_index));
                pattern_index += 1;
            }
            Some(LikePatternToken::DanglingEscape) => {
                return Err(SQLError::Routine {
                    sqlstate: "22025".into(),
                    message: "LIKE pattern must not end with escape character".into(),
                });
            }
            _ => {
                if let Some((star_pattern, star_haystack)) = star {
                    pattern_index = star_pattern + 1;
                    haystack_index = star_haystack + 1;
                    star = Some((star_pattern, haystack_index));
                } else {
                    return Ok(false);
                }
            }
        }
    }
    while matches!(pattern.get(pattern_index), Some(LikePatternToken::AnyMany)) {
        control.check()?;
        pattern_index += 1;
    }
    Ok(pattern_index == pattern.len())
}

#[cfg(test)]
mod tests;
