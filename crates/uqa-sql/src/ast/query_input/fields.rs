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

macro_rules! projected_finish_field {
    ($parts:ident, $value:ident, required) => {
        $parts.take($value.expect("completed required projected field"))
    };
    ($parts:ident, $value:ident, $mode:ident $(($($argument:tt)*))?) => {
        finish_field!($parts, $value, $mode $(($($argument)*))?)
    };
}
macro_rules! projected_read_field {
    ($input:ident, $work:ident, custom($reader:ident)) => {
        crate::ast::query_input::projected::special::$reader($input, $work).await
    };
    ($input:ident, $work:ident, $mode:ident $(($($argument:tt)*))?) => {
        $input.read($work).await
    };
}
macro_rules! projected_expected {
    ($ty:ident) => {
        concat!("struct ", stringify!($ty))
    };
    ($ty:ident::$variant:ident) => {
        concat!(
            "struct variant ",
            stringify!($ty),
            "::",
            stringify!($variant)
        )
    };
}
macro_rules! projected_unknown {
    ($input:ident, $work:ident, $cursor:ident, $value:ident, $names:ident, flatten($flat:ident, $field:ident)) => {
        $flat
            .observe(
                $input.child($value),
                $cursor.name().expect("flattened map field"),
                $work,
            )
            .await?;
        continue;
    };

    ($input:ident, $work:ident, $cursor:ident, $value:ident, $names:ident) => {
        continue;
    };
    ($input:ident, $work:ident, $cursor:ident, $value:ident, $names:ident, deny_unknown) => {
        return Err(uqa_core::json::JsonDiagnostic::unknown_field(
            $input
                .text($cursor.name().expect("unknown map field"), $work)
                .await?,
            $names,
            uqa_core::json::JsonPosition::unlocated(),
        )
        .into());
    };
}
macro_rules! projected_wire_name {
    ($variant:ident) => {
        stringify!($variant)
    };
    ($variant:ident as $wire:literal) => {
        $wire
    };
}
macro_rules! projected_has_flatten {
    () => {
        false
    };
    ($field:ident) => {
        true
    };
}
macro_rules! projected_fields {
    ($input:ident, $work:ident, $make:ident $(::$variant:ident)? { $($field:ident : $mode:ident $(($($argument:tt)*))?),* $(,)? } $(skip { $($skip:ident: $empty:expr),* $(,)? })? $(flatten($flat_field:ident, $keys:expr, $reader:path))? $(unknown($unknown:ident))?) => {{
        fn read_fields<'i, 'a>(
            input: crate::ast::query_input::projected::Input<'i>, work: &'a mut crate::ast::query_input::projected::Work<'i>,
        ) -> crate::ast::query_input::projected::Result<crate::ast::query_input::projected::Read<'a, $make>> {
            use crate::ast::query_input::projected as p;
            p::frame(input.control, async move {
                $(let mut $field = None;)*
                $(let mut flattened = p::FlatFields::new($keys);)?
                const NAMES: &[&str] = &[$(stringify!($field)),*];
                let required = &[$(required!($mode $(($($argument)*))?)),*];
                let (map, sequence) = required.iter().enumerate().fold(
                    (0, 0), |(map, sequence), (index, &(map_required, sequence_required))| (
                        map | (u64::from(map_required) << index),
                        sequence | (u64::from(sequence_required) << index),
                    ),
                );
                let mut cursor = p::Fields::new(input, NAMES, [map, sequence], projected_expected!($make $(::$variant)?), projected_has_flatten!($($flat_field)?), work).await?;
                while let Some((index, value)) = cursor.next(work).await? {
                    let Some(index) = index else { projected_unknown!(input, work, cursor, value, NAMES $(, $unknown)? $(, flatten(flattened, $flat_field))?); };
                    let child = input.child(value);
                    match NAMES[index] {
                        $(stringify!($field) => { $field = Some(projected_read_field!(child, work, $mode $(($($argument)*))?)?); },)*
                        _ => unreachable!("declared projected field"),
                    }
                }
                $(let flat_value = $reader(input, &flattened, work).await?;)?
                let mut parts = crate::ast::query_input::Parts::new(input.control);
                let result = $make $(::$variant)? {
                    $($field: projected_finish_field!(parts, $field, $mode $(($($argument)*))?),)*
                    $($flat_field: parts.take(flat_value),)?
                    $($($skip: $empty,)*)?
                };
                p::finish(parts, result)
            })
        }
        read_fields($input, $work)
    }};
}
macro_rules! projected_variant {
    ($input:ident, $work:ident, $tag:ident, $ty:ident::$variant:ident) => {{
        fn read_variant<'i, 'a>(
            input: crate::ast::query_input::projected::Input<'i>,
            tag: crate::ast::query_input::projected::Tag<'i>,
            work: &'a mut crate::ast::query_input::projected::Work<'i>,
        ) -> crate::ast::query_input::projected::Result<crate::ast::query_input::projected::Read<'a, $ty>> {
            crate::ast::query_input::projected::frame(input.control, async move {
                tag.unit(input, work).await?;
                input.scalar($ty::$variant)
            })
        }
        read_variant($input, $tag, $work)
    }};
    ($input:ident, $work:ident, $tag:ident, $ty:ident::$variant:ident ($value:ident)) => {{
        fn read_variant<'i, 'a>(
            input: crate::ast::query_input::projected::Input<'i>,
            tag: crate::ast::query_input::projected::Tag<'i>,
            work: &'a mut crate::ast::query_input::projected::Work<'i>,
        ) -> crate::ast::query_input::projected::Result<crate::ast::query_input::projected::Read<'a, $ty>> {
            let child = input.child(tag.payload("newtype variant")?);
            crate::ast::query_input::projected::frame(input.control, async move {
                let value = child.read(work).await?;
                let mut parts = crate::ast::query_input::Parts::new(child.control);
                let result = $ty::$variant(parts.take(value));
                crate::ast::query_input::projected::finish(parts, result)
            })
        }
        read_variant($input, $tag, $work)
    }};
    ($input:ident, $work:ident, $tag:ident, $ty:ident::$variant:ident { $($field:ident: $mode:ident $(($($argument:tt)*))?),* $(,)? }) => {{
        fn read_variant<'i, 'a>(
            input: crate::ast::query_input::projected::Input<'i>,
            tag: crate::ast::query_input::projected::Tag<'i>,
            work: &'a mut crate::ast::query_input::projected::Work<'i>,
        ) -> crate::ast::query_input::projected::Result<crate::ast::query_input::projected::Read<'a, $ty>> {
            crate::ast::query_input::projected::frame(input.control, async move {
                let child = tag.structure(input, work).await?;
                projected_fields!(child, work, $ty::$variant { $($field: $mode $(($($argument)*))?),* })?.await
            })
        }
        read_variant($input, $tag, $work)
    }};
    ($input:ident, $work:ident, $tag:ident, $ty:ident::$variant:ident => $reader:ident) => {{
        let child = $input.child($tag.payload("struct variant")?);
        crate::ast::query_input::projected::special::$reader(child, $work)
    }};
}

macro_rules! projected_struct {
    ($ty:ident { $($field:ident : $mode:ident $(($($argument:tt)*))?),* $(,)? } $(skip { $($skip:ident: $empty:expr),* $(,)? })? $(flatten($flat_field:ident, $keys:expr, $reader:path))? $(unknown($unknown:ident))?) => {
        impl crate::ast::query_input::projected::Projected for $ty {
            fn read_value<'i, 'a>(
                input: crate::ast::query_input::projected::Input<'i>, work: &'a mut crate::ast::query_input::projected::Work<'i>,
            ) -> crate::ast::query_input::projected::Result<crate::ast::query_input::projected::Read<'a, Self>> {
                projected_fields!(input, work, $ty { $($field: $mode $(($($argument)*))?),* } $(skip { $($skip: $empty),* })? $(flatten($flat_field, $keys, $reader))? $(unknown($unknown))?)
            }
        }
    };
}

macro_rules! projected_enum {
    ($ty:ident { $($variant:ident $(as $wire:literal)? $([$($alias:literal),+])? $(($value:ident))? $({ $($field:ident: $mode:ident $(($($argument:tt)*))?),* $(,)? })? $(=> $reader:ident)?),* $(,)? } $(skip { $($skip:ident $($skip_payload:tt)?),* $(,)? })? $(untagged($untagged:literal))?) => {
        const _: fn(&$ty) = |value| match value {
            $(stored_pattern!($ty::$variant $(($value))? $({ $($field: $mode $(($($argument)*))?),* })? $(=> $reader)?))|* $( $(| stored_pattern!($ty::$skip $($skip_payload)?))* )? => (),
        };
        impl crate::ast::query_input::projected::Projected for $ty {
            const BUFFERED: bool = false $(|| { let _ = $untagged; true })?;
            fn read_value<'i, 'a>(
                input: crate::ast::query_input::projected::Input<'i>, work: &'a mut crate::ast::query_input::projected::Work<'i>,
            ) -> crate::ast::query_input::projected::Result<crate::ast::query_input::projected::Read<'a, Self>> {
                use crate::ast::query_input::projected as p;
                p::frame(input.control, async move {
                    let result = async {
                        let tag = input.tag(work).await?;
                        match tag.name {
                            $(projected_wire_name!($variant $(as $wire)?) $($(| $alias)+)? => {
                                let child = projected_variant!(input, work, tag, $ty::$variant $(($value))? $({ $($field: $mode $(($($argument)*))?),* })? $(=> $reader)?)?;
                                child.await
                            },)*
                            _ => Err(input.unknown_variant(tag.name, &[$(projected_wire_name!($variant $(as $wire)?), $($($alias,)*)?)*], work).await?),
                        }
                    }.await;
                    $(let result = p::special::untagged(result, $untagged);)?
                    result
                })
            }
        }
    };
}

macro_rules! stored_enum {
    ($ty:ident { $($variant:ident $([$($alias:literal),+])? $(($value:ident))? $({ $($field:ident: $mode:ident $(($($argument:tt)*))?),* $(,)? })? $(=> $reader:ident)?),* $(,)? } $(skip { $($skip:ident $($skip_payload:tt)?),* $(,)? })?) => {
        // Exhaustive native patterns make new enum variants require an input declaration.
        const _: fn(&$ty) = |value| match value {
            $(stored_pattern!($ty::$variant $(($value))? $({ $($field: $mode $(($($argument)*))?),* })? $(=> $reader)?))|* $( $(| stored_pattern!($ty::$skip $($skip_payload)?))* )? => (),
        };
        impl Stored for $ty {
            fn read<'a>(input: &mut Input<'a, '_>, token: JsonToken<'a>, values: &Values<'_>) -> Result<Native<Self>, InputError> {
                let _ = values;
                let (tag, object) = input.tag(token)?;
                let result = match tag.as_str() {
                    $(stringify!($variant) $($(| $alias)+)? => stored_variant!(input, values, object, $ty::$variant $(($value))? $({ $($field: $mode $(($($argument)*))?),* })? $(=> $reader)?),)*
                    _ => Err(invalid()),
                }?;
                input.close_tag(object)?;
                Ok(result)
            }
        }
        projected_enum!($ty { $($variant $([$($alias),+])? $(($value))? $({ $($field: $mode $(($($argument)*))?),* })? $(=> $reader)?),* } $(skip { $($skip $($skip_payload)?),* })?);
    };
}
macro_rules! stored_struct {
    ($ty:ident { $($field:ident : $mode:ident $(($($argument:tt)*))?),* $(,)? } $(skip { $($skip:ident: $empty:expr),* $(,)? })?) => {
        impl Stored for $ty {
            fn read<'a>(input: &mut Input<'a, '_>, token: JsonToken<'a>, values: &Values<'_>) -> Result<Native<Self>, InputError> {
                fields!(input, token, values, $ty { $($field: $mode $(($($argument)*))?),* } $(skip { $($skip: $empty),* })?)
            }
        }
        projected_struct!($ty { $($field: $mode $(($($argument)*))?),* } $(skip { $($skip: $empty),* })?);
    };
}
