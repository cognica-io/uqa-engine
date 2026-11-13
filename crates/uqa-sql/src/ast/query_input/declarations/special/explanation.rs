//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::{
    ast::Statement,
    explain::{ExplainOption, ExplainOptionValue, ExplainOptions},
};
use uqa_core::memory::Produced;

stored_struct!(ExplainOption {
    name: required,
    value: optional
});
stored_enum!(ExplainOptionValue { Boolean(value), Integer(value), Float(value), String(value) });

fn read_options<'a>(
    input: &mut Input<'a, '_>,
    token: JsonToken<'a>,
    values: &Values<'_>,
) -> Result<Native<ExplainOptions>, InputError> {
    let (mut options, mut analyze, mut verbose, mut format) = (None, None, None, None);
    let mut cursor = input.field_cursor(
        token,
        &["options", "analyze", "verbose", "format"],
        [0, 15],
        false,
    )?;
    while let Some((index, token)) = cursor.next(input)? {
        match index {
            0 => options = Some(input.stored::<Option<Vec<ExplainOption>>>(token, values)?),
            1 => analyze = Some(input.stored::<Option<bool>>(token, values)?),
            2 => verbose = Some(input.stored::<Option<bool>>(token, values)?),
            3 => format = Some(input.stored::<Option<String>>(token, values)?),
            _ => unreachable!("EXPLAIN fields"),
        }
    }
    if let Some(options) = options {
        let (options, memory) = options.into_parts();
        if let Some(options) = options {
            return Native::new(
                input.control.finish(ExplainOptions { options }, memory)?,
                input.control,
            );
        }
    }
    let analyze = analyze.and_then(|value| value.into_parts().0);
    let verbose = verbose.and_then(|value| value.into_parts().0);
    let format = format.map(Produced::from).unwrap_or(
        input
            .control
            .finish(None, input.control.empty_reservation())?,
    );
    Native::new(
        ExplainOptions::from_legacy_with_control(analyze, verbose, format, input.control)?,
        input.control,
    )
}

pub(in crate::ast::query_input::declarations) fn read_explain<'a>(
    input: &mut Input<'a, '_>,
    token: JsonToken<'a>,
    values: &Values<'_>,
) -> Result<Native<Statement>, InputError> {
    let (text, depth) = input.encoded(token)?;
    let mut child = input.replay_at(text, depth);
    let token = child.next()?;
    let child_input = &mut child;
    let result = fields!(child_input, token, values, Statement::Explain { body: required } skip { options: ExplainOptions::default() } flatten(true))?;
    child.end()?;
    let mut child = Input::content_at_depth(text, input.control, depth);
    let token = child.next()?;
    let mut parts = Parts::new(input.control);
    let mut result = parts.take(result);
    let Statement::Explain { options, .. } = &mut result else {
        unreachable!("EXPLAIN input")
    };
    *options = parts.take(read_options(&mut child, token, values)?);
    child.end()?;
    parts.finish(result)
}
