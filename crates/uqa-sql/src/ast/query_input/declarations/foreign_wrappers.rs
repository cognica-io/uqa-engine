//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::ast::foreign_wrappers::{CreateForeignWrapper, ForeignWrapperFunctionOption};

stored_struct!(CreateForeignWrapper {
    name: required,
    functions: required,
    options: required,
});

stored_enum!(ForeignWrapperFunctionOption {
    Handler (value),
    Validator (value),
});
