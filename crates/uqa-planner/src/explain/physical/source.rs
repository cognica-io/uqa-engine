//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind physical retrieval leaves to the source that receives their predicate.

use super::{Collector, Scope};
use crate::filter_pushdown::push_output_filter_into_query_plan;
use uqa_sql::{
    ast::OperatorJoinRelations,
    plan::{source_projection::QualifierFilters, SourcePlan},
    semantics::{cte_reference_name, source_filters::qualifier_filter},
    SQLError, ScalarExpr,
};

impl Collector<'_> {
    pub(super) fn source(
        &mut self,
        source: &SourcePlan,
        filters: Option<&QualifierFilters>,
        scope: &Scope,
        path: &str,
    ) -> Result<(), SQLError> {
        match source {
            SourcePlan::Table { .. } => self.table_source(source, filters, scope, path),
            SourcePlan::Join { left, right, .. } => {
                self.source(left, filters, scope, &format!("{path}/Left"))?;
                self.source(right, filters, scope, &format!("{path}/Right"))
            }
            SourcePlan::Subquery { body, .. } => self.query(body, scope, &format!("{path}/Source")),
            SourcePlan::Function {
                relations, args, ..
            } => self.operator_join(relations.as_ref(), args, path),
            SourcePlan::FunctionGroup { functions, .. } => {
                for (index, function) in functions.iter().enumerate() {
                    self.operator_join(
                        function.relations.as_ref(),
                        &function.args,
                        &format!("{path}/Function {index}"),
                    )?;
                }
                Ok(())
            }
            SourcePlan::Values { .. } => Ok(()),
        }
    }

    fn table_source(
        &mut self,
        source: &SourcePlan,
        filters: Option<&QualifierFilters>,
        scope: &Scope,
        path: &str,
    ) -> Result<(), SQLError> {
        let SourcePlan::Table {
            name,
            qualifier,
            alias,
            column_aliases,
            bound_columns,
            include_descendants,
        } = source
        else {
            unreachable!()
        };
        let qualifier = alias.as_deref().unwrap_or(qualifier);
        if let Some(cte) = cte_reference_name(name).and_then(|name| scope.get(&name)) {
            if let Some(cte) = cte {
                if let Some(query) = cte.plan.body.query() {
                    self.query(query, &cte.outer, &format!("{path}/CTE {}", cte.plan.name))?;
                }
            }
            return Ok(());
        }
        let correlation = self.context.filters.correlation;
        let predicate = qualifier_filter(filters, qualifier);
        if let Some(view) = correlation
            .catalog
            .view_resolved(correlation.resolution, name)?
        {
            if !view.materialized {
                let specialized = if column_aliases.is_empty() {
                    predicate
                        .as_ref()
                        .map(|predicate| {
                            push_output_filter_into_query_plan(
                                self.context.filters,
                                &view.query,
                                qualifier,
                                predicate,
                                view.output_columns.as_deref(),
                            )
                        })
                        .transpose()?
                        .flatten()
                } else {
                    None
                };
                self.query(
                    specialized.as_ref().unwrap_or(&view.query),
                    &Scope::new(),
                    &format!("{path}/View {name}"),
                )?;
            }
            return Ok(());
        }
        (self.context.validate_source)(correlation.resolution, name)?;
        let Some(mut predicate) = predicate else {
            return Ok(());
        };
        let Some(table) = correlation
            .catalog
            .table_resolved(correlation.resolution, name)?
        else {
            return Ok(());
        };
        let columns = uqa_sql::semantics::bound_source_column_names(
            table
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect(),
            bound_columns.as_deref(),
        )?;
        uqa_sql::plan::rewrite_scalar_expression(&mut predicate, &mut |expression| {
            if let ScalarExpr::Column(column) | ScalarExpr::QualifiedColumn { column, .. } =
                expression
            {
                if let Some(position) = column_aliases
                    .iter()
                    .position(|alias| alias.eq_ignore_ascii_case(column))
                {
                    if let Some(physical) = columns.get(position) {
                        column.clone_from(physical);
                    }
                }
            }
        });
        let Some(canonical) = correlation
            .catalog
            .table_name_resolved(correlation.resolution, name)?
        else {
            return Ok(());
        };
        let tables = if *include_descendants {
            self.context.statistics.hierarchy_scan_tables(&canonical)?
        } else {
            vec![canonical]
        };
        for table in tables {
            self.vector_predicate(&table, qualifier, &predicate, path)?;
        }
        Ok(())
    }

    fn operator_join(
        &mut self,
        relations: Option<&OperatorJoinRelations>,
        args: &[ScalarExpr],
        path: &str,
    ) -> Result<(), SQLError> {
        if let Some(relations) = relations {
            for (side, relation, predicate) in [
                ("Left", &relations.left, args.first()),
                ("Right", &relations.right, args.get(1)),
            ] {
                if let Some(predicate) = predicate {
                    self.vector_predicate(
                        relation,
                        relation,
                        predicate,
                        &format!("{path}/{side} operand"),
                    )?;
                }
            }
        }
        Ok(())
    }
}
