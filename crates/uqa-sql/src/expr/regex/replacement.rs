//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` replacement syntax shares the native capture interpolator while admitting its exact output buffer before expansion.

use super::{compile, parameter, string};
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
        [_, _, _, Value::Str(flags) | Value::FixedChar(flags)] => (
            1,
            usize::from(!flags.contains('g')),
            control.copy_text(flags)?,
        ),
        [_, _, _, start] => (
            parameter(Some(start), 1, 1, "start", control)?,
            1,
            control.copy_text("")?,
        ),
        [_, _, _, start, occurrence] => (
            parameter(Some(start), 1, 1, "start", control)?,
            parameter(Some(occurrence), 1, 0, "N", control)?,
            control.copy_text("")?,
        ),
        [_, _, _, start, occurrence, flags] => (
            parameter(Some(start), 1, 1, "start", control)?,
            parameter(Some(occurrence), 1, 0, "N", control)?,
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
            control,
        )?,
        control,
    )
}

fn replace(
    input: &str,
    pattern: &str,
    replacement: &str,
    start: usize,
    occurrence: usize,
    flags: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    let base_chars = start - 1;
    let mut char_count = 0;
    let mut byte_start = input.len();
    for (offset, (byte, _)) in input.char_indices().enumerate() {
        control.check()?;
        if offset == base_chars {
            byte_start = byte;
        }
        char_count += 1;
    }
    if base_chars > char_count {
        return control.copy_text(input).map_err(Into::into);
    }
    let (prefix, tail) = input.split_at(byte_start);
    let regex = compile(pattern, flags, true, control)?;
    let replacement = postgres_replacement(replacement, control)?;
    let mut output = ProductionString::new(*control);
    output.push_str(prefix)?;
    let mut end = 0;
    for (index, capture) in regex.captures_iter(tail).enumerate() {
        control.check()?;
        if occurrence != 0 && index != occurrence - 1 {
            continue;
        }
        let matched = capture.get(0).ok_or_else(|| {
            SQLError::Internal("regex capture set omitted its mandatory full match".into())
        })?;
        output.push_str(&tail[end..matched.start()])?;
        let expanded = expand(&regex, &capture, &replacement, control)?;
        output.push_str(&expanded)?;
        end = matched.end();
        if occurrence != 0 {
            break;
        }
    }
    output.push_str(&tail[end..])?;
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
