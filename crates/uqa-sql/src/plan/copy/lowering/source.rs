//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, QueryArena, Result, SyntaxCopy};
use crate::{
    ast::{FromClause, TableFunction},
    plan::{AggregateClassifier, JoinExecutionStrategy, SourcePlan, TableFunctionPlan},
};

impl Copying<'_> {
    pub(super) fn lower_source(
        &mut self,
        source: &FromClause,
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<SourcePlan> {
        self.field("source", |copying| {
            copying.lower_source_fields(source, aggregates, arena)
        })
    }
    #[expect(
        clippy::too_many_lines,
        reason = "exhaustive source copying admits every destination field"
    )]
    fn lower_source_fields(
        &mut self,
        source: &FromClause,
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<SourcePlan> {
        self.check()?;
        Ok(match source {
            FromClause::RetainedRows(rows) => SourcePlan::RetainedRows(rows.clone()),
            FromClause::Table {
                name,
                qualifier,
                alias,
                column_aliases,
                bound_columns,
                include_descendants,
            } => SourcePlan::Table {
                name: self.field("name", |copying| copying.text(name))?,
                executable_query: self.field("executable_query", |_| Ok(None))?,
                qualifier: self.field("qualifier", |copying| copying.text(qualifier))?,
                alias: self.optional_text(alias.as_deref())?,
                column_aliases: self.texts(column_aliases)?,
                bound_columns: bound_columns
                    .as_ref()
                    .map(|columns| self.texts(columns))
                    .transpose()?,
                include_descendants: *include_descendants,
            },
            FromClause::Join {
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
            } => SourcePlan::Join {
                comma: *comma,
                left: self.field("left", |copying| {
                    copying.boxed(|copying| copying.lower_source(left, aggregates, arena))
                })?,
                right: self.field("right", |copying| {
                    copying.boxed(|copying| copying.lower_source(right, aggregates, arena))
                })?,
                kind: *kind,
                on: self.field("on", |copying| {
                    copying.lower_optional_scalar(on.as_ref(), aggregates, arena)
                })?,
                using: using
                    .as_ref()
                    .map(|using| {
                        Ok::<_, crate::SQLError>(crate::ast::JoinUsing {
                            columns: self.texts(&using.columns)?,
                            alias: self.optional_text(using.alias.as_deref())?,
                        })
                    })
                    .transpose()?,
                natural: *natural,
                alias: self.optional_text(alias.as_deref())?,
                column_aliases: self.texts(column_aliases)?,
                lateral: *lateral,
                strategy: JoinExecutionStrategy::Auto,
            },
            FromClause::Values {
                lateral,
                rows,
                alias,
                column_aliases,
                internal_relation,
                internal_column_types,
                internal_column_collations,
            } => SourcePlan::Values {
                lateral: *lateral,
                rows: self.field("rows", |copying| {
                    copying.lower_rows(rows, aggregates, arena)
                })?,
                alias: self.optional_text(alias.as_deref())?,
                column_aliases: self.texts(column_aliases)?,
                internal_relation: *internal_relation,
                internal_column_types: self.map(internal_column_types.iter(), |copying, ty| {
                    ty.as_ref().map(|ty| copying.ty(ty)).transpose()
                })?,
                internal_column_collations: self
                    .map(internal_column_collations.iter(), |_, collation| {
                        Ok(*collation)
                    })?,
            },
            FromClause::Function {
                name,
                binding,
                output_name,
                relations,
                args,
                alias,
                column_aliases,
                ordinality,
                column_types,
            } => SourcePlan::Function {
                name: self.text(name)?,
                binding: self.field("binding", |copying| binding.copy_syntax(copying))?,
                output_name: self.field("output_name", |copying| copying.text(output_name))?,
                relations: self.field("relations", |copying| relations.copy_syntax(copying))?,
                args: self.field("args", |copying| {
                    copying.map(args.iter(), |copying, expression| {
                        copying.lower_scalar(expression, aggregates, arena)
                    })
                })?,
                alias: self.optional_text(alias.as_deref())?,
                column_aliases: self.texts(column_aliases)?,
                ordinality: *ordinality,
                column_types: self.texts(column_types)?,
            },
            FromClause::FunctionGroup {
                functions,
                alias,
                column_aliases,
                ordinality,
            } => SourcePlan::FunctionGroup {
                functions: self.field("functions", |copying| {
                    copying.map(functions.iter(), |copying, function| {
                        copying.lower_table_function(function, aggregates, arena)
                    })
                })?,
                alias: self.optional_text(alias.as_deref())?,
                column_aliases: self.texts(column_aliases)?,
                ordinality: *ordinality,
            },
            FromClause::Subquery {
                lateral,
                body,
                alias,
                column_aliases,
            } => SourcePlan::Subquery {
                captured_output: None,
                lateral: *lateral,
                body: self.field("body", |copying| {
                    copying.boxed(|copying| copying.lower_nested_query(body, aggregates))
                })?,
                alias: self.optional_text(alias.as_deref())?,
                column_aliases: self.texts(column_aliases)?,
            },
        })
    }

    fn lower_table_function(
        &mut self,
        function: &TableFunction,
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<TableFunctionPlan> {
        Ok(TableFunctionPlan {
            name: self.text(&function.name)?,
            binding: self.field("binding", |copying| function.binding.copy_syntax(copying))?,
            output_name: self
                .field("output_name", |copying| copying.text(&function.output_name))?,
            relations: self.field("relations", |copying| {
                function.relations.copy_syntax(copying)
            })?,
            args: self.field("args", |copying| {
                copying.map(function.args.iter(), |copying, expression| {
                    copying.lower_scalar(expression, aggregates, arena)
                })
            })?,
            column_aliases: self.texts(&function.column_aliases)?,
            column_types: self.texts(&function.column_types)?,
        })
    }
}
