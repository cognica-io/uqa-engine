//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::ast::identity_sequence::{
    DeferredSQLError, IdentitySequenceDeclaration, IdentitySequenceName,
};

stored_struct!(DeferredSQLError {
    sqlstate: required,
    message: required,
    detail: defaulted,
    hint: defaulted,
});

stored_struct!(IdentitySequenceDeclaration {
    name: optional,
    persistence: optional,
    sequence: required,
    error: optional,
});

stored_struct!(IdentitySequenceName {
    schema: optional,
    name: required,
});
