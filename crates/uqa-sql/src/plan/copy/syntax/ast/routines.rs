//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_struct!(crate::ast::CreateFunction {
    object_id,
    catalog_revision,
    catalog_oid,
    name,
    or_replace,
    is_procedure,
    params,
    returns,
    return_type_reference,
    return_written_type,
    language,
    body,
    body_origin,
    sql_body_form,
    creation_search_path,
    volatility,
    strict,
    owner,
    security,
    parallel,
    support,
    cost,
    rows,
    config,
    config_actions,
    attribute_clauses,
    execute_acl
});

syntax_struct!(crate::ast::DropFunctionItem { name, arg_types });

syntax_struct!(crate::ast::DropFunctionStmt {
    is_procedure,
    if_exists,
    cascade,
    items
});

syntax_enum!(crate::ast::FunctionBody {
    Source(value0),
    Statements(value0)
});

syntax_struct!(crate::ast::FunctionParam {
    name,
    type_name,
    type_reference,
    written_type,
    mode,
    default,
    default_type
});

syntax_enum!(crate::ast::FunctionParamMode {
    In,
    Out,
    InOut,
    Variadic,
    Table
});

syntax_enum!(crate::ast::FunctionReturns {
    None,
    Scalar { type_name },
    SetOf { type_name },
    Table
});

syntax_enum!(crate::ast::FunctionVolatility {
    Immutable,
    Stable,
    Volatile
});

syntax_struct!(crate::ast::RoutineColumnTypeReference {
    schema,
    relation,
    column
});

syntax_enum!(crate::ast::RoutineDefaultType {
    Concrete(value0),
    Polymorphic(value0)
});

syntax_enum!(crate::ast::SQLBodyForm { Return, Atomic });
