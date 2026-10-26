//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result};
use crate::ast::{FunctionBinding, OperatorJoinRelations};
use crate::plan::{SourcePlan, TableFunctionPlan};

impl Copying<'_> {
    #[expect(
        clippy::too_many_lines,
        reason = "exhaustive source copying admits every destination field"
    )]
    pub(super) fn source(&mut self, source: &SourcePlan) -> Result<SourcePlan> {
        self.check()?;
        Ok(match source {
            SourcePlan::RetainedRows(rows) => SourcePlan::RetainedRows(rows.clone()),
            SourcePlan::Table {
                name,
                executable_query,
                qualifier,
                alias,
                column_aliases,
                bound_columns,
                include_descendants,
            } => SourcePlan::Table {
                name: self.text(name)?,
                executable_query: executable_query
                    .as_ref()
                    .map(|query| self.boxed(|copying| copying.query(query)))
                    .transpose()?,
                qualifier: self.text(qualifier)?,
                alias: self.optional_text(alias.as_deref())?,
                column_aliases: self.texts(column_aliases)?,
                bound_columns: bound_columns
                    .as_ref()
                    .map(|columns| self.texts(columns))
                    .transpose()?,
                include_descendants: *include_descendants,
            },
            SourcePlan::Join {
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
            } => SourcePlan::Join {
                comma: *comma,
                left: self.boxed(|copying| copying.source(left))?,
                right: self.boxed(|copying| copying.source(right))?,
                kind: *kind,
                on: self.optional_scalar(on.as_ref())?,
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
                strategy: *strategy,
            },
            SourcePlan::Values {
                lateral,
                rows,
                alias,
                column_aliases,
                internal_relation,
                internal_column_types,
                internal_column_collations,
            } => SourcePlan::Values {
                lateral: *lateral,
                rows: self.rows(rows)?,
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
            SourcePlan::Function {
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
                binding: self.function_binding(binding.as_ref())?,
                output_name: self.text(output_name)?,
                relations: self.join_relations(relations.as_ref())?,
                args: self.map(args.iter(), Self::scalar)?,
                alias: self.optional_text(alias.as_deref())?,
                column_aliases: self.texts(column_aliases)?,
                ordinality: *ordinality,
                column_types: self.texts(column_types)?,
            },
            SourcePlan::FunctionGroup {
                functions,
                alias,
                column_aliases,
                ordinality,
            } => SourcePlan::FunctionGroup {
                functions: self.map(functions.iter(), Self::table_function)?,
                alias: self.optional_text(alias.as_deref())?,
                column_aliases: self.texts(column_aliases)?,
                ordinality: *ordinality,
            },
            SourcePlan::Subquery {
                captured_output,
                lateral,
                body,
                alias,
                column_aliases,
            } => SourcePlan::Subquery {
                captured_output: *captured_output,
                lateral: *lateral,
                body: self.boxed(|copying| copying.query(body))?,
                alias: self.optional_text(alias.as_deref())?,
                column_aliases: self.texts(column_aliases)?,
            },
        })
    }

    fn function_binding(
        &mut self,
        binding: Option<&FunctionBinding>,
    ) -> Result<Option<FunctionBinding>> {
        binding
            .map(|binding| {
                let output = binding.clone_with_control(&self.control)?;
                Ok(self.take(output))
            })
            .transpose()
    }

    fn join_relations(
        &mut self,
        relations: Option<&OperatorJoinRelations>,
    ) -> Result<Option<OperatorJoinRelations>> {
        relations
            .map(|relations| {
                Ok(OperatorJoinRelations {
                    left: self.text(&relations.left)?,
                    right: self.text(&relations.right)?,
                })
            })
            .transpose()
    }

    fn table_function(&mut self, function: &TableFunctionPlan) -> Result<TableFunctionPlan> {
        Ok(TableFunctionPlan {
            name: self.text(&function.name)?,
            binding: self.function_binding(function.binding.as_ref())?,
            output_name: self.text(&function.output_name)?,
            relations: self.join_relations(function.relations.as_ref())?,
            args: self.map(function.args.iter(), Self::scalar)?,
            column_aliases: self.texts(&function.column_aliases)?,
            column_types: self.texts(&function.column_types)?,
        })
    }
}
