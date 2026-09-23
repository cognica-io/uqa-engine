//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` pattern preparation shared by ordinary and controlled compilation.

use super::Options;
use crate::error::{Result, SQLError};
use uqa_core::memory::{Produced, ProductionControl, ProductionString, ProductionVec};

pub(super) struct PreparedPattern {
    pub pattern: Produced<String>,
    pub options: Options,
}

pub(super) fn prepare(
    pattern: &str,
    flags: &str,
    global_allowed: bool,
    control: &ProductionControl<'_>,
) -> Result<PreparedPattern> {
    #[derive(Clone, Copy)]
    enum Syntax {
        Advanced,
        Basic,
        Quoted,
    }
    control.check()?;
    let mut options = Options {
        case_insensitive: false,
        multi_line: false,
        dot_matches_new_line: true,
    };
    let mut expanded = false;
    let mut syntax = Syntax::Advanced;
    for flag in flags.chars() {
        control.check()?;
        match flag {
            'g' if global_allowed => {}
            // Preserve the existing PostgreSQL REG_ADVANCED/REG_EXTENDED option behavior.
            'b' | 'e' => syntax = Syntax::Basic,
            'c' => options.case_insensitive = false,
            'i' => options.case_insensitive = true,
            'm' | 'n' => {
                options.multi_line = true;
                options.dot_matches_new_line = false;
            }
            'p' => {
                options.multi_line = false;
                options.dot_matches_new_line = false;
            }
            'q' => syntax = Syntax::Quoted,
            's' => {
                options.multi_line = false;
                options.dot_matches_new_line = true;
            }
            't' => expanded = false,
            'w' => {
                options.multi_line = true;
                options.dot_matches_new_line = true;
            }
            'x' => expanded = true,
            invalid => {
                return Err(SQLError::Routine {
                    sqlstate: "22023".into(),
                    message: format!("invalid regular expression option: \"{invalid}\""),
                })
            }
        }
    }
    if matches!(syntax, Syntax::Quoted)
        && (expanded || options.multi_line || !options.dot_matches_new_line)
    {
        return Err(SQLError::Routine {
            sqlstate: "2201B".into(),
            message: "invalid regular expression: invalid argument to regex function".into(),
        });
    }
    let expanded = if expanded {
        expand_postgres_regex(pattern, control)?
    } else {
        control.copy_text(pattern)?
    };
    let syntax_pattern = match syntax {
        Syntax::Advanced => expanded,
        Syntax::Basic => {
            let pattern = postgres_basic_regex(&expanded, control)?;
            drop(expanded);
            pattern
        }
        Syntax::Quoted => {
            let pattern = quote(&expanded, control)?;
            drop(expanded);
            pattern
        }
    };
    let pattern =
        postgres_character_class_regex(&syntax_pattern, !options.dot_matches_new_line, control)?;
    drop(syntax_pattern);
    Ok(PreparedPattern { pattern, options })
}

fn scratch_characters(
    pattern: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<char>>> {
    let mut characters = ProductionVec::new(*control);
    for character in pattern.chars() {
        characters.push_copy(character)?;
    }
    Ok(characters.finish()?)
}

fn quote(pattern: &str, control: &ProductionControl<'_>) -> Result<Produced<String>> {
    // regex_syntax::escape_into uses this same predicate and emission order. The owning destination admits each append instead of exposing its raw String.
    let mut output = ProductionString::new(*control);
    for character in pattern.chars() {
        if regex_syntax::is_meta_character(character) {
            output.push('\\')?;
        }
        output.push(character)?;
    }
    Ok(output.finish()?)
}

fn postgres_character_class_regex(
    pattern: &str,
    exclude_newline: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    let characters = scratch_characters(pattern, control)?;
    let mut output = ProductionString::new(*control);
    output.reserve(pattern.len())?;
    let mut position = 0usize;
    let mut in_bracket = false;
    let mut bracket_can_close = false;
    while let Some(&character) = characters.get(position) {
        position += 1;
        if character == '\\' {
            output.push(character)?;
            if let Some(&escaped) = characters.get(position) {
                position += 1;
                output.push(escaped)?;
                if in_bracket {
                    bracket_can_close = true;
                }
            }
            continue;
        }
        if !in_bracket {
            output.push(character)?;
            if character == '[' {
                in_bracket = true;
                bracket_can_close = false;
                if characters.get(position) == Some(&'^') {
                    position += 1;
                    output.push('^')?;
                    if characters.get(position) == Some(&']') {
                        position += 1;
                        output.push(']')?;
                        bracket_can_close = true;
                    }
                    if exclude_newline {
                        output.push_str("\\n")?;
                        if characters.get(position) == Some(&'-') {
                            position += 1;
                            output.push_str("\\-")?;
                            bracket_can_close = true;
                        }
                    }
                }
            }
            continue;
        }
        if character == '[' && matches!(characters.get(position), Some('.' | ':' | '=')) {
            let delimiter = characters[position];
            output.push(character)?;
            output.push(delimiter)?;
            position += 1;
            while let Some(&nested) = characters.get(position) {
                position += 1;
                output.push(nested)?;
                if nested == delimiter && characters.get(position) == Some(&']') {
                    output.push(']')?;
                    position += 1;
                    break;
                }
            }
            bracket_can_close = true;
            continue;
        }
        if character == '[' {
            output.push_str("\\[")?;
            bracket_can_close = true;
            continue;
        }
        output.push(character)?;
        if character == ']' && bracket_can_close {
            in_bracket = false;
        } else if character != '^' || bracket_can_close {
            bracket_can_close = true;
        }
    }
    Ok(output.finish()?)
}

fn expand_postgres_regex(
    pattern: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    let mut output = ProductionString::new(*control);
    output.reserve(pattern.len())?;
    let mut characters = pattern.chars().peekable();
    let mut in_bracket = false;
    let mut bracket_can_close = false;
    while let Some(character) = characters.next() {
        control.check()?;
        if character == '\\' {
            output.push(character)?;
            if let Some(escaped) = characters.next() {
                output.push(escaped)?;
                if in_bracket {
                    bracket_can_close = true;
                }
            }
            continue;
        }
        if in_bracket {
            if character == '[' {
                if let Some(delimiter @ ('.' | ':' | '=')) = characters.peek().copied() {
                    output.push(character)?;
                    output.push(delimiter)?;
                    characters.next();
                    while let Some(nested) = characters.next() {
                        output.push(nested)?;
                        if nested == delimiter && characters.peek() == Some(&']') {
                            output.push(']')?;
                            characters.next();
                            break;
                        }
                    }
                    bracket_can_close = true;
                    continue;
                }
            }
            output.push(character)?;
            if character == ']' && bracket_can_close {
                in_bracket = false;
            } else if character != '^' || bracket_can_close {
                bracket_can_close = true;
            }
            continue;
        }
        match character {
            '[' => {
                in_bracket = true;
                bracket_can_close = false;
                output.push(character)?;
            }
            '#' => {
                for comment in characters.by_ref() {
                    control.check()?;
                    if comment == '\n' {
                        break;
                    }
                }
            }
            whitespace if postgres_expanded_regex_whitespace(whitespace) => {}
            other => output.push(other)?,
        }
    }
    Ok(output.finish()?)
}

fn postgres_expanded_regex_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'..='\u{000D}'
            | '\u{0020}'
            | '\u{1680}'
            | '\u{2000}'..='\u{2006}'
            | '\u{2008}'..='\u{200A}'
            | '\u{2028}'..='\u{2029}'
            | '\u{205F}'
            | '\u{3000}'
    )
}

fn postgres_basic_regex(
    pattern: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    let mut output = ProductionString::new(*control);
    output.reserve(pattern.len())?;
    let characters = scratch_characters(pattern, control)?;
    let mut position = 0usize;
    let mut in_bracket = false;
    let mut bracket_can_close = false;
    let mut at_subexpression_start = true;
    while let Some(&character) = characters.get(position) {
        position += 1;
        if in_bracket {
            if character == '\\' {
                output.push_str(r"\\")?;
                bracket_can_close = true;
                continue;
            }
            output.push(character)?;
            if character == ']' && bracket_can_close {
                in_bracket = false;
                at_subexpression_start = false;
            } else if character != '^' || bracket_can_close {
                bracket_can_close = true;
            }
            continue;
        }
        if character == '\\' {
            match characters.get(position).copied() {
                Some('(') => {
                    position += 1;
                    output.push('(')?;
                    at_subexpression_start = true;
                }
                Some(')') => {
                    position += 1;
                    output.push(')')?;
                    at_subexpression_start = false;
                }
                Some(bound @ ('{' | '}')) => {
                    position += 1;
                    output.push(bound)?;
                }
                Some(escaped) if escaped.is_ascii_alphabetic() => {
                    position += 1;
                    output.push(escaped)?;
                    at_subexpression_start = false;
                }
                Some(escaped) => {
                    position += 1;
                    output.push('\\')?;
                    output.push(escaped)?;
                    at_subexpression_start = false;
                }
                None => output.push('\\')?,
            }
            continue;
        }
        match character {
            '[' => {
                in_bracket = true;
                bracket_can_close = false;
                output.push(character)?;
            }
            '^' if at_subexpression_start => output.push(character)?,
            '^' => {
                output.push_str(r"\^")?;
                at_subexpression_start = false;
            }
            '$' => {
                let closes_subexpression = matches!(
                    (characters.get(position), characters.get(position + 1)),
                    (Some('\\'), Some(')'))
                );
                if position == characters.len() || closes_subexpression {
                    output.push(character)?;
                } else {
                    output.push_str(r"\$")?;
                    at_subexpression_start = false;
                }
            }
            '*' if at_subexpression_start => {
                output.push_str(r"\*")?;
                at_subexpression_start = false;
            }
            literal @ ('+' | '?' | '(' | ')' | '{' | '}' | '|') => {
                output.push('\\')?;
                output.push(literal)?;
                at_subexpression_start = false;
            }
            other => {
                output.push(other)?;
                at_subexpression_start = false;
            }
        }
    }
    Ok(output.finish()?)
}
