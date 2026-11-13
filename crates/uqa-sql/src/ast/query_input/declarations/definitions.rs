//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::special::{read_estimate, read_owner};
use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::ast::{CreateFunction, RoutineSecurityAttributes};

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
    (; $ty:ident { $($fields:tt)* }) => {
        projected_struct!($ty { $($fields)* } skip { body_origin: None }
            flatten(security, crate::ast::query_input::projected::compound::SECURITY_KEYS,
                crate::ast::query_input::projected::compound::read_security));
    };
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
