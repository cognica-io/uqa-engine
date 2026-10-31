//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact routine identities admit name components and argument metadata as the scanner produces them.

use super::{
    parse_regobject_name_with_control, parse_regtype_name_with_control, scanner_isspace,
    ParsedRegprocedureName, ParsedRegtypeName, POSTGRES_FUNCTION_MAX_ARGUMENTS,
};
use crate::SQLError;
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub fn parse_regprocedure_name_with_control(
    input: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ParsedRegprocedureName>>, SQLError> {
    control.check()?;
    let mut in_quote = false;
    let mut left_parenthesis = None;
    for (offset, byte) in input.bytes().enumerate() {
        if offset % 4096 == 0 {
            control.check()?;
        }
        if byte == b'"' {
            in_quote = !in_quote;
        } else if byte == b'(' && !in_quote {
            left_parenthesis = Some(offset);
            break;
        }
    }
    let Some(left_parenthesis) = left_parenthesis else {
        let names = parse_regobject_name_with_control(input, control)?.ok_or_else(|| {
            error(
                "22P02",
                format_args!("expected a left parenthesis"),
                control,
            )
        })?;
        let (names, memory) = names.into_parts();
        return Ok(Some(control.finish(
            ParsedRegprocedureName {
                names,
                argument_types: None,
            },
            memory,
        )?));
    };
    let Some(names) = parse_regobject_name_with_control(&input[..left_parenthesis], control)?
    else {
        return Ok(None);
    };
    let bytes = input.as_bytes();
    let mut end = bytes.len();
    while end > left_parenthesis + 1 && scanner_isspace(bytes[end - 1]) {
        if end.is_multiple_of(4096) {
            control.check()?;
        }
        end -= 1;
    }
    if end <= left_parenthesis + 1 || bytes[end - 1] != b')' {
        return Err(error(
            "22P02",
            format_args!("expected a right parenthesis in routine identity \"{input}\""),
            control,
        ));
    }
    let arguments = argument_types(input, &input[left_parenthesis + 1..end - 1], control)?;
    let (names, names_memory) = names.into_parts();
    let (arguments, arguments_memory) = arguments.into_parts();
    Ok(Some(control.finish(
        ParsedRegprocedureName {
            names,
            argument_types: Some(arguments),
        },
        control.combine(names_memory, arguments_memory),
    )?))
}

fn argument_types(
    input: &str,
    arguments: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<ParsedRegtypeName>>, SQLError> {
    let bytes = arguments.as_bytes();
    let mut types = ProductionVec::new(*control);
    let mut offset = 0;
    let mut had_comma = false;
    loop {
        control.check()?;
        while bytes.get(offset).is_some_and(|byte| scanner_isspace(*byte)) {
            if offset % 4096 == 0 {
                control.check()?;
            }
            offset += 1;
        }
        if offset == bytes.len() {
            if had_comma {
                return Err(error(
                    "22P02",
                    format_args!("expected a type name in routine identity \"{input}\""),
                    control,
                ));
            }
            break;
        }
        let start = offset;
        let mut quoted = false;
        let mut nesting = 0_i32;
        while let Some(byte) = bytes.get(offset).copied() {
            if offset % 4096 == 0 {
                control.check()?;
            }
            if byte == b'"' {
                quoted = !quoted;
            } else if byte == b',' && !quoted && nesting == 0 {
                break;
            } else if !quoted {
                match byte {
                    b'(' | b'[' => nesting += 1,
                    b')' | b']' => nesting -= 1,
                    _ => {}
                }
            }
            offset += 1;
        }
        if quoted || nesting != 0 {
            return Err(error(
                "22P02",
                format_args!("improper type name in routine identity \"{input}\""),
                control,
            ));
        }
        let mut type_end = offset;
        while type_end > start && scanner_isspace(bytes[type_end - 1]) {
            type_end -= 1;
        }
        let Some(ty) = parse_regtype_name_with_control(&arguments[start..type_end], control)?
        else {
            return Err(
                match control.format(format_args!(
                    "invalid type name in routine identity \"{input}\""
                )) {
                    Ok(message) => SQLError::Parse(message.into_parts().0),
                    Err(error) => error.into(),
                },
            );
        };
        if types.len() == POSTGRES_FUNCTION_MAX_ARGUMENTS {
            return Err(error(
                "54023",
                format_args!("too many arguments in routine identity \"{input}\""),
                control,
            ));
        }
        types.push_produced(ty)?;
        had_comma = bytes.get(offset) == Some(&b',');
        if had_comma {
            offset += 1;
        }
    }
    Ok(types.finish()?)
}

fn error(
    sqlstate: &str,
    message: std::fmt::Arguments<'_>,
    control: &ProductionControl<'_>,
) -> SQLError {
    match control.format(message) {
        Ok(message) => SQLError::Routine {
            sqlstate: sqlstate.into(),
            message: message.into_parts().0,
        },
        Err(error) => error.into(),
    }
}
