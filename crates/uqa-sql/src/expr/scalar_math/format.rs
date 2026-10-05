//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `format(formatstr, VARIADIC "any")` with `PostgreSQL` 18 conversion specifiers: `%[position$][-][width]type`, where the width may be `*` or `*position$` and the type is `s`, `I` or `L`.

use uqa_core::memory::ProductionControl;

use super::{Result, SQLError, Value};
use crate::expr::scalar_helpers::{quote_ident_with_control, quote_literal_with_control};

fn invalid_parameter(message: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "22023".into(),
        message: message.into(),
    }
}

fn specifier_error(message: String) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "22023".into(),
        message,
        detail: None,
        hint: Some("For a single \"%\" use \"%%\".".into()),
    }
}

fn out_of_range() -> SQLError {
    SQLError::Routine {
        sqlstate: "22003".into(),
        message: "number is out of range".into(),
    }
}

/// The argument a `*` width is read from.
#[derive(Clone, Copy)]
enum WidthArgument {
    /// The argument after the previously consumed one.
    Next,
    /// A one-based position given as `*n$`.
    Position(usize),
}

struct Specifier {
    /// One-based argument position, when given as `n$`.
    argument: Option<usize>,
    width_argument: Option<WidthArgument>,
    left_align: bool,
    width: i32,
    conversion: char,
}

/// Parse after `%`; `characters[index]` is the first character of the specifier.
struct Parser<'a> {
    characters: &'a [char],
    index: usize,
}

impl Parser<'_> {
    fn current(&self) -> char {
        self.characters[self.index]
    }

    fn advance(&mut self) -> Result<()> {
        self.index += 1;
        if self.index >= self.characters.len() {
            return Err(specifier_error(
                "unterminated format() type specifier".into(),
            ));
        }
        Ok(())
    }

    fn digits(&mut self) -> Result<Option<i32>> {
        let mut value = None::<i32>;
        while self.current().is_ascii_digit() {
            let digit = i32::try_from(self.current().to_digit(10).unwrap_or(0))
                .map_err(|_| out_of_range())?;
            value = Some(
                value
                    .unwrap_or(0)
                    .checked_mul(10)
                    .and_then(|value| value.checked_add(digit))
                    .ok_or_else(out_of_range)?,
            );
            self.advance()?;
        }
        Ok(value)
    }

    fn position(value: i32) -> Result<usize> {
        if value == 0 {
            return Err(invalid_parameter(
                "format specifies argument 0, but arguments are numbered from 1",
            ));
        }
        usize::try_from(value).map_err(|_| out_of_range())
    }

    fn specifier(&mut self) -> Result<Specifier> {
        let mut specifier = Specifier {
            argument: None,
            width_argument: None,
            left_align: false,
            width: 0,
            conversion: '\0',
        };
        if let Some(number) = self.digits()? {
            if self.current() != '$' {
                specifier.width = number;
                specifier.conversion = self.current();
                return Ok(specifier);
            }
            specifier.argument = Some(Self::position(number)?);
            self.advance()?;
        }
        while self.current() == '-' {
            specifier.left_align = true;
            self.advance()?;
        }
        if self.current() == '*' {
            self.advance()?;
            if let Some(number) = self.digits()? {
                if self.current() != '$' {
                    return Err(invalid_parameter(
                        "width argument position must be ended by \"$\"",
                    ));
                }
                specifier.width_argument = Some(WidthArgument::Position(Self::position(number)?));
                self.advance()?;
            } else {
                specifier.width_argument = Some(WidthArgument::Next);
            }
        } else if let Some(width) = self.digits()? {
            specifier.width = width;
        }
        specifier.conversion = self.current();
        Ok(specifier)
    }
}

/// The value's output-function text; `bool` prints `t` and `f`.
fn output_text(value: &Value) -> Result<String> {
    Ok(match value {
        Value::Bool(true) => "t".into(),
        Value::Bool(false) => "f".into(),
        other => crate::expr::value_to_string(other)?,
    })
}

/// A width argument: `int2` and `int4` values directly, NULL as zero, and any other type through its text as an `int4` input.
fn width_value(value: &Value) -> Result<i32> {
    match value {
        Value::Null => Ok(0),
        Value::Int(width) => i32::try_from(*width).map_err(|_| SQLError::Routine {
            sqlstate: "22003".into(),
            message: format!("value \"{width}\" is out of range for type integer"),
        }),
        other => match crate::expr::cast_value(&Value::Str(output_text(other)?), "integer")? {
            Value::Int(width) => i32::try_from(width).map_err(|_| out_of_range()),
            other => Err(SQLError::Internal(format!(
                "integer input produced {other:?}"
            ))),
        },
    }
}

fn append_padded(output: &mut String, text: &str, left_align: bool, width: i32) -> Result<()> {
    if width == 0 {
        output.push_str(text);
        return Ok(());
    }
    let (left_align, width) = if width < 0 {
        (true, width.checked_neg().ok_or_else(out_of_range)?)
    } else {
        (left_align, width)
    };
    let width = usize::try_from(width).map_err(|_| out_of_range())?;
    let padding = width.saturating_sub(text.chars().count());
    if left_align {
        output.push_str(text);
        output.extend(std::iter::repeat_n(' ', padding));
    } else {
        output.extend(std::iter::repeat_n(' ', padding));
        output.push_str(text);
    }
    Ok(())
}

/// Evaluate `format`. `arguments[0]` is the format string; a NULL format string yields NULL.
pub(super) fn format(arguments: &[Value]) -> Result<Value> {
    let Some(format) = arguments.first() else {
        return Err(SQLError::TypeMismatch(
            "format needs a format string".into(),
        ));
    };
    if matches!(format, Value::Null) {
        return Ok(Value::Null);
    }
    let control = ProductionControl::uncontrolled();
    let characters = output_text(format)?.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(characters.len());
    let mut next = 1_usize;
    let mut parser = Parser {
        characters: &characters,
        index: 0,
    };
    while parser.index < characters.len() {
        let character = parser.current();
        if character != '%' {
            output.push(character);
            parser.index += 1;
            continue;
        }
        parser.advance()?;
        if parser.current() == '%' {
            output.push('%');
            parser.index += 1;
            continue;
        }
        let specifier = parser.specifier()?;
        if !matches!(specifier.conversion, 's' | 'I' | 'L') {
            return Err(specifier_error(format!(
                "unrecognized format() type specifier \"{}\"",
                specifier.conversion
            )));
        }
        let mut width = specifier.width;
        if let Some(source) = specifier.width_argument {
            if let WidthArgument::Position(position) = source {
                next = position;
            }
            let value = arguments
                .get(next)
                .ok_or_else(|| invalid_parameter("too few arguments for format()"))?;
            width = width_value(value)?;
            next += 1;
        }
        if let Some(position) = specifier.argument {
            next = position;
        }
        let value = arguments
            .get(next)
            .ok_or_else(|| invalid_parameter("too few arguments for format()"))?;
        next += 1;
        let text = match (specifier.conversion, value) {
            ('s', Value::Null) => String::new(),
            ('L', Value::Null) => "NULL".into(),
            ('I', Value::Null) => {
                return Err(SQLError::Routine {
                    sqlstate: "22004".into(),
                    message: "null values cannot be formatted as an SQL identifier".into(),
                })
            }
            ('I', value) => quote_ident_with_control(&output_text(value)?, &control)?
                .into_uncontrolled()
                .map_err(|_| SQLError::Internal("ordinary identifier quoting owner".into()))?,
            ('L', value) => quote_literal_with_control(&output_text(value)?, &control)?
                .into_uncontrolled()
                .map_err(|_| SQLError::Internal("ordinary literal quoting owner".into()))?,
            (_, value) => output_text(value)?,
        };
        append_padded(&mut output, &text, specifier.left_align, width)?;
        parser.index += 1;
    }
    Ok(Value::Str(output))
}
