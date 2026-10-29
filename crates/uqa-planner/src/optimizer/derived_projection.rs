//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Eliminate unused scalar outputs of a simple derived source before constant evaluation.

use crate::{AggregateClassifier, QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan};
use uqa_core::{
    memory::{ProductionControl, ProductionMap, ProductionVec},
    Value,
};
use uqa_sql::{ast::FunctionVolatility, RowSchema, SQLError, ScalarExpr};

/// Catalog and lexical inputs for removing an unobserved output without losing its effects.
#[derive(Clone, Copy)]
pub struct OutputPruningContext<'a> {
    pub production: ProductionControl<'a>,
    pub aggregates: &'a dyn AggregateClassifier,
    pub routines: Option<&'a dyn uqa_sql::routines::RoutineResolution>,
    pub volatility: Option<&'a dyn uqa_sql::semantics::volatility::VolatilityCatalog>,
    pub visible_ctes: &'a dyn Fn(&str) -> bool,
}

impl<'a> OutputPruningContext<'a> {
    pub fn new(aggregates: &'a dyn AggregateClassifier) -> Self {
        Self {
            production: ProductionControl::uncontrolled(),
            aggregates,
            routines: None,
            volatility: None,
            visible_ctes: &|_| false,
        }
    }
}

pub(super) fn prune(
    block: &mut QueryBlockPlan,
    context: OutputPruningContext<'_>,
) -> Result<(), SQLError> {
    let Some(SourcePlan::Subquery {
        body,
        alias,
        column_aliases,
        ..
    }) = &block.from
    else {
        return Ok(());
    };
    if !block.subqueries.is_empty() {
        return Ok(());
    }
    let control = &context.production;
    let mut required = ProductionMap::new(*control);
    let mut all = false;
    block.try_visit_expressions(&mut |expression| {
        control.check()?;
        super::output_demand::try_visit(expression, control, &mut |part| {
            match part {
                ScalarExpr::Column(name) => {
                    if alias.as_deref() == Some(name.as_str())
                        && !has_named_output(body, column_aliases, name.as_str(), control)?
                    {
                        all = true;
                    } else {
                        required.insert_copy(name.as_str(), ())?;
                    }
                }
                ScalarExpr::QualifiedColumn { qualifier, column }
                    if alias.as_deref() == Some(qualifier.as_str()) =>
                {
                    required.insert_copy(column.as_str(), ())?;
                }
                ScalarExpr::QualifiedStar(qualifier)
                    if alias.as_deref() == Some(qualifier.as_str()) =>
                {
                    all = true;
                }
                ScalarExpr::Star | ScalarExpr::Position(_) => all = true,
                _ => {}
            }
            Ok::<_, SQLError>(())
        })?;
        Ok::<_, SQLError>(true)
    })?;
    if all {
        return Ok(());
    }
    let demanded = output_demand(
        body,
        column_aliases,
        &|name| required.get(name).is_some(),
        None,
        *control,
    )?;
    drop(required);
    let Some(demanded) = demanded else {
        return Ok(());
    };
    let Some(SourcePlan::Subquery { body, .. }) = &mut block.from else {
        unreachable!()
    };
    prune_positions(
        body,
        &|position| demanded.get(position).copied().unwrap_or(true),
        None,
        context,
    )
}

fn has_named_output(
    query: &QueryPlan,
    aliases: &[String],
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    if let Some(schema) = &query.analyzed_output {
        for (index, column) in schema.columns().iter().enumerate() {
            control.check()?;
            if aliases.get(index).unwrap_or(column) == name {
                return Ok(true);
            }
        }
        return Ok(false);
    }
    let RelationalPlan::QueryBlock(block) = &query.root else {
        // Other relational roots are not pruned by this direct-source path.
        return Ok(true);
    };
    for (index, projection) in block.projections.iter().enumerate() {
        control.check()?;
        let matches = if let Some(alias) = aliases.get(index) {
            alias == name
        } else {
            uqa_sql::semantics::with_projection_label(projection, control, |label| {
                Ok(label == name)
            })?
        };
        if matches {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Remove unused scalar output expressions while retaining the relation's complete positional descriptor and analyzed column authority.
pub fn prune_outputs(
    query: &mut QueryPlan,
    aliases: &[String],
    required: &dyn Fn(&str) -> bool,
    schema: Option<&RowSchema>,
    context: OutputPruningContext<'_>,
) -> Result<(), SQLError> {
    let Some(demanded) = output_demand(query, aliases, required, schema, context.production)?
    else {
        return Ok(());
    };
    prune_positions(
        query,
        &|position| demanded.get(position).copied().unwrap_or(true),
        schema,
        context,
    )
}

pub(super) fn output_demand<'a>(
    query: &QueryPlan,
    aliases: &[String],
    required: &dyn Fn(&str) -> bool,
    schema: Option<&RowSchema>,
    control: ProductionControl<'a>,
) -> Result<Option<ProductionVec<'a, bool>>, SQLError> {
    let mut demanded = ProductionVec::new(control);
    if let Some(schema) = schema.or(query.analyzed_output.as_ref()) {
        demanded.reserve(schema.columns().len())?;
        for (position, name) in schema.columns().iter().enumerate() {
            control.check()?;
            demanded.push_copy(required(aliases.get(position).unwrap_or(name)))?;
        }
    } else if !uqa_sql::semantics::visit_query_output_columns(
        query,
        aliases,
        &control,
        &mut |name| {
            demanded.reserve(1)?;
            demanded.push_copy(required(name))?;
            Ok(())
        },
    )? {
        return Ok(None);
    }
    control.check()?;
    Ok(Some(demanded))
}

pub(super) fn prune_positions(
    query: &mut QueryPlan,
    required: &dyn Fn(usize) -> bool,
    schema: Option<&RowSchema>,
    context: OutputPruningContext<'_>,
) -> Result<(), SQLError> {
    if query.analyzed_output.is_none() {
        if let Some(schema) = schema {
            query.analyzed_output = Some(schema.clone());
            if let RelationalPlan::QueryBlock(block) = &mut query.root {
                block.compute = block.compute_kind(context.aggregates);
            }
        }
    }
    if matches!(query.root, RelationalPlan::SetOp { .. }) {
        return super::union_projection::prune(query, required, context);
    }
    let schema = schema.or(query.analyzed_output.as_ref());
    let RelationalPlan::QueryBlock(inner) = &mut query.root else {
        return Ok(());
    };
    if inner.distinct || !inner.distinct_on.is_empty() || !inner.windows.is_empty() {
        return Ok(());
    }
    if !expand_local_stars(inner, query.analyzed_source.as_deref(), schema) {
        return Ok(());
    }
    let visible_ctes =
        |name: &str| query.ctes.iter().any(|cte| cte.name == name) || (context.visible_ctes)(name);
    let mut replacements = Vec::new();
    for (index, projection) in inner.projections.iter().enumerate() {
        if required(index)
            || needed_by_ordering(inner, index)
            || needed_by_grouping(inner, index, query.analyzed_source.as_deref())
            || !removable(&projection.expr, &inner.subqueries, context, &visible_ctes)?
        {
            continue;
        }
        // Retain the schema even though the enclosing query never reads this slot.
        let ty = schema
            .and_then(|schema| schema.column_types().get(index))
            .cloned()
            .flatten()
            .or_else(|| {
                uqa_sql::scalar_type(&projection.expr, &RowSchema::default(), &[])
                    .ok()
                    .flatten()
            });
        if let Some(ty) = ty {
            replacements.push((
                index,
                ScalarExpr::TypedLiteral {
                    collation_oid: schema.and_then(|schema| schema.column_collation(index)),
                    composite_source: None,
                    value: Value::Null,
                    ty: ty.catalog_name(),
                    bound_type: Some(ty),
                    parameter_index: None,
                },
            ));
        }
    }
    for (index, expression) in replacements {
        let projection = &mut inner.projections[index];
        projection.expr.visit(&mut |part| match part {
            ScalarExpr::Column(column) => {
                inner
                    .privilege_columns
                    .insert(uqa_sql::ColumnIdentity::unqualified(column.clone()));
            }
            ScalarExpr::QualifiedColumn { qualifier, column } => {
                inner
                    .privilege_columns
                    .insert(uqa_sql::ColumnIdentity::qualified(
                        qualifier.clone(),
                        column.clone(),
                    ));
            }
            _ => {}
        });
        if projection.alias.is_none() {
            projection.alias = Some(uqa_sql::semantics::projection_label_at(projection));
        }
        projection.expr = expression;
    }
    Ok(())
}

pub(crate) fn expand_local_stars(
    inner: &mut QueryBlockPlan,
    source: Option<&uqa_sql::plan::AnalyzedSource>,
    schema: Option<&RowSchema>,
) -> bool {
    if inner
        .projections
        .iter()
        .any(|p| matches!(p.expr, ScalarExpr::Star | ScalarExpr::QualifiedStar(_)))
    {
        let Some(source) = source else {
            return false;
        };
        if schema.is_some_and(|schema| schema.columns_are_open(None))
            || inner
                .projections
                .iter()
                .any(|projection| match &projection.expr {
                    ScalarExpr::Star => source.schema.columns_are_open(None),
                    ScalarExpr::QualifiedStar(qualifier) => {
                        source.schema.columns_are_open(Some(qualifier))
                    }
                    _ => false,
                })
        {
            return false;
        }
        let Ok(expanded) =
            uqa_sql::semantics::expand_bound_projection_stars(&inner.projections, &source.schema)
        else {
            // A qualified enclosing star needs its enclosing layout; a local descriptor alone cannot authorize that expansion.
            return false;
        };
        if schema.is_none_or(|schema| schema.len() != expanded.len()) {
            return false;
        }
        inner.projections = expanded;
    }
    true
}

fn needed_by_grouping(
    block: &QueryBlockPlan,
    index: usize,
    source: Option<&uqa_sql::plan::AnalyzedSource>,
) -> bool {
    let name = uqa_sql::semantics::projection_label_at(&block.projections[index]);
    block
        .group_by
        .iter()
        .chain(block.grouping_sets.iter().flatten())
        .any(|expression| match expression {
            ScalarExpr::Column(column) if *column == name => source.is_none_or(|source| {
                uqa_sql::semantics::grouping_sets::grouping_output_name(
                    expression,
                    &source.schema,
                    None,
                )
                .is_some()
            }),
            ScalarExpr::Position(position) => *position == index,
            _ => false,
        })
}

fn needed_by_ordering(block: &QueryBlockPlan, index: usize) -> bool {
    let projection = &block.projections[index];
    let name = uqa_sql::semantics::projection_label_at(projection);
    block.order_by.iter().any(|order| {
        matches!(&order.expr, ScalarExpr::Column(column) if *column == name)
            || matches!(order.expr, ScalarExpr::Position(position) if position == index)
            || matches!(order.expr, ScalarExpr::Literal(Value::Int(position)) if uqa_sql::semantics::query_output_position(position, block.projections.len(), "ORDER BY").is_ok_and(|position| position == index))
            || uqa_sql::semantics::aggregates::exprs_match(&order.expr, &projection.expr)
    })
}

fn removable(
    expression: &ScalarExpr,
    subqueries: &[QueryPlan],
    context: OutputPruningContext<'_>,
    visible_ctes: &dyn Fn(&str) -> bool,
) -> Result<bool, SQLError> {
    let mut removable = true;
    let mut contains_subquery = false;
    expression.visit(&mut |part| match part {
        ScalarExpr::Func {
            name,
            binding,
            args,
            ..
        } => {
            if uqa_sql::semantics::aggregates::is_builtin_aggregate(part) {
                // The aggregate's arguments are still visited for volatile effects; its row cardinality belongs to the block's compute phase.
                return;
            }
            let name = uqa_sql::semantics::builtin_function_dispatch_name(name);
            if context.aggregates.is_registered_aggregate(&name)
                || uqa_sql::semantics::sets::validation::builtin_returns_set(&name)
                || binding.as_ref().is_none_or(|binding| !binding.builtin)
                || uqa_sql::semantics::volatility::builtin_function_volatility(
                    &name,
                    binding.as_ref(),
                    args.len(),
                ) == FunctionVolatility::Volatile
            {
                removable = false;
            }
        }
        ScalarExpr::Cast { ty, .. }
            if !uqa_sql::ColumnType::from_sql_name(ty)
                .is_ok_and(|ty| uqa_sql::type_resolution::immutable_cast_type(&ty)) =>
        {
            removable = false;
        }
        ScalarExpr::WindowCall { .. } | ScalarExpr::CompositeRow { .. } => removable = false,
        ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => contains_subquery = true,
        _ => {}
    });
    if !removable || !contains_subquery {
        return Ok(removable);
    }
    match context.volatility {
        Some(catalog) => Ok(
            !uqa_sql::semantics::volatility::scalar_in_scope_contains_volatile_function(
                catalog,
                expression,
                subqueries,
                visible_ctes,
            )?,
        ),
        None => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    mod demand;
    mod required;

    struct NoAggregates;
    impl AggregateClassifier for NoAggregates {
        fn is_registered_aggregate(&self, _: &str) -> bool {
            false
        }
    }

    struct Catalog;
    impl uqa_sql::semantics::volatility::VolatilityCatalog for Catalog {
        fn host_function_volatility(&self, _: &str) -> Option<FunctionVolatility> {
            None
        }
        fn routine_volatilities(
            &self,
            _: &str,
            _: Option<&uqa_sql::ast::FunctionBinding>,
        ) -> Option<Vec<FunctionVolatility>> {
            None
        }
        fn view_query(&self, name: &str) -> Result<Option<QueryPlan>, SQLError> {
            if name == "broken" {
                return Err(SQLError::Internal("catalog lookup failed".into()));
            }
            if name != "shadow" {
                return Ok(None);
            }
            let crate::UnifiedPlan::Query(query) = crate::UnifiedPlan::lower(
                uqa_sql::compile("SELECT nextval('effects')")
                    .unwrap()
                    .remove(0),
            ) else {
                unreachable!();
            };
            Ok(Some(*query))
        }
    }

    #[test]
    fn discarded_derived_subqueries_keep_their_lexical_scope_and_effects() {
        for (sql, outer_cte, removed) in [
            ("SELECT 7,(SELECT 1/0)", false, true),
            (
                "SELECT (SELECT nextval('effects')),(SELECT 1/0)",
                false,
                true,
            ),
            ("SELECT 7,(SELECT nextval('effects'))", false, false),
            (
                "SELECT 7,(WITH shadow AS (VALUES(1/0)) SELECT * FROM shadow)",
                false,
                true,
            ),
            ("SELECT 7,(SELECT * FROM shadow)", true, true),
            ("SELECT 7,(SELECT * FROM shadow)", false, false),
        ] {
            let crate::UnifiedPlan::Query(mut query) =
                crate::UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
            else {
                panic!("query");
            };
            query.analyzed_output = Some(RowSchema::with_types(
                vec!["used".into(), "unused".into()],
                vec![Some(uqa_sql::ColumnType::Integer); 2],
            ));
            prune_positions(
                &mut query,
                &|position| position == 0,
                None,
                OutputPruningContext {
                    volatility: Some(&Catalog),
                    visible_ctes: &|name| outer_cte && name == "shadow",
                    ..OutputPruningContext::new(&NoAggregates)
                },
            )
            .unwrap();
            let RelationalPlan::QueryBlock(block) = &query.root else {
                unreachable!();
            };
            assert_eq!(
                matches!(
                    block.projections[1].expr,
                    ScalarExpr::TypedLiteral {
                        value: Value::Null,
                        ..
                    }
                ),
                removed,
                "{sql}, outer CTE={outer_cte}"
            );
        }
    }

    #[test]
    fn discarded_derived_subquery_catalog_failure_is_not_hidden() {
        let crate::UnifiedPlan::Query(mut query) = crate::UnifiedPlan::lower(
            uqa_sql::compile("SELECT (SELECT * FROM broken)")
                .unwrap()
                .remove(0),
        ) else {
            panic!("query");
        };
        let before = query.clone();
        let error = prune_positions(
            &mut query,
            &|_| false,
            None,
            OutputPruningContext {
                volatility: Some(&Catalog),
                ..OutputPruningContext::new(&NoAggregates)
            },
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "internal error: catalog lookup failed");
        assert_eq!(
            serde_json::to_value(query).unwrap(),
            serde_json::to_value(before).unwrap()
        );
    }

    #[test]
    fn unused_grouping_aliases_preserve_keys_but_local_inputs_take_precedence() {
        for (input, pruned) in [("x", true), ("v", false)] {
            let crate::UnifiedPlan::Query(mut query) = crate::UnifiedPlan::lower(
                uqa_sql::compile("SELECT 1/0 AS x FROM data GROUP BY x")
                    .unwrap()
                    .remove(0),
            ) else {
                panic!("query");
            };
            query.analyzed_source = Some(std::sync::Arc::new(uqa_sql::plan::AnalyzedSource {
                schema: RowSchema::with_types(
                    vec![input.into()],
                    vec![Some(uqa_sql::ColumnType::Integer)],
                ),
                children: None,
            }));
            let output =
                RowSchema::with_types(vec!["x".into()], vec![Some(uqa_sql::ColumnType::Integer)]);
            prune_positions(
                &mut query,
                &|_| false,
                Some(&output),
                OutputPruningContext::new(&NoAggregates),
            )
            .unwrap();
            let RelationalPlan::QueryBlock(block) = &query.root else {
                panic!("block");
            };
            assert_eq!(
                matches!(
                    block.projections[0].expr,
                    ScalarExpr::TypedLiteral {
                        value: Value::Null,
                        ..
                    }
                ),
                pruned,
                "input {input}"
            );
        }
    }

    #[test]
    fn positional_demand_distinguishes_duplicate_output_names() {
        let crate::UnifiedPlan::Query(mut query) = crate::UnifiedPlan::lower(
            uqa_sql::compile("SELECT 1/0 AS v,2 AS v")
                .unwrap()
                .remove(0),
        ) else {
            panic!("query");
        };
        query.analyzed_output = Some(RowSchema::with_types(
            vec!["v".into(), "v".into()],
            vec![Some(uqa_sql::ColumnType::Integer); 2],
        ));
        prune_positions(
            &mut query,
            &|position| position == 1,
            None,
            OutputPruningContext::new(&NoAggregates),
        )
        .unwrap();
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("block");
        };
        assert!(matches!(
            block.projections[0].expr,
            ScalarExpr::TypedLiteral {
                value: Value::Null,
                ..
            }
        ));
        assert!(matches!(
            block.projections[1].expr,
            ScalarExpr::Literal(Value::Int(2))
        ));
    }

    #[test]
    fn named_output_pruning_preserves_bound_types_aliases_and_column_authority() {
        let crate::UnifiedPlan::Query(mut query) = crate::UnifiedPlan::lower(
            uqa_sql::compile("SELECT a AS first, b AS second FROM data")
                .unwrap()
                .remove(0),
        ) else {
            panic!("query");
        };
        let schema = RowSchema::with_types(
            vec!["first".into(), "second".into()],
            vec![
                Some(uqa_sql::ColumnType::Text),
                Some(uqa_sql::ColumnType::Integer),
            ],
        );
        prune_outputs(
            &mut query,
            &["selected".into(), "unused".into()],
            &|name| name == "selected",
            Some(&schema),
            OutputPruningContext::new(&NoAggregates),
        )
        .unwrap();
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("block");
        };
        assert_eq!(block.projections.len(), 2);
        assert!(matches!(&block.projections[0].expr, ScalarExpr::Column(name) if name == "a"));
        assert_eq!(
            uqa_sql::scalar_type(&block.projections[1].expr, &RowSchema::default(), &[]).unwrap(),
            Some(uqa_sql::ColumnType::Integer)
        );
        assert!(matches!(
            block.projections[1].expr,
            ScalarExpr::TypedLiteral {
                value: Value::Null,
                ..
            }
        ));
        assert_eq!(block.projections[1].alias.as_deref(), Some("second"));
        assert_eq!(
            block.privilege_columns,
            uqa_core::memory::OwnedSet::from([uqa_sql::ColumnIdentity::unqualified("b")])
        );
    }

    #[test]
    fn unused_derived_values_are_removed_without_losing_column_authority_or_schema() {
        for (sql, pruned) in [
            ("SELECT 1 FROM (SELECT lower(v) AS x FROM data) q", true),
            ("SELECT x FROM (SELECT lower(v) AS x FROM data) q", false),
            (
                "SELECT 1 FROM (SELECT DISTINCT lower(v) AS x FROM data) q",
                false,
            ),
            (
                "SELECT 1 FROM (SELECT lower(v) AS x FROM data ORDER BY x) q",
                false,
            ),
            (
                "SELECT 1 FROM (SELECT lower(v) AS x FROM data ORDER BY 1) q",
                false,
            ),
            (
                "SELECT 1 FROM (SELECT lower(v) AS x FROM data ORDER BY 0+1) q",
                true,
            ),
            (
                "SELECT 1 FROM (SELECT generate_series(1,2) AS x FROM data) q",
                false,
            ),
            ("SELECT 1 FROM (SELECT random() AS x FROM data) q", false),
            ("SELECT count(*) FROM (SELECT lower(v) AS x FROM data GROUP BY x) q", false),
            ("SELECT count(*) FROM (SELECT lower(v) AS x FROM data GROUP BY GROUPING SETS ((x), ())) q", false),
        ] {
            let crate::UnifiedPlan::Query(mut query) =
                crate::UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
            else {
                panic!("query")
            };
            let RelationalPlan::QueryBlock(block) = &mut query.root else {
                panic!("block")
            };
            let Some(SourcePlan::Subquery { body, .. }) = &mut block.from else {
                panic!("derived")
            };
            let RelationalPlan::QueryBlock(inner) = &mut body.root else {
                panic!("inner")
            };
            let schema =
                RowSchema::with_types(vec!["v".into()], vec![Some(uqa_sql::ColumnType::Text)]);
            inner.projections[0].expr =
                uqa_sql::bind_type_introspection(inner.projections[0].expr.clone(), &schema, &[]);
            prune(block, OutputPruningContext::new(&NoAggregates)).unwrap();
            let Some(SourcePlan::Subquery { body, .. }) = &block.from else {
                unreachable!()
            };
            let RelationalPlan::QueryBlock(inner) = &body.root else {
                unreachable!()
            };
            assert_eq!(
                matches!(
                    &inner.projections[0].expr,
                    ScalarExpr::TypedLiteral {
                        value: Value::Null,
                        ..
                    }
                ),
                pruned,
                "{sql}"
            );
            if pruned {
                assert_eq!(
                    inner.privilege_columns,
                    uqa_core::memory::OwnedSet::from([uqa_sql::ColumnIdentity::unqualified("v")])
                );
                assert_eq!(inner.projections[0].alias.as_deref(), Some("x"));
                assert_eq!(
                    uqa_sql::scalar_type(&inner.projections[0].expr, &schema, &[]).unwrap(),
                    Some(uqa_sql::ColumnType::Text)
                );
            }
        }
    }
    #[test]
    fn count_rows_and_whole_row_references_have_different_output_demand() {
        for (sql, removed) in [
            ("SELECT count(*) FROM (SELECT 1 AS id,1/0 AS z) q", true),
            (
                "SELECT 2+pg_catalog.count(*) FROM (SELECT 1 AS id,1/0 AS z) q",
                true,
            ),
            (
                "SELECT count(*) FILTER(WHERE id>0) FROM (SELECT 1 AS id,1/0 AS z) q",
                true,
            ),
            ("SELECT count(q) FROM (SELECT 1 AS id,1/0 AS z) q", false),
            ("SELECT count(q.*) FROM (SELECT 1 AS id,1/0 AS z) q", false),
            ("SELECT count(q) FROM (SELECT 1 AS q,1/0 AS z) q", true),
            (
                "SELECT count(q) FROM (SELECT 1 AS id,1/0 AS z) q(q,z)",
                true,
            ),
        ] {
            let crate::UnifiedPlan::Query(mut query) =
                crate::UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
            else {
                panic!("query");
            };
            let RelationalPlan::QueryBlock(outer) = &mut query.root else {
                panic!("outer block");
            };
            prune(outer, OutputPruningContext::new(&NoAggregates)).unwrap();
            let Some(SourcePlan::Subquery { body, .. }) = &outer.from else {
                panic!("derived source");
            };
            let RelationalPlan::QueryBlock(inner) = &body.root else {
                panic!("inner block");
            };
            assert_eq!(
                matches!(
                    inner.projections[1].expr,
                    ScalarExpr::TypedLiteral {
                        value: Value::Null,
                        ..
                    }
                ),
                removed,
                "{sql}"
            );
        }
    }
}
