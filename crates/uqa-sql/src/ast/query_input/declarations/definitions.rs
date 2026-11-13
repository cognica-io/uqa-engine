//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::special::{read_estimate, read_owner};
use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::ast::{CreateFunction, RoutineSecurityAttributes};
use uqa_core::memory::Produced;

macro_rules! function_fields {
    ($reader:ident($($arguments:tt)*)) => {
        $reader! { $($arguments)*; CreateFunction {
            object_id: default(None), catalog_revision: default(None), catalog_oid: default(None),
            name: required, or_replace: required, is_procedure: required, params: required,
            returns: required, return_type_reference: default(None), return_written_type: default(None),
            language: required, body: required, sql_body_form: default(None),
            creation_search_path: defaulted, volatility: required, strict: required,
            owner: custom(read_owner), parallel: defaulted, support: default(None),
            cost: custom(read_estimate), rows: custom(read_estimate), config: defaulted,
            config_actions: defaulted, attribute_clauses: defaulted,
            execute_acl: default(None)
        } }
    };
}
macro_rules! read_stored_fields {
    ($input:ident, $token:ident, $values:ident; $ty:ident { $($fields:tt)* }) => {
        fields!($input, $token, $values, $ty { $($fields)* }
            skip { body_origin: None, security: RoutineSecurityAttributes::default() } flatten(true))
    };
}
macro_rules! read_projected_fields {
    (; $ty:ident { $($field:ident: $mode:ident $(($($argument:tt)*))?),* $(,)? }) => {
        pub(in crate::ast::query_input) fn projected_function<'i, 'a>(
            input: crate::ast::query_input::projected::Input<'i>,
            mut authority: Option<Produced<crate::routines::security::binding::BoundRoutineAuthority>>,
            work: &'a mut crate::ast::query_input::projected::Work<'i>,
        ) -> crate::ast::query_input::projected::Result<crate::ast::query_input::projected::Read<'a, CreateFunction>> {
            use crate::ast::query_input::projected as p;
            p::frame(input.control, async move {
                let bound_owner = authority.as_ref().map(|value| value.owner);
                $(let mut $field = None;)*
                let mut flattened = p::FlatFields::new(p::compound::SECURITY_KEYS);
                const NAMES: &[&str] = &[$(stringify!($field)),*];
                let required = &[$(required!($mode $(($($argument)*))?)),*];
                let map = required.iter().enumerate().fold(0, |mask, (index, &(required, _))| {
                    mask | (u64::from(required) << index)
                });
                let mut cursor = p::Fields::new(input, NAMES, [map, 0], "struct CreateFunction", true, work).await?;
                if authority.is_some() {
                    cursor = cursor.with_native_fields(&["execute_acl", "owner"]);
                }
                while let Some((index, value)) = cursor.next_field(work).await? {
                    let Some(index) = index else {
                        flattened.observe(input.child(value.expect("stored flattened field")), cursor.name().expect("flattened map field"), work).await?;
                        continue;
                    };
                    match NAMES[index] {
                        $(stringify!($field) => {
                            $field = Some(read_function_field!(input, work, value, authority, bound_owner, $field, $mode $(($($argument)*))?)?);
                        },)*
                        _ => unreachable!("declared routine field"),
                    }
                }
                let security = p::compound::read_security(input, &flattened, work).await?;
                let mut parts = Parts::new(input.control);
                let result = CreateFunction {
                    $($field: projected_finish_field!(parts, $field, $mode $(($($argument)*))?),)*
                    body_origin: None,
                    security: parts.take(security),
                };
                p::finish(parts, result)
            })
        }

        impl crate::ast::query_input::projected::Projected for CreateFunction {
            fn read_value<'i, 'a>(
                input: crate::ast::query_input::projected::Input<'i>,
                work: &'a mut crate::ast::query_input::projected::Work<'i>,
            ) -> crate::ast::query_input::projected::Result<crate::ast::query_input::projected::Read<'a, Self>> {
                projected_function(input, None, work)
            }
        }
    };
}

macro_rules! read_function_field {
    ($input:ident, $work:ident, $value:ident, $authority:ident, $owner:ident, owner, $($mode:tt)*) => {
        if let Some(value) = $value {
            let child = $input.child(value);
            projected_read_field!(child, $work, $($mode)*)
        } else {
            $input.scalar($owner)
        }
    };
    ($input:ident, $work:ident, $value:ident, $authority:ident, $owner:ident, execute_acl, $($mode:tt)*) => {
        if let Some(value) = $value {
            let child = $input.child(value);
            projected_read_field!(child, $work, $($mode)*)
        } else {
            let (authority, memory) = $authority.take().expect("prepared native authority").into_parts();
            p::native($input.control.finish(authority.execute_acl, memory).map_err(uqa_core::json::JsonReadError::from)?, $input.control)
        }
    };
    ($input:ident, $work:ident, $value:ident, $authority:ident, $owner:ident, $field:ident, $($mode:tt)*) => {{
        let child = $input.child($value.expect("stored routine field"));
        projected_read_field!(child, $work, $($mode)*)
    }};
}
function_fields!(read_projected_fields());

impl Stored for CreateFunction {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Native<Self>, InputError> {
        let (text, depth) = input.encoded(token)?;
        let mut child = input.replay_at(text, depth);
        let token = child.next()?;
        let child_input = &mut child;
        let result = function_fields!(read_stored_fields(child_input, token, values))?;
        child.end()?;
        let mut child = Input::content_at_depth(text, input.control, depth);
        let token = child.next()?;
        let mut parts = Parts::new(input.control);
        let mut result = parts.take(result);
        result.security = parts.take(child.stored(token, values)?);
        child.end()?;
        parts.finish(result)
    }
}
