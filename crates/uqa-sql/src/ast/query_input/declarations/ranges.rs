//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::ast::{CreateRange, RangeOption, RangeOptionValue};

stored_struct!(CreateRange {
    name: required,
    options: required,
});
stored_struct!(RangeOption {
    name: required,
    value: required,
    position: optional,
});
stored_enum!(RangeOptionValue { Type (value), Name (value), Invalid, });
