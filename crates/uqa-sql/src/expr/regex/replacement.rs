//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `regexp_replace`: `PostgreSQL`'s `replace_text_regexp` and its replacement escapes, writing into one admitted output string.

use super::{argument_text, engine, flags::ReFlags, parameter, plain, string};
use crate::error::{Result, SQLError};
use crate::expr::text_size::TextBuffer;
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};
use uqa_pg_regex::{CompileFlags, MatchSpan, REG_NOSUB};

/// The whole match and the subexpressions `\1` to `\9` can name.
const REPLACEMENT_SLOTS: usize = 10;

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
        return plain(Value::Null, control);
    }
    let input = argument_text(&args[0], control)?;
    let pattern = argument_text(&args[1], control)?;
    let replacement = argument_text(&args[2], control)?;
    let (start, occurrence, flags) = match args {
        [_, _, _] => (1, None, ReFlags::ADVANCED),
        [_, _, _, Value::Str(flags) | Value::FixedChar(flags)] => {
            reject_integer_flags(flags)?;
            (1, None, ReFlags::parse(flags)?)
        }
        [_, _, _, start] => (
            parameter(Some(start), 1, 1, "start", control)?,
            None,
            ReFlags::ADVANCED,
        ),
        [_, _, _, start, occurrence] => (
            parameter(Some(start), 1, 1, "start", control)?,
            Some(parameter(Some(occurrence), 1, 0, "n", control)?),
            ReFlags::ADVANCED,
        ),
        [_, _, _, start, occurrence, flags] => {
            let start = parameter(Some(start), 1, 1, "start", control)?;
            let occurrence = parameter(Some(occurrence), 1, 0, "n", control)?;
            let flags = argument_text(flags, control)?;
            (start, Some(occurrence), ReFlags::parse(&flags)?)
        }
        _ => unreachable!("regexp_replace arity was checked"),
    };
    // Without an explicit `n`, `g` selects every match and its absence the first one.
    let occurrence = occurrence.unwrap_or(usize::from(!flags.global));
    string(
        replace(
            &input,
            &pattern,
            &replacement,
            flags.cflags,
            collation,
            start - 1,
            occurrence,
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

/// What a replacement string asks of each match, `check_replace_text_has_escape`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Escapes {
    /// No backslash needs processing: the replacement is copied as it is.
    None,
    /// Backslash sequences, but no subexpression reference.
    Literal,
    /// A reference `\1` to `\9`, which needs the subexpression spans.
    Subexpressions,
}

impl Escapes {
    /// Scan for backslashes as `PostgreSQL` does, skipping the character after each one; a backslash at the end needs no processing.
    fn of(replacement: &str) -> Self {
        let bytes = replacement.as_bytes();
        let mut escapes = Self::None;
        let mut index = 0;
        while let Some(offset) = bytes[index..].iter().position(|byte| *byte == b'\\') {
            index += offset + 1;
            let Some(next) = bytes.get(index) else {
                break;
            };
            if (b'1'..=b'9').contains(next) {
                return Self::Subexpressions;
            }
            escapes = Self::Literal;
            index += 1;
        }
        escapes
    }
}

/// `replace_text_regexp`: the pattern is compiled before the search, which covers the whole input from code point `search_start`; the `occurrence`-th match, or every match when it is zero, is replaced and the rest of the input is copied unchanged.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors replace_text_regexp's inputs, compile options and collation"
)]
fn replace(
    input: &str,
    pattern: &str,
    replacement: &str,
    cflags: CompileFlags,
    collation: u32,
    search_start: usize,
    occurrence: usize,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    let points = engine::code_points(input, control)?;
    let escapes = Escapes::of(replacement);
    // Without subexpression references only the whole match is needed.
    let (cflags, slots) = if escapes < Escapes::Subexpressions {
        (cflags | REG_NOSUB, 1)
    } else {
        (cflags, REPLACEMENT_SLOTS)
    };
    let call = engine::RegexCall::new(control);
    let mut allowance = call.allowance();
    let mut regex = call.compile(pattern, cflags, collation, &mut allowance)?;
    let mut spans = [MatchSpan::UNSET; REPLACEMENT_SLOTS];
    let spans = &mut spans[..slots];
    let mut output = TextBuffer::new(control);
    let mut remaining = input;
    let mut copied = 0;
    let mut matches = 0_usize;
    let mut search = search_start;
    while search <= points.len() {
        call.check()?;
        if !call.execute(&mut regex, &points, search, spans)? {
            break;
        }
        let whole = engine::whole_match(spans[0])?;
        matches += 1;
        if occurrence > 0 && matches != occurrence {
            // The skipped match is copied with the text after it.
            search = whole.end + usize::from(whole.is_empty());
            continue;
        }
        output.push_str(take_chars(&mut remaining, whole.start - copied, control)?)?;
        let whole_text = take_chars(&mut remaining, whole.len(), control)?;
        if escapes == Escapes::None {
            output.push_str(replacement)?;
        } else {
            expand(
                &mut output,
                replacement,
                spans,
                whole_text,
                whole.start,
                control,
            )?;
        }
        copied = whole.end;
        if occurrence > 0 {
            break;
        }
        search = whole.end + usize::from(whole.is_empty());
    }
    drop(regex);
    output.push_str(remaining)?;
    output.finish()
}

/// Consume a character range of the original UTF-8 input without building another decoded string.
fn take_chars<'a>(
    remaining: &mut &'a str,
    count: usize,
    control: &ProductionControl<'_>,
) -> Result<&'a str> {
    let mut bytes = 0;
    let mut chars = remaining.chars();
    for index in 0..count {
        if index % 1024 == 0 {
            control.check()?;
        }
        let character = chars.next().ok_or_else(|| {
            SQLError::Internal("regular expression span exceeds the input".into())
        })?;
        bytes += character.len_utf8();
    }
    let (prefix, tail) = remaining.split_at(bytes);
    *remaining = tail;
    Ok(prefix)
}

/// `appendStringInfoRegexpSubstr`: `\1` to `\9` insert a subexpression and `\&` the whole match, each empty when it did not take part in the match; `\\` inserts one backslash, and any other backslash is copied as written.
fn expand(
    output: &mut TextBuffer<'_>,
    mut replacement: &str,
    spans: &[MatchSpan],
    whole_text: &str,
    match_start: usize,
    control: &ProductionControl<'_>,
) -> Result<()> {
    // Replacement escapes can reference the first nine subexpressions. Decode each requested range once.
    let mut captures = [None; REPLACEMENT_SLOTS];
    captures[0] = Some(whole_text);
    while let Some((prefix, rest)) = replacement.split_once('\\') {
        output.push_str(prefix)?;
        replacement = rest;
        let Some(&escape) = rest.as_bytes().first() else {
            output.push('\\')?;
            return Ok(());
        };
        let slot = match escape {
            b'1'..=b'9' => usize::from(escape - b'0'),
            b'&' => 0,
            b'\\' => {
                output.push('\\')?;
                replacement = &rest[1..];
                continue;
            }
            _ => {
                output.push('\\')?;
                continue;
            }
        };
        replacement = &rest[1..];
        let capture = if let Some(cached) = captures[slot] {
            cached
        } else {
            let text = match spans.get(slot).and_then(|span| span.range()) {
                Some(range) => {
                    let mut input = whole_text;
                    let offset = range.start.checked_sub(match_start).ok_or_else(|| {
                        SQLError::Internal("regular expression capture before match".into())
                    })?;
                    take_chars(&mut input, offset, control)?;
                    take_chars(&mut input, range.len(), control)?
                }
                None => "",
            };
            captures[slot] = Some(text);
            text
        };
        output.push_str(capture)?;
    }
    output.push_str(replacement)
}
