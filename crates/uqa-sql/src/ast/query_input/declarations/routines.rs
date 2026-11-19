//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::ast::routines::{
    DropFunctionItem, DropFunctionStmt, FunctionBody, FunctionParam, FunctionParamMode,
    FunctionReturns, FunctionVolatility, RoutineColumnTypeReference, RoutineDefaultType,
    SQLBodyForm,
};

stored_struct!(DropFunctionItem {
    name: required,
    arg_types: optional,
});

stored_struct!(DropFunctionStmt {
    is_procedure: required,
    if_exists: required,
    cascade: defaulted,
    items: required,
});

stored_enum!(FunctionBody {
    Source (value),
    Statements (value),
});

stored_struct!(FunctionParam {
    name: required,
    type_name: required,
    type_reference: defaulted,
    written_type: defaulted,
    mode: required,
    default: defaulted,
    default_type: defaulted,
});

stored_enum!(FunctionParamMode {
    In,
    Out,
    InOut,
    Variadic,
    Table,
});

stored_enum!(FunctionReturns {
    None,
    Scalar {
        type_name: required,
    },
    SetOf {
        type_name: required,
    },
    Table,
});

stored_enum!(FunctionVolatility {
    Immutable,
    Stable,
    Volatile,
});

stored_struct!(RoutineColumnTypeReference {
    schema: optional,
    relation: required,
    column: required,
});

stored_enum!(RoutineDefaultType {
    Concrete (value),
    Polymorphic (value),
});

stored_enum!(SQLBodyForm { Return, Atomic });
