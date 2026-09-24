//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! LIKE compilation and wildcard matching share admitted and ordinary owners.

use super::casing;
use crate::{
    error::{Result, SQLError},
    expr::conversion::value_to_string_with_control,
};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};

/// A LIKE/ILIKE pattern compiled once for repeated evaluation. ASCII values use a byte matcher; Unicode retains character-oriented `_` semantics and contextual lowercase rules.
pub struct CompiledLikePattern {
    case_insensitive: bool,
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
    #[must_use]
    pub fn new(pattern: &str, case_insensitive: bool) -> Self {
        Self::with_escape(pattern, case_insensitive, None)
            .expect("the default LIKE escape is exactly one character")
    }

    #[must_use]
    pub fn from_value(pattern: &Value, case_insensitive: bool) -> Self {
        Self::new(&crate::expr::value_to_string(pattern), case_insensitive)
    }

    pub fn with_escape(
        pattern: &str,
        case_insensitive: bool,
        escape: Option<&str>,
    ) -> Result<Self> {
        Self::with_escape_with_control(
            pattern,
            case_insensitive,
            escape,
            &ProductionControl::uncontrolled(),
        )
    }

    /// Compile while keeping each native token buffer under the supplied owner.
    pub fn with_escape_with_control(
        pattern: &str,
        case_insensitive: bool,
        escape: Option<&str>,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        control.check()?;
        let escape = escape_character(escape)?;
        let pattern_chars = compile(pattern, case_insensitive, escape, control)?;
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
            case_insensitive,
            pattern_chars,
            pattern_ascii,
        })
    }

    #[must_use]
    pub fn is_match(&self, haystack: &str) -> bool {
        self.try_is_match(haystack).unwrap_or(false)
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
        let normalized = if self.case_insensitive {
            Some(casing::lowercase(haystack, control)?)
        } else {
            None
        };
        let haystack = normalized.as_deref().map_or(haystack, String::as_str);
        if let Some(pattern) = self
            .pattern_ascii
            .as_deref()
            .filter(|_| haystack.is_ascii())
        {
            return wildcard_match(haystack.as_bytes(), pattern, control);
        }
        let mut characters = ProductionVec::new(*control);
        for character in haystack.chars() {
            characters.push_copy(character)?;
        }
        wildcard_match(&characters, &self.pattern_chars, control)
    }

    #[must_use]
    pub fn matches_value(&self, haystack: &Value) -> bool {
        self.try_matches_value(haystack).unwrap_or(false)
    }

    pub fn try_matches_value(&self, haystack: &Value) -> Result<bool> {
        self.try_matches_value_with_control(haystack, &ProductionControl::uncontrolled())
    }

    pub fn try_matches_value_with_control(
        &self,
        haystack: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<bool> {
        match haystack {
            Value::Str(text) => self.try_is_match_with_control(text, control),
            Value::FixedChar(text) => {
                self.try_is_match_with_control(text.trim_end_matches(' '), control)
            }
            Value::Null => self.try_is_match_with_control("", control),
            other => self
                .try_is_match_with_control(&value_to_string_with_control(other, control)?, control),
        }
    }
}

pub(super) fn escape_character(escape: Option<&str>) -> Result<Option<char>> {
    let Some(escape) = escape else {
        return Ok(Some('\\'));
    };
    let mut characters = escape.chars();
    let first = characters.next();
    if characters.next().is_some() {
        return Err(SQLError::Routine {
            sqlstate: "22025".into(),
            message: "invalid escape string".into(),
        });
    }
    Ok(first)
}

fn compile(
    pattern: &str,
    insensitive: bool,
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
            push_literal(&mut output, literal, insensitive)?;
            continue;
        }
        match character {
            '%' => output.push_copy(LikePatternToken::AnyMany)?,
            '_' => output.push_copy(LikePatternToken::AnyOne)?,
            literal => push_literal(&mut output, literal, insensitive)?,
        }
    }
    Ok(output.finish()?)
}

fn push_literal(
    output: &mut ProductionVec<'_, LikePatternToken<char>>,
    literal: char,
    insensitive: bool,
) -> Result<()> {
    if insensitive {
        for character in literal.to_lowercase() {
            output.push_copy(LikePatternToken::Literal(character))?;
        }
    } else {
        output.push_copy(LikePatternToken::Literal(literal))?;
    }
    Ok(())
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
