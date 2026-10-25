//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::ast::{FunctionBinding, FunctionVolatility, InternalRelationId};
struct Catalog;
impl uqa_sql::semantics::volatility::VolatilityCatalog for Catalog {
    fn host_function_volatility(&self, name: &str) -> Option<FunctionVolatility> {
        (name == "host_effect").then_some(FunctionVolatility::Volatile)
    }
    fn routine_volatilities(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
    ) -> Option<Vec<FunctionVolatility>> {
        None
    }
    fn view_query(&self, _: &str) -> Result<Option<QueryPlan>, SQLError> {
        Ok(None)
    }
}
fn plan(sql: &str) -> UnifiedPlan {
    UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
}
#[test]
fn captured_input_pruning_keeps_identity_cardinality_and_volatile_outputs() {
    for (inner, pruned) in [
        ("SELECT 7 AS n,1/0 AS unused WHERE false", true),
        ("SELECT 7 AS n,host_effect() AS unused", false),
        ("SELECT DISTINCT 7 AS n,1/0 AS unused", false),
        ("SELECT 7 AS n,1/0 AS unused ORDER BY unused LIMIT 1", false),
    ] {
        let mut plan = plan(&format!("SELECT input.n FROM ({inner}) input"));
        let relation = InternalRelationId::allocate();
        let UnifiedPlan::Query(query) = &mut plan else {
            panic!("query")
        };
        let RelationalPlan::QueryBlock(outer) = &mut query.root else {
            panic!("block")
        };
        outer.projections[0].expr = ScalarExpr::InternalColumn(relation.column(0));
        let SourcePlan::Subquery {
            captured_output,
            body,
            ..
        } = outer.from.as_mut().unwrap()
        else {
            panic!("input")
        };
        *captured_output = Some(relation);
        let mut expected = body.clone();
        let RelationalPlan::QueryBlock(expected_block) = &mut expected.root else {
            panic!("input block")
        };
        if pruned {
            expected_block.projections[1].expr = ScalarExpr::Literal(Value::Null);
        }
        prune_captured_inputs(&Catalog, &mut plan);
        let UnifiedPlan::Query(query) = &plan else {
            panic!("query")
        };
        let RelationalPlan::QueryBlock(outer) = &query.root else {
            panic!("block")
        };
        let SourcePlan::Subquery {
            body,
            captured_output,
            ..
        } = outer.from.as_ref().unwrap()
        else {
            panic!("input")
        };
        assert_eq!(captured_output, &Some(relation));
        assert_eq!(
            serde_json::to_value(body).unwrap(),
            serde_json::to_value(&expected).unwrap(),
            "{inner}"
        );
    }
}
