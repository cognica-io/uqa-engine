//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Input, InputError, JsonToken, Native, Stored, Values};
use crate::ast::{ObjectAclEntry, RoleAttribute};
use uqa_core::{
    catalog_identity::CatalogObjectIdentity,
    catalog_role::RoleIdentity,
    memory::{OwnedMap, OwnedSet},
    Value,
};

macro_rules! scalar {
    ($ty:ty, $input:ident, $token:ident, $value:expr) => {
        impl Stored for $ty {
            fn read<'a>(
                $input: &mut Input<'a, '_>,
                $token: JsonToken<'a>,
                _: &Values<'_>,
            ) -> Result<Native<Self>, InputError> {
                let value = $value?;
                Native::new($input.scalar(value)?, $input.control)
            }
        }
    };
}
scalar!(u8, input, token, crate::ast::stored_input::number(token));
scalar!(i32, input, token, crate::ast::stored_input::number(token));
scalar!(i64, input, token, crate::ast::stored_input::number(token));
scalar!([u8; 16], input, token, input.identity(token));
scalar!(
    RoleIdentity,
    input,
    token,
    input.stored_role_identity(token)
);
scalar!(
    CatalogObjectIdentity,
    input,
    token,
    input.catalog_identity(token)
);

impl Stored for ObjectAclEntry {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        _: &Values<'_>,
    ) -> Result<Native<Self>, InputError> {
        Native::new(input.stored_object_acl(token)?, input.control)
    }
}

impl Stored for Value {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Native<Self>, InputError> {
        Native::new(values.value(input, token)?, input.control)
    }
}

impl Stored for OwnedSet<RoleAttribute> {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Native<Self>, InputError> {
        let result = input.set(token, |input, token| {
            Ok(input.stored::<RoleAttribute>(token, values)?.into_parts().0)
        })?;
        Native::new(result, input.control)
    }
}

impl Stored for OwnedMap<RoleAttribute, bool> {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        _: &Values<'_>,
    ) -> Result<Native<Self>, InputError> {
        let mut result = Self::new();
        let mut cursor = input.object_cursor(token)?;
        while let Some((key, token)) = cursor.next(input)? {
            let key = crate::ast::stored_input::unit::<RoleAttribute>(&key)?;
            let value = crate::ast::stored_input::boolean(token)?;
            result.insert_budgeted(
                key,
                value,
                input.control.budget().expect("native input allowance"),
            )?;
        }
        Native::new(
            input
                .control
                .finish(result, input.control.empty_reservation())?,
            input.control,
        )
    }
}
