//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Body, Empty, Field, Input, PlanMemory, SQLError, Step};
use crate::ast::FromClause;

syntax_enum!(FromClause, FromClause::Values {
    lateral: false, rows: Vec::new(), alias: None, column_aliases: Vec::new(),
    internal_relation: None, internal_column_types: Vec::new(), internal_column_collations: Vec::new(),
};
    0 => Table { name, qualifier, alias, column_aliases, bound_columns, include_descendants },
    1 => Join { comma, left, right, kind, on, using, natural, alias, column_aliases, lateral },
    2 => Values { lateral, rows, alias, column_aliases, internal_relation, internal_column_types, internal_column_collations },
    3 => Function { name, binding, output_name, relations, args, alias, column_aliases, ordinality, column_types },
    4 => FunctionGroup { functions, alias, column_aliases, ordinality },
    5 => Subquery { lateral, body, alias, column_aliases },
    reject { Self::RetainedRows(_) }
);
