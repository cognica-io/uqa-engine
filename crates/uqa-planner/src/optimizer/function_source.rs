//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Function range-table entries share scalar planning and SQL body expansion.

use super::{AggregateClassifier, OptimizerConfig, SourcePlan};
use uqa_sql::{SQLError, ScalarExpr};

pub(super) fn optimize(
    source: &mut SourcePlan,
    config: &OptimizerConfig<'_>,
    aggregates: &dyn AggregateClassifier,
) -> Result<(), SQLError> {
    let Some(context) = config.routine_inlining.as_ref() else {
        return Ok(());
    };
    let SourcePlan::Function {
        binding: Some(binding),
        name,
        output_name,
        args,
        alias,
        column_aliases,
        column_types,
        ordinality,
        relations: None,
    } = source
    else {
        return Ok(());
    };
    if !*ordinality {
        if let Some(mut inlined) = context.prepare_table(
            binding,
            args,
            &config.active_inline_routines,
            column_aliases,
            column_types,
        )? {
            ensure_unit_source(&mut inlined.query);
            let mut nested = config.clone();
            nested.active_inline_routines.push(inlined.identity);
            super::traversal::optimize_query(&mut inlined.query, &nested, aggregates)?;
            let columns = uqa_sql::semantics::apply_table_function_aliases(
                inlined
                    .declared_columns
                    .unwrap_or_else(|| vec![alias.as_deref().unwrap_or(output_name).to_string()]),
                column_aliases,
                false,
            );
            *source = SourcePlan::Subquery {
                captured_output: None,
                lateral: true,
                body: inlined.query,
                alias: Some(alias.as_deref().unwrap_or(output_name).to_string()),
                column_aliases: columns,
            };
            return Ok(());
        }
    }
    if !column_types.is_empty() {
        return Ok(());
    }
    let Some(columns) = context.scalar_from_columns(
        binding,
        args,
        output_name,
        alias.as_deref(),
        column_aliases,
        *ordinality,
    )?
    else {
        return Ok(());
    };
    let mut expression = ScalarExpr::Func {
        name: name.clone(),
        binding: Some(binding.clone()),
        args: args.clone(),
        distinct: false,
        order_by: Vec::new(),
        filter: None,
        order_syntax: uqa_sql::ast::FunctionCallSyntax::Ordinary,
    };
    let original = expression.clone();
    super::optimize_scalar_slot(&mut expression, config)?;
    if expression == original {
        return Ok(());
    }
    *source = SourcePlan::Subquery {
        captured_output: None,
        lateral: true,
        body: Box::new(scalar_query(expression, &columns, *ordinality)),
        alias: Some(alias.as_deref().unwrap_or(output_name).to_string()),
        column_aliases: columns,
    };
    Ok(())
}

pub(super) fn optimize_group(
    source: &mut SourcePlan,
    config: &OptimizerConfig<'_>,
    aggregates: &dyn AggregateClassifier,
) -> Result<(), SQLError> {
    let SourcePlan::FunctionGroup {
        functions,
        alias,
        column_aliases,
        ordinality: false,
    } = source
    else {
        return Ok(());
    };
    let [function] = functions.as_slice() else {
        return Ok(());
    };
    let mut member = SourcePlan::Function {
        name: function.name.clone(),
        binding: function.binding.clone(),
        output_name: function.output_name.clone(),
        relations: function.relations.clone(),
        args: function.args.clone(),
        alias: alias.clone(),
        column_aliases: function.column_aliases.clone(),
        column_types: function.column_types.clone(),
        ordinality: false,
    };
    optimize(&mut member, config, aggregates)?;
    if let SourcePlan::Subquery {
        column_aliases: columns,
        ..
    } = &mut member
    {
        *columns = uqa_sql::semantics::apply_table_function_aliases(
            columns.clone(),
            column_aliases,
            false,
        );
        *source = member;
    }
    Ok(())
}

fn scalar_query(
    expression: ScalarExpr,
    columns: &[String],
    ordinality: bool,
) -> uqa_sql::plan::QueryPlan {
    let mut row = vec![expression];
    if ordinality {
        row.push(ScalarExpr::TypedLiteral {
            value: uqa_core::Value::Int(1),
            ty: "bigint".into(),
            bound_type: Some(uqa_sql::ColumnType::BigInteger),
            collation_oid: None,
            composite_source: None,
            parameter_index: None,
        });
    }
    let mut query = uqa_sql::plan::QueryPlan {
        analyzed_output: None,
        analyzed_source: None,
        analyzed_grouping: None,
        relations_bound: false,
        ctes: Vec::new(),
        root: uqa_sql::plan::RelationalPlan::QueryBlock(Box::new(uqa_sql::plan::QueryBlockPlan {
            windows: Vec::new(),
            projections: row
                .into_iter()
                .zip(columns)
                .map(|(expr, name)| uqa_sql::plan::ProjectionPlan {
                    expr,
                    alias: Some(name.clone()),
                })
                .collect(),
            privilege_columns: std::collections::BTreeSet::new(),
            from: None,
            r#where: None,
            compute: uqa_sql::plan::ComputePlan::Project,
            group_by: Vec::new(),
            grouping_sets: Vec::new(),
            group_distinct: false,
            having: None,
            order_by: Vec::new(),
            limit: None,
            with_ties: false,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
            subqueries: Vec::new(),
            access: uqa_sql::plan::AccessPathPlan::Row,
            locking: Vec::new(),
        })),
    };
    ensure_unit_source(&mut query);
    query
}

/// Projection over the unit relation preserves one scalar result while deferring its evaluation until a row is requested.
fn ensure_unit_source(query: &mut uqa_sql::plan::QueryPlan) {
    if let uqa_sql::plan::RelationalPlan::QueryBlock(block) = &mut query.root {
        block.from.get_or_insert_with(|| SourcePlan::Values {
            lateral: false,
            rows: vec![Vec::new()],
            alias: None,
            column_aliases: Vec::new(),
            internal_relation: None,
            internal_column_types: Vec::new(),
            internal_column_collations: Vec::new(),
        });
    }
}
