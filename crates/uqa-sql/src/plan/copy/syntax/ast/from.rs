//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_enum!(crate::ast::FromClause {
    RetainedRows(value0),
    Table { name, qualifier, alias, column_aliases, bound_columns, include_descendants },
    Join { comma, left, right, kind, on, using, natural, alias, column_aliases, lateral },
    Values { lateral, rows, alias, column_aliases, internal_relation, internal_column_types, internal_column_collations },
    Function { name, binding, output_name, relations, args, alias, column_aliases, ordinality, column_types },
    FunctionGroup { functions, alias, column_aliases, ordinality },
    Subquery { lateral, body, alias, column_aliases }
});

syntax_enum!(crate::ast::JoinKind {
    Inner,
    Left,
    Right,
    Full,
    Cross
});

syntax_struct!(crate::ast::JoinUsing { columns, alias });

syntax_struct!(crate::ast::OperatorJoinRelations { left, right });

syntax_struct!(crate::ast::TableFunction {
    name,
    binding,
    output_name,
    relations,
    args,
    column_aliases,
    column_types
});
