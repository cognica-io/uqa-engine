//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::ast::indexes::{IndexKey, RenameIndexStmt};

impl Stored for IndexKey {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Native<Self>, InputError> {
        let mut child = input.content_child(token)?;
        let token = child.next()?;
        let mut parts = Parts::new(input.control);
        let value = if matches!(token, JsonToken::String(_)) {
            Self::Column(parts.take(child.stored(token, values)?))
        } else {
            Self::Expression(parts.take(child.stored(token, values)?))
        };
        child.end()?;
        parts.finish(value)
    }
}

stored_struct!(RenameIndexStmt {
    name: required,
    new_name: required,
    if_exists: required,
});
