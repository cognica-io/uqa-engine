//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, variant, Node, Part, SQLError};
use crate::plan::SourcePlan;

impl Node for SourcePlan {
    #[expect(
        clippy::too_many_lines,
        reason = "exhaustive native source fields and stored projection"
    )]
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
        Ok(match self {
            Self::RetainedRows(_) => return Err(invalid("execution-scoped stored query source")),
            Self::Table {
                name,
                qualifier,
                alias,
                column_aliases,
                bound_columns,
                include_descendants,
                executable_query: _,
            } => variant(
                position,
                0,
                &[
                    name,
                    qualifier,
                    alias,
                    column_aliases,
                    bound_columns,
                    include_descendants,
                ],
            ),
            Self::Join {
                comma,
                left,
                right,
                kind,
                on,
                using,
                natural,
                alias,
                column_aliases,
                lateral,
                strategy,
            } => variant(
                position,
                1,
                &[
                    comma,
                    left,
                    right,
                    kind,
                    on,
                    using,
                    natural,
                    alias,
                    column_aliases,
                    lateral,
                    strategy,
                ],
            ),
            Self::Values {
                lateral,
                rows,
                alias,
                column_aliases,
                internal_relation,
                internal_column_types,
                internal_column_collations,
            } => variant(
                position,
                2,
                &[
                    lateral,
                    rows,
                    alias,
                    column_aliases,
                    internal_relation,
                    internal_column_types,
                    internal_column_collations,
                ],
            ),
            Self::Function {
                name,
                binding,
                output_name,
                relations,
                args,
                alias,
                column_aliases,
                ordinality,
                column_types,
            } => variant(
                position,
                3,
                &[
                    name,
                    binding,
                    output_name,
                    relations,
                    args,
                    alias,
                    column_aliases,
                    ordinality,
                    column_types,
                ],
            ),
            Self::FunctionGroup {
                functions,
                alias,
                column_aliases,
                ordinality,
            } => variant(position, 4, &[functions, alias, column_aliases, ordinality]),
            Self::Subquery {
                lateral,
                body,
                alias,
                column_aliases,
                captured_output: _,
            } => variant(position, 5, &[lateral, body, alias, column_aliases]),
        })
    }
}
