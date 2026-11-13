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
    (defaulted) => {
        (false, false)
    };
    (custom($reader:expr)) => {
        (false, false)
    };
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
    ($parts:ident, $value:ident, defaulted) => {
        $value.map(|value| $parts.take(value)).unwrap_or_default()
    };
    ($parts:ident, $value:ident, custom($reader:expr)) => {
        $value.map(|value| $parts.take(value)).unwrap_or_default()
    };
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
macro_rules! read_declared_field {
    ($input:ident, $token:ident, $values:ident, custom($reader:expr)) => {
        ($reader)($input, $token, $values)
    };
    ($input:ident, $token:ident, $values:ident, $mode:ident $(($default:expr))?) => {
        $input.stored($token, $values)
    };
}
macro_rules! fields {
    ($input:ident, $token:ident, $values:ident, $make:ident $(::$variant:ident)? { $($field:ident : $mode:ident $(($default:expr))?),* $(,)? } $(skip { $($skip:ident: $empty:expr),* $(,)? })? $(flatten($flatten:expr))?) => {{
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
            let cursor = input.field_cursor(token, names, [map, sequence], false)?;
            $(let cursor = cursor.with_flattened($flatten)?;)?
            let mut cursor = cursor;
            while let Some((index, token)) = cursor.next(input)? {
                match names[index] {
                    $(name if name == crate::ast::query_input::fields::name(stringify!($field)) => {
                        $field = Some(read_declared_field!(input, token, values, $mode $(($default))?)?);
                    },)*
                    _ => unreachable!("declared stored field"),
                }
            }
            let mut parts = Parts::new(input.control);
            let result = $make $(::$variant)? {
                $($field: finish_field!(parts, $field, $mode $(($default))?),)*
                $($($skip: $empty,)*)?
            };
            parts.finish(result)
        }
        read_fields($input, $token, $values)
    }};
}

macro_rules! stored_variant {
    ($input:ident, $values:ident, $object:ident, $ty:ident::$variant:ident) => {{
        if $object && $input.next()? != JsonToken::Null { return Err(invalid()); }
        fn construct(input: &Input<'_, '_>) -> Result<Native<$ty>, InputError> {
            Parts::new(input.control).finish($ty::$variant)
        }
        construct($input)
    }};
    ($input:ident, $values:ident, $object:ident, $ty:ident::$variant:ident ($value:ident)) => {{
        if !$object { return Err(invalid()); }
        fn construct(input: &mut Input<'_, '_>, values: &Values<'_>) -> Result<Native<$ty>, InputError> {
            let token = input.next()?;
            let mut parts = Parts::new(input.control);
            let value = $ty::$variant(parts.take(input.stored(token, values)?));
            parts.finish(value)
        }
        construct($input, $values)
    }};
    ($input:ident, $values:ident, $object:ident, $ty:ident::$variant:ident { $($field:ident: $mode:ident $(($default:expr))?),* $(,)? }) => {{
        if !$object { return Err(invalid()); }
        let token = $input.next()?;
        fields!($input, token, $values, $ty::$variant { $($field: $mode $(($default))?),* })
    }};
    ($input:ident, $values:ident, $object:ident, $ty:ident::$variant:ident => $reader:ident) => {{
        if !$object { return Err(invalid()); }
        let token = $input.next()?;
        $reader($input, token, $values)
    }};
}

macro_rules! stored_pattern {
    ($ty:ident::$variant:ident) => {
        $ty::$variant
    };
    ($ty:ident::$variant:ident ($value:ident)) => {
        $ty::$variant(..)
    };
    ($ty:ident::$variant:ident { $($fields:tt)* }) => {
        $ty::$variant { .. }
    };
    ($ty:ident::$variant:ident => $reader:ident) => {
        $ty::$variant { .. }
    };
}

macro_rules! stored_enum {
    ($ty:ident { $($variant:ident $([$($alias:literal),+])? $(($value:ident))? $({ $($field:ident: $mode:ident $(($default:expr))?),* $(,)? })? $(=> $reader:ident)?),* $(,)? }) => {
        // Exhaustive native patterns make new enum variants require an input declaration.
        const _: fn(&$ty) = |value| match value {
            $(stored_pattern!($ty::$variant $(($value))? $({ $($field: $mode $(($default))?),* })? $(=> $reader)?))|* => (),
        };
        impl Stored for $ty {
            fn read<'a>(input: &mut Input<'a, '_>, token: JsonToken<'a>, values: &Values<'_>) -> Result<Native<Self>, InputError> {
                let _ = values;
                let (tag, object) = input.tag(token)?;
                let result = match tag.as_str() {
                    $(stringify!($variant) $($(| $alias)+)? => stored_variant!(input, values, object, $ty::$variant $(($value))? $({ $($field: $mode $(($default))?),* })? $(=> $reader)?),)*
                    _ => Err(invalid()),
                }?;
                input.close_tag(object)?;
                Ok(result)
            }
        }
    };
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
