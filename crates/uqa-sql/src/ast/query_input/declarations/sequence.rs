//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::ast::sequence::{AutoIncrement, AutoIncrementKind, AutoIncrementOwner};

stored_struct!(AutoIncrement {
    kind: required,
    sequence: defaulted,
    owner: defaulted,
    declaration: defaulted,
});

units!(AutoIncrementKind);

stored_struct!(AutoIncrementOwner {
    table: required,
    column: required,
});
