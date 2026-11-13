//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    invalid, AssignmentStep as GenericAssignmentStep, AssignmentTarget, AssignmentTargets, Expr,
    Input, InputError, JsonToken, MultipleAssignmentTargets as GenericMultipleAssignmentTargets,
    Native, Parts, Stored, Values,
};

type AssignmentStep = GenericAssignmentStep<Expr>;
type MultipleAssignmentTargets = GenericMultipleAssignmentTargets<Expr>;
stored_struct!(MultipleAssignmentTargets {
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

stored_enum!(AssignmentStep { Field(value), Index(value), Slice { lower: optional, upper: optional } });
