//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Map omissions and positional omissions follow their distinct stored declarations.

pub(super) fn name(name: &str) -> &str {
    name.strip_prefix("r#").unwrap_or(name)
}

macro_rules! required {
    (required) => {
        (true, true)
    };
    (optional) => {
        (false, true)
    };
    (default($value:expr)) => {
        (false, false)
    };
}
macro_rules! finish_field {
    ($parts:ident, $value:ident, required) => {
        $parts.take($value.ok_or_else(invalid)?)
    };
    ($parts:ident, $value:ident, optional) => {
        $value.map(|value| $parts.take(value)).unwrap_or(None)
    };
    ($parts:ident, $value:ident, default($default:expr)) => {
        $value.map(|value| $parts.take(value)).unwrap_or($default)
    };
}
macro_rules! fields {
    ($input:ident, $token:ident, $values:ident, $make:ident $(::$variant:ident)? { $($field:ident : $mode:ident $(($default:expr))?),* $(,)? }) => {{
        fn read_fields<'a>(
            input: &mut Input<'a, '_>,
            token: JsonToken<'a>,
            values: &Values<'_>,
        ) -> Result<Native<$make>, InputError> {
            $(let mut $field = None;)*
            let names = &[$(crate::ast::query_input::fields::name(stringify!($field))),*];
            let required = &[$(required!($mode $(($default))?)),*];
            let (map, sequence) = required.iter().enumerate().fold(
                (0, 0),
                |(map, sequence), (index, &(map_required, sequence_required))| (
                    map | (u64::from(map_required) << index),
                    sequence | (u64::from(sequence_required) << index),
                ),
            );
            let mut cursor = input.field_cursor(token, names, [map, sequence], false)?;
            while let Some((index, token)) = cursor.next(input)? {
                match names[index] {
                    $(name if name == crate::ast::query_input::fields::name(stringify!($field)) => {
                        $field = Some(input.stored(token, values)?);
                    },)*
                    _ => unreachable!("declared stored field"),
                }
            }
            let mut parts = Parts::new(input.control);
            let result = $make $(::$variant)? {
                $($field: finish_field!(parts, $field, $mode $(($default))?)),*
            };
            parts.finish(result)
        }
        read_fields($input, $token, $values)
    }};
}
macro_rules! stored_struct {
    ($ty:ident { $($field:ident : $mode:ident $(($default:expr))?),* $(,)? }) => {
        impl Stored for $ty {
            fn read<'a>(input: &mut Input<'a, '_>, token: JsonToken<'a>, values: &Values<'_>) -> Result<Native<Self>, InputError> {
                fields!(input, token, values, $ty { $($field: $mode $(($default))?),* })
            }
        }
    };
}
