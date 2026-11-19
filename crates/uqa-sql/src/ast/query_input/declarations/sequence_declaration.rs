//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::ast::sequence_declaration::{SequenceDeclaration, SequenceOptionValue};

stored_struct!(SequenceDeclaration {
    data_type: optional,
    increment: optional,
    cycle: optional,
    max_value: optional,
    min_value: optional,
    start: optional,
    restart: optional,
    cache: optional,
    owned_by: optional,
});

stored_enum!(SequenceOptionValue {
    Absent,
    Integer (value),
    Text (value),
});
