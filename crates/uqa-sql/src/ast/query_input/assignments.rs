//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    invalid, AssignmentStep, AssignmentTarget, AssignmentTargets, Expr, Input, InputError,
    JsonToken, MultipleAssignmentTargets, Native, Parts, Stored, Values,
};

type QueryAssignmentStep = AssignmentStep<Expr>;
type MultipleTargets = MultipleAssignmentTargets<Expr>;
stored_struct!(MultipleTargets {
    targets: required,
    source_positions: required,
    source_width: required
});

impl Stored for AssignmentTarget {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Native<Self>, InputError> {
        if matches!(token, JsonToken::String(_)) {
            let mut parts = Parts::new(input.control);
            let result = Self {
                column: parts.take(input.text(token)?),
                indirection: Vec::new(),
            };
            return parts.finish(result);
        }
        fields!(
            input,
            token,
            values,
            AssignmentTarget {
                column: required,
                indirection: required
            }
        )
    }
}

impl Stored for AssignmentTargets {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Native<Self>, InputError> {
        let (text, depth) = input.encoded(token)?;
        let mut candidate = Input::content_at_depth(text, input.control, depth);
        let first = candidate.next()?;
        let single = AssignmentTarget::read(&mut candidate, first, values).and_then(|value| {
            candidate.end()?;
            Ok(value)
        });
        let mut parts = Parts::new(input.control);
        let value = match single {
            Ok(single) => Self::Single(parts.take(single)),
            Err(InputError::Invalid) => {
                let mut candidate = Input::content_at_depth(text, input.control, depth);
                let first = candidate.next()?;
                let group = candidate.stored(first, values)?;
                candidate.end()?;
                Self::Multiple(parts.take(group))
            }
            Err(error) => return Err(error),
        };
        parts.finish(value)
    }
}

impl Stored for AssignmentStep<Expr> {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Native<Self>, InputError> {
        let (tag, object) = input.tag(token)?;
        if !object {
            return Err(invalid());
        }
        let token = input.next()?;
        let mut parts = Parts::new(input.control);
        let result = match tag.as_str() {
            "Field" => {
                let value = Self::Field(parts.take(input.stored(token, values)?));
                parts.finish(value)
            }
            "Index" => {
                let value = Self::Index(parts.take(input.stored(token, values)?));
                parts.finish(value)
            }
            "Slice" => fields!(
                input,
                token,
                values,
                QueryAssignmentStep::Slice {
                    lower: optional,
                    upper: optional
                }
            ),
            _ => Err(invalid()),
        }?;
        input.close_tag(object)?;
        Ok(result)
    }
}
