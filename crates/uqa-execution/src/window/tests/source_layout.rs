//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Window passes over a projected source with retained physical fragments.

use super::*;
use crate::scalar::plan::{PhysicalOuterRow, PhysicalSubqueryRunner};
use std::sync::Arc;
use uqa_sql::plan::{QueryPlan, RelationalPlan};

impl uqa_sql::FunctionTypeResolver for NoSequences {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&uqa_sql::ast::FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}

impl uqa_sql::plan::AggregateClassifier for NoSequences {
    fn is_registered_aggregate(&self, _: &str) -> bool {
        false
    }
}

impl crate::functions::AggregateFunctionRegistry for NoSequences {
    fn registered_aggregate_function(
        &self,
        _: &str,
    ) -> Option<Arc<dyn crate::functions::SQLAggregateFunction>> {
        None
    }
}

impl PhysicalSubqueryRunner for NoSequences {
    fn execute_subquery(
        &self,
        _: usize,
        _: &QueryPlan,
        _: PhysicalOuterRow<'_>,
        _: &[SQLParam],
    ) -> Result<crate::SubqueryResult, SQLError> {
        panic!("unexpected window fixture subquery")
    }
}

impl QueryExpressionContext for NoSequences {
    fn expression_evaluator<'a>(
        &'a self,
        params: &'a [SQLParam],
    ) -> crate::SharedExpressionEvaluator<'a> {
        assert!(params.is_empty());
        Arc::new(NoSequences)
    }

    fn subquery_plans(&self) -> &[QueryPlan] {
        &[]
    }
}

impl crate::ExpressionEvaluator for NoSequences {
    fn evaluate(
        &self,
        expression: &ScalarExpr,
        row: &dyn uqa_sql::expr::RowLookup,
    ) -> crate::ExecResult<Value> {
        Ok(eval_scalar(
            expression,
            &ScalarEvalContext::from_row_lookup(row, &[]),
        )?)
    }
}

#[test]
fn projected_source_layout_survives_multiple_window_sort_passes() {
    let source = RowSchema::with_types(
        vec!["discarded".into(), "v".into(), "padding".into()],
        vec![Some(ColumnType::Integer); 3],
    );
    let projected = RowSchema::project_with_sources(
        &source,
        vec![(
            "v".into(),
            Some(ColumnType::Integer),
            crate::batch::ProjectedSlot::Input(Some(1)),
        )],
        Vec::new(),
        0,
        false,
    );
    assert_eq!(projected.columns(), ["v"]);
    assert_eq!(projected.physical_width(), 3);
    let uqa_sql::Statement::Select(select) = uqa_sql::compile(
        "SELECT v,row_number() OVER(ORDER BY v) AS asc_position,row_number() OVER(ORDER BY v DESC) AS desc_position FROM sample",
    )
    .unwrap()
    .remove(0) else {
        panic!("expected SELECT")
    };
    let RelationalPlan::QueryBlock(block) = QueryPlan::lower(*select).root else {
        panic!("expected query block")
    };
    for budget in [1, 1 << 20] {
        let context = Arc::new(NoSequences);
        let plan = prepare_window_plan(&block.projections);
        let schema = plan
            .output_schema(context.as_ref(), &projected, &[])
            .unwrap();
        let result_columns = plan.projections()[1..]
            .iter()
            .map(|projection| {
                let ScalarExpr::InternalColumn(column) = projection.expr else {
                    panic!("expected window result slot")
                };
                column
            })
            .collect::<Vec<_>>();
        let mut executor =
            PhysicalWindowExecutor::new(context, plan, &[], projected.clone(), budget);
        executor
            .consume(Batch::from_physical_rows(
                projected.clone(),
                [10, 20, 7]
                    .into_iter()
                    .map(|value| {
                        PhysicalRow::from_values(vec![
                            Value::Int(99),
                            Value::Int(value),
                            Value::Int(42),
                        ])
                    })
                    .collect(),
            ))
            .unwrap();
        let mut output = executor.finish().unwrap();
        assert_eq!(output.has_spilled(), budget == 1);
        let mut rows = Vec::new();
        for batch in output.drain().unwrap() {
            let batch = batch.unwrap();
            assert_eq!(batch.schema, schema);
            for row in &batch.rows {
                let view = batch.schema.view(row);
                rows.push(vec![
                    view.column("v").unwrap().clone(),
                    view.internal_column(result_columns[0]).unwrap().clone(),
                    view.internal_column(result_columns[1]).unwrap().clone(),
                ]);
            }
        }
        rows.sort();
        // PostgreSQL result from the nested-window reference, independent of
        // the internal layout and the order in which window passes finish.
        assert_eq!(
            rows,
            vec![
                vec![Value::Int(7), Value::Int(1), Value::Int(3)],
                vec![Value::Int(10), Value::Int(2), Value::Int(2)],
                vec![Value::Int(20), Value::Int(3), Value::Int(1)]
            ]
        );
    }
}
