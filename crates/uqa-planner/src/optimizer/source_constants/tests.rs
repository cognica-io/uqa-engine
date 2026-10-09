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

    fn view_query(&self, name: &str) -> Result<Option<QueryPlan>, SQLError> {
        Ok((name == "literal_view").then(|| self.0.clone()))
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
