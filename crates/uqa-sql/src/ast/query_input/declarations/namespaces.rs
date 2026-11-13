//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::ast::namespaces::SchemaAuthorization;

stored_enum!(SchemaAuthorization {
    Role (value),
    CurrentUser,
    SessionUser,
});
