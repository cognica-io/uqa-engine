//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rename creation-bound view column references without changing public row types.

use uqa_planner::{QueryBlockPlan, QueryPlan, RelationalPlan, ScalarExpr, SourcePlan};
use uqa_sql::ast::BinaryOp;

use super::{
    expression_name, query_columns, Column, Deparser, RelationLookupMode, SQLError, Scope,
};

pub(crate) fn rename_view_column_query(
    engine: &crate::Engine,
    query: &mut QueryPlan,
    table: &str,
    from: &str,
    to: &str,
) -> Result<(), SQLError> {
    let catalog = engine.catalog_read_view();
    let dynamic = engine.session_execution_view().relation_name_resolution();
    let mut bound = dynamic.clone();
    bound.set_lookup_mode(RelationLookupMode::Bound);
    let deparser = Deparser {
        catalog: &catalog,
        dynamic,
        bound,
        pretty: false,
        wrap: 0,
    };
    let rename = Rename { table, from, to };
    deparser.rename_query(query, &Scope::default(), &rename)
}

struct Rename<'a> {
    table: &'a str,
    from: &'a str,
    to: &'a str,
}

impl Deparser<'_> {
    fn rename_query(
        &self,
        query: &mut QueryPlan,
        parent: &Scope,
        rename: &Rename<'_>,
    ) -> Result<(), SQLError> {
        let mut scope = parent.clone();
        for cte in &query.ctes {
            let mut names = query_columns(&cte.query);
            for (name, alias) in names.iter_mut().zip(&cte.columns) {
                name.clone_from(alias);
            }
            scope.ctes.insert(cte.name.clone(), names);
        }
        for cte in &mut query.ctes {
            self.rename_query(&mut cte.query, &scope.child(), rename)?;
        }
        match &mut query.root {
            RelationalPlan::QueryBlock(block) => self.rename_block(block, &scope, rename)?,
            RelationalPlan::SetOp {
                left,
                right,
                subqueries,
                ..
            } => {
                self.rename_query(left, &scope, rename)?;
                self.rename_query(right, &scope, rename)?;
                for subquery in subqueries {
                    self.rename_query(subquery, &scope.child(), rename)?;
                }
            }
            RelationalPlan::Values { rows, subqueries } => {
                for row in rows {
                    for expression in row {
                        rename_expression(expression, &scope, rename);
                    }
                }
                for subquery in subqueries {
                    self.rename_query(subquery, &scope.child(), rename)?;
                }
            }
        }
        Ok(())
    }

    fn rename_block(
        &self,
        block: &mut QueryBlockPlan,
        parent: &Scope,
        rename: &Rename<'_>,
    ) -> Result<(), SQLError> {
        let mut scope = parent.clone();
        scope.columns = block
            .from
            .as_ref()
            .map(|source| self.source_columns(source, parent))
            .transpose()?
            .unwrap_or_default();
        for projection in &mut block.projections {
            let old_name = expression_name(&projection.expr);
            rename_expression(&mut projection.expr, &scope, rename);
            if projection.alias.is_none() && expression_name(&projection.expr) != old_name {
                projection.alias = Some(old_name);
            }
        }
        for expression in block
            .r#where
            .iter_mut()
            .chain(&mut block.having)
            .chain(&mut block.limit)
            .chain(&mut block.offset)
            .chain(&mut block.group_by)
            .chain(&mut block.distinct_on)
            .chain(block.grouping_sets.iter_mut().flatten())
        {
            rename_expression(expression, &scope, rename);
        }
        for order in &mut block.order_by {
            let is_output = matches!(&order.expr, ScalarExpr::Column(name) if block.projections.iter().any(|projection| projection.alias.as_ref() == Some(name)));
            if !is_output {
                rename_expression(&mut order.expr, &scope, rename);
            }
        }
        if let Some(source) = &mut block.from {
            self.rename_source(source, &scope, rename)?;
        }
        for subquery in &mut block.subqueries {
            self.rename_query(subquery, &scope.child(), rename)?;
        }
        Ok(())
    }

    fn rename_source(
        &self,
        source: &mut SourcePlan,
        scope: &Scope,
        rename: &Rename<'_>,
    ) -> Result<(), SQLError> {
        if matches!(source, SourcePlan::Join { alias: Some(_), .. }) {
            self.preserve_join_columns(source, scope, rename)?;
        }
        match source {
            SourcePlan::Table { .. } => {}
            SourcePlan::Subquery { body, .. } => self.rename_query(body, &scope.child(), rename)?,
            SourcePlan::Values { rows, .. } => {
                for row in rows {
                    for expression in row {
                        rename_expression(expression, scope, rename);
                    }
                }
            }
            SourcePlan::Function { args, .. } => {
                for expression in args {
                    rename_expression(expression, scope, rename);
                }
            }
            SourcePlan::FunctionGroup { functions, .. } => {
                for function in functions {
                    for expression in &mut function.args {
                        rename_expression(expression, scope, rename);
                    }
                }
            }
            SourcePlan::Join {
                left,
                right,
                on,
                using,
                natural,
                ..
            } => {
                let left_columns = self.source_columns(left, scope)?;
                let right_columns = self.source_columns(right, scope)?;
                let names = using.as_ref().map_or_else(
                    || {
                        if *natural {
                            left_columns
                                .iter()
                                .filter(|column| {
                                    right_columns.iter().any(|other| other.name == column.name)
                                })
                                .map(|column| column.name.clone())
                                .collect()
                        } else {
                            Vec::new()
                        }
                    },
                    |using| using.columns.clone(),
                );
                let affected = left_columns.iter().chain(&right_columns).any(|column| {
                    column.relation.as_deref() == Some(rename.table)
                        && column.name == rename.from
                        && names.contains(&column.name)
                });
                if affected {
                    let predicates = names
                        .iter()
                        .map(|name| {
                            let left = left_columns
                                .iter()
                                .find(|column| column.name == *name)
                                .expect("bound USING column exists on left");
                            let right = right_columns
                                .iter()
                                .find(|column| column.name == *name)
                                .expect("bound USING column exists on right");
                            ScalarExpr::Binary {
                                op: BinaryOp::Equal,
                                lhs: Box::new(renamed_column(left, rename)),
                                rhs: Box::new(renamed_column(right, rename)),
                            }
                        })
                        .collect::<Vec<_>>();
                    *on = Some(if predicates.len() == 1 {
                        predicates.into_iter().next().expect("one predicate")
                    } else {
                        ScalarExpr::And(predicates)
                    });
                    *using = None;
                    *natural = false;
                }
                if let Some(on) = on {
                    rename_expression(on, scope, rename);
                }
                self.rename_source(left, scope, rename)?;
                self.rename_source(right, scope, rename)?;
            }
        }
        Ok(())
    }

    /// Preserve an aliased join's row type and USING merge semantics while its underlying relation changes column names.
    fn preserve_join_columns(
        &self,
        source: &mut SourcePlan,
        scope: &Scope,
        rename: &Rename<'_>,
    ) -> Result<(), SQLError> {
        match source {
            SourcePlan::Table {
                name,
                qualifier,
                alias,
                column_aliases,
                ..
            } if name == rename.table => {
                let mut names = self.table_columns(name, scope)?;
                let needs_aliases = names
                    .iter()
                    .position(|name| name == rename.from)
                    .is_some_and(|index| index >= column_aliases.len());
                if needs_aliases {
                    for (name, alias) in names.iter_mut().zip(column_aliases.iter()) {
                        name.clone_from(alias);
                    }
                    *column_aliases = names;
                    alias.get_or_insert_with(|| qualifier.clone());
                }
            }
            SourcePlan::Join { left, right, .. } => {
                self.preserve_join_columns(left, scope, rename)?;
                self.preserve_join_columns(right, scope, rename)?;
            }
            _ => {}
        }
        Ok(())
    }
}

fn renamed_column(column: &Column, rename: &Rename<'_>) -> ScalarExpr {
    let name = if column.relation.as_deref() == Some(rename.table) && column.name == rename.from {
        rename.to
    } else {
        &column.name
    };
    ScalarExpr::qualified_column(&column.qualifier, name)
}

fn rename_expression(expression: &mut ScalarExpr, scope: &Scope, rename: &Rename<'_>) {
    uqa_planner::rewrite_scalar_expression(expression, &mut |expression| {
        let (name, qualifier) = match expression {
            ScalarExpr::Column(name) => (name.as_str(), None),
            ScalarExpr::QualifiedColumn { qualifier, column } => {
                (column.as_str(), Some(qualifier.as_str()))
            }
            _ => return,
        };
        let Some(column) = scope.columns.iter().chain(&scope.outer).find(|column| {
            column.name == name && qualifier.is_none_or(|qualifier| qualifier == column.qualifier)
        }) else {
            return;
        };
        if qualifier.is_none() {
            if let Some(merged) = &column.merged_expression {
                let mut merged = merged.clone();
                rename_expression(&mut merged, scope, rename);
                *expression = merged;
                return;
            }
        }
        *expression = renamed_column(column, rename);
    });
}
