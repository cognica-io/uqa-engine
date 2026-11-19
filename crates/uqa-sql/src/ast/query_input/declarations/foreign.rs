//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::ast::{relation_lifecycle::AlterForeignTableStmtSerde, AlterForeignTableStmt};

stored_struct!(AlterForeignTableStmtSerde {
    name: required,
    if_exists: required,
    owner: default(None),
    rename_to: default(None),
    set_schema: default(None),
    drop_columns: default(None),
    drop_constraints: default(None),
});

impl Stored for AlterForeignTableStmt {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Native<Self>, InputError> {
        let mut parts = Parts::new(input.control);
        let stored: AlterForeignTableStmtSerde = parts.take(input.stored(token, values)?);
        parts.finish(Self::try_from(stored).map_err(|_| invalid())?)
    }
}
