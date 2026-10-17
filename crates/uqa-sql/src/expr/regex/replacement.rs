//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` replacement syntax shares the native capture interpolator while admitting its exact output buffer before expansion.

use super::{char_byte_offset, compile, parameter, search_loop, string};
use crate::{
    error::{Result, SQLError},
    expr::conversion::value_to_string_with_control,
};
use std::cell::Cell;
use uqa_core::{
    memory::{MemoryError, MemoryReservation, Produced, ProductionControl, ProductionString},
    Value,
};

pub(super) fn evaluate(
    _: &str,
    args: &[Value],
    collation: u32,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    if !(3..=6).contains(&args.len()) {
        return Err(SQLError::TypeMismatch(
            "regexp_replace takes 3 to 6 args".into(),
        ));
    }
    if args.iter().any(|value| matches!(value, Value::Null)) {
        return super::plain(Value::Null, control);
    }
    let input = value_to_string_with_control(&args[0], control)?;
    let pattern = value_to_string_with_control(&args[1], control)?;
    let replacement = value_to_string_with_control(&args[2], control)?;
    let (start, occurrence, flags) = match args {
        [_, _, _] => (1, 1, control.copy_text("")?),
        [_, _, _, Value::Str(flags) | Value::FixedChar(flags)] => {
            reject_integer_flags(flags)?;
            (
                1,
                usize::from(!flags.contains('g')),
                control.copy_text(flags)?,
            )
        }
        [_, _, _, start] => (
            parameter(Some(start), 1, 1, "start", control)?,
            1,
            control.copy_text("")?,
        ),
        [_, _, _, start, occurrence] => (
            parameter(Some(start), 1, 1, "start", control)?,
            parameter(Some(occurrence), 1, 0, "n", control)?,
            control.copy_text("")?,
        ),
        [_, _, _, start, occurrence, flags] => (
            parameter(Some(start), 1, 1, "start", control)?,
            parameter(Some(occurrence), 1, 0, "n", control)?,
            value_to_string_with_control(flags, control)?,
        ),
        _ => unreachable!("regexp_replace arity was checked"),
    };
    string(
        replace(
            &input,
            &pattern,
            &replacement,
            start,
            occurrence,
            &flags,
            collation,
            control,
        )?,
        control,
    )
}

/// `textregexreplace` resolves a four-argument call with an unknown literal to its text flags, so flags that begin with a digit most likely meant `start`; `PostgreSQL` reports that first character with a hint before it parses the flags.
fn reject_integer_flags(flags: &str) -> Result<()> {
    match flags.chars().next() {
        Some(first) if first.is_ascii_digit() => Err(SQLError::Diagnostic {
            sqlstate: "22023".into(),
            message: format!("invalid regular expression option: \"{first}\""),
            detail: None,
            hint: Some(
                "If you meant to use regexp_replace() with a start parameter, cast the fourth argument to integer explicitly."
                    .into(),
            ),
        }),
        _ => Ok(()),
    }
}

/// `replace_text_regexp`: the pattern is compiled before the search starts, the search covers the whole input from character `start`, and the `occurrence`-th match, or every match when it is zero, is replaced while the rest of the input is copied unchanged.
#[expect(
    clippy::too_many_arguments,
    reason = "keeps replace_text_regexp's inputs and the call's input collation aligned"
)]
fn replace(
    input: &str,
    pattern: &str,
    replacement: &str,
    start: usize,
    occurrence: usize,
    flags: &str,
    collation: u32,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    let regex = compile(
        pattern,
        flags,
        super::GlobalFlag::Accepted,
        collation,
        control,
    )?;
    let replacement = postgres_replacement(replacement, control)?;
    let mut output = ProductionString::new(*control);
    let Some(start) = char_byte_offset(input, start - 1, control)? else {
        output.push_str(input)?;
        return output.finish().map_err(Into::into);
    };
    let mut copied = 0;
    for (index, captures) in search_loop(&regex, input, start).enumerate() {
        control.check()?;
        if occurrence != 0 && index + 1 != occurrence {
            continue;
        }
        let matched = captures.get_match();
        output.push_str(&input[copied..matched.start()])?;
        let expanded = expand(&regex, &captures, &replacement, control)?;
        output.push_str(&expanded)?;
        copied = matched.end();
        if occurrence != 0 {
            break;
        }
    }
    output.push_str(&input[copied..])?;
    output.finish().map_err(Into::into)
}

fn postgres_replacement(
    replacement: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    let mut output = ProductionString::new(*control);
    output.reserve(replacement.len())?;
    let mut characters = replacement.chars();
    while let Some(character) = characters.next() {
        match character {
            '$' => output.push_str("$$")?,
            '\\' => match characters.next() {
                Some(digit @ '1'..='9') => {
                    output.push('$')?;
                    output.push(digit)?;
                }
                Some('&') => output.push_str("$0")?,
                Some('\\') => output.push('\\')?,
                Some(other) => {
                    output.push('\\')?;
                    output.push(other)?;
                }
                None => output.push('\\')?,
            },
            other => output.push(other)?,
        }
    }
    output.finish().map_err(Into::into)
}

/// Both native interpolation passes use this already allocated string. The first appends only literal replacement text (at most `replacement.len()` bytes); the second uses the exact length selected by the same native parser. Neither call may grow the admitted capacity. Field order keeps the buffer within its lease during error unwinding.
struct InterpolationBuffer {
    value: String,
    memory: Option<MemoryReservation>,
}

impl InterpolationBuffer {
    fn new(capacity: usize, control: &ProductionControl<'_>) -> Result<Self> {
        let mut output = ProductionString::new(*control);
        output.reserve(capacity)?;
        let (value, memory) = output.finish()?.into_parts();
        Ok(Self { value, memory })
    }

    fn finish(self, control: &ProductionControl<'_>) -> Result<Produced<String>> {
        control.finish(self.value, self.memory).map_err(Into::into)
    }
}

fn expand(
    regex: &regex::Regex,
    captures: &regex::Captures<'_>,
    replacement: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    let mut literals = InterpolationBuffer::new(replacement.len(), control)?;
    let captured_bytes = Cell::new(Some(0_usize));
    regex_automata::util::interpolate::string(
        replacement,
        |index, _| {
            if let Some(matched) = captures.get(index) {
                captured_bytes.set(
                    captured_bytes
                        .get()
                        .and_then(|bytes| bytes.checked_add(matched.len())),
                );
            }
        },
        |name| {
            regex
                .capture_names()
                .position(|candidate| candidate == Some(name))
        },
        &mut literals.value,
    );
    control.check()?;
    let length = captured_bytes
        .get()
        .and_then(|bytes| bytes.checked_add(literals.value.len()))
        .ok_or(MemoryError::SizeOverflow)?;
    drop(literals);
    let mut output = InterpolationBuffer::new(length, control)?;
    let capacity = output.value.capacity();
    captures.expand(replacement, &mut output.value);
    assert_eq!(
        output.value.len(),
        length,
        "native interpolation uses the same capture selection"
    );
    assert_eq!(
        output.value.capacity(),
        capacity,
        "native interpolation fits its admitted destination"
    );
    output.finish(control)
}
