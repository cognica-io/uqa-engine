//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::{
    ast::{FunctionBinding, FunctionVolatility},
    plan::QueryPlan,
};

struct Views(QueryPlan);

impl uqa_sql::semantics::volatility::VolatilityCatalog for Views {
    fn host_function_volatility(&self, _: &str) -> Option<FunctionVolatility> {
        None
    }

    fn routine_volatilities(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
    ) -> Option<Vec<FunctionVolatility>> {
        None
    }

    fn view_query(&self, _: &str) -> Result<Option<QueryPlan>, SQLError> {
        panic!("constant inspection must borrow the retained view")
    }

    fn with_view_query(
        &self,
        name: &str,
        read: &mut dyn FnMut(&QueryPlan) -> Result<bool, SQLError>,
    ) -> Result<Option<bool>, SQLError> {
        (name == "literal_view").then(|| read(&self.0)).transpose()
    }
}

fn query(sql: &str) -> QueryPlan {
    let crate::UnifiedPlan::Query(query) =
        crate::UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("query expected")
    };
    *query
}

#[test]
fn retained_source_constants_preserve_propagation_and_grouping() {
    use uqa_core::{memory::ProductionControl, Value};
    use uqa_sql::ir::{RetainedConstantValue, RetainedScalarLiteral};
    let control = ProductionControl::uncontrolled();
    let retained = ScalarExpr::RetainedLiteral(RetainedScalarLiteral::new(
        RetainedConstantValue::from_produced(
            control.copy_value(&Value::Str("value".into())).unwrap(),
            &control,
        )
        .unwrap(),
        Some(ColumnType::Text),
        Some("text".into()),
        None,
    ));
    for expression in [
        retained.clone(),
        ScalarExpr::Cast {
            expr: Box::new(retained.clone()),
            ty: "text".into(),
            implicit: false,
        },
    ] {
        let values = [expression.clone()];
        let constants = constants_from_values(&values, |_| "x".into(), None);
        assert_eq!(constants["x"].as_ref(), Some(&retained));
        let mut plan = query("SELECT count(*) FROM (SELECT 1 WHERE false) s GROUP BY true");
        let RelationalPlan::QueryBlock(block) = &mut plan.root else {
            panic!("block")
        };
        let source = serde_json::to_value(&block.from).unwrap();
        block.group_by = vec![expression];
        simplify_constant_group(block, None);
        assert_eq!(block.group_by, [ScalarExpr::Literal(Value::Bool(true))]);
        assert_eq!(serde_json::to_value(&block.from).unwrap(), source);
    }
    let incompatible = ScalarExpr::Cast {
        expr: Box::new(retained),
        ty: "integer".into(),
        implicit: false,
    };
    assert!(!constant_literal(&incompatible, None));
}

#[test]
fn view_constants_keep_source_authority_cardinality_and_visible_aliases() {
    for definition in [
        "SELECT 1 AS internal_name",
        "SELECT 1 AS internal_name WHERE false",
    ] {
        let views = Views(query(definition));
        let mut plan = query("SELECT q.renamed FROM literal_view q(renamed)");
        let RelationalPlan::QueryBlock(block) = &mut plan.root else {
            panic!("query block")
        };
        let Some(SourcePlan::Table { bound_columns, .. }) = &mut block.from else {
            panic!("view source")
        };
        *bound_columns = Some(vec!["catalog_column".into()]);
        let source = serde_json::to_value(&block.from).unwrap();
        propagate_source_constants(block, None, Some(&views)).unwrap();
        assert!(matches!(
            block.projections[0].expr,
            ScalarExpr::TypedLiteral {
                value: uqa_core::Value::Int(1),
                ..
            }
        ));
        assert_eq!(serde_json::to_value(&block.from).unwrap(), source);
    }
}

#[test]
fn volatile_view_outputs_and_ordinary_tables_are_not_substituted() {
    for (definition, source) in [
        ("SELECT random() AS x", "literal_view"),
        ("SELECT 1 AS x", "ordinary_table"),
        ("SELECT 1 AS x FROM underlying_table", "literal_view"),
    ] {
        let views = Views(query(definition));
        let mut plan = query(&format!("SELECT x FROM {source}"));
        let RelationalPlan::QueryBlock(block) = &mut plan.root else {
            panic!("query block")
        };
        let before = serde_json::to_value(&*block).unwrap();
        propagate_source_constants(block, None, Some(&views)).unwrap();
        assert_eq!(serde_json::to_value(&*block).unwrap(), before);
    }
}

#[test]
fn source_constants_preserve_resolved_unknown_output_types() {
    for source in [
        "(SELECT NULL AS x) s",
        "(VALUES (NULL)) s(x)",
        "(SELECT 'label' AS x) s",
        "(VALUES ('label')) s(x)",
        "literal_view",
    ] {
        let views = Views(query("SELECT NULL AS x"));
        let mut plan = query(&format!("SELECT x FROM {source}"));
        let RelationalPlan::QueryBlock(block) = &mut plan.root else {
            panic!("query block")
        };
        if let Some(SourcePlan::Table { bound_columns, .. }) = &mut block.from {
            *bound_columns = Some(vec!["x".into()]);
        }
        let original_source = serde_json::to_value(&block.from).unwrap();
        propagate_source_constants(block, None, Some(&views)).unwrap();
        assert!(
            matches!(
                &block.projections[0].expr,
                ScalarExpr::TypedLiteral {
                    value: uqa_core::Value::Null | uqa_core::Value::Str(_),
                    bound_type: Some(ColumnType::Text),
                    parameter_index: None,
                    ..
                }
            ),
            "{source}"
        );
        assert_eq!(serde_json::to_value(&block.from).unwrap(), original_source);
    }
}

#[test]
fn view_constant_inspection_does_not_copy_unrelated_predicate_payloads() {
    for definition in [
        "SELECT 1 AS x WHERE 'payload' = 'x'",
        "SELECT 1 AS x FROM underlying_table WHERE 'payload' = 'x'",
        "WITH t AS (SELECT 'payload') SELECT 1 AS x",
    ] {
        let measure = |bytes| {
            let mut views = Views(query(definition));
            views.0.rewrite_scalar_expressions(&mut |expression| {
                if let ScalarExpr::Literal(uqa_core::Value::Str(text)) = expression {
                    if text == "payload" {
                        *text = "x".repeat(bytes);
                    }
                }
            });
            let mut plan = query("SELECT x FROM literal_view");
            let RelationalPlan::QueryBlock(block) = &mut plan.root else {
                panic!("query block")
            };
            let source = serde_json::to_value(&block.from).unwrap();
            let allocations = allocation_counter::measure(|| {
                propagate_source_constants(block, None, Some(&views)).unwrap();
            });
            assert_eq!(serde_json::to_value(&block.from).unwrap(), source);
            (allocations.count_total, allocations.bytes_total)
        };
        assert_eq!(measure(1), measure(4 * 1024 * 1024), "{definition}");
    }
}

#[test]
fn borrowed_view_constants_keep_partial_aliases_and_ambiguous_output_names() {
    for (columns, aliases, expected) in [
        (
            vec!["catalog_a", "catalog_b"],
            vec!["renamed"],
            [true, true],
        ),
        (vec!["renamed", "renamed"], vec![], [false, false]),
    ] {
        let views = Views(query("SELECT 1 AS a, 2 AS b"));
        let mut plan = query("SELECT q.renamed, q.catalog_b FROM literal_view q");
        let RelationalPlan::QueryBlock(block) = &mut plan.root else {
            panic!("query block")
        };
        let Some(SourcePlan::Table {
            bound_columns,
            column_aliases,
            ..
        }) = &mut block.from
        else {
            panic!("view source")
        };
        *bound_columns = Some(columns.into_iter().map(String::from).collect());
        *column_aliases = aliases.into_iter().map(String::from).collect();
        propagate_source_constants(block, None, Some(&views)).unwrap();
        for (projection, expected) in block.projections.iter().zip(expected) {
            assert_eq!(
                matches!(projection.expr, ScalarExpr::TypedLiteral { .. }),
                expected
            );
        }
    }
}
