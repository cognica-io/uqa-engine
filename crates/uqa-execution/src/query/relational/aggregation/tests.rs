//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::test_support::NoRoutines;
use crate::physical::run_to_rows;
use crate::query::relational::limit::resolved_sort_keys;
use crate::query::relational::ordering::prepare_deferred_order_projection;
use crate::relational::WindowSpec;
use crate::{ColumnSelection, Limit, PhysicalOperator, Project, Sort, Window, WindowKind};
use uqa_core::Value;
use uqa_sql::plan::{QueryPlan, RelationalPlan};

struct NoSetFunctions;

impl uqa_sql::FunctionTypeResolver for NoSetFunctions {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&uqa_sql::ast::FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<uqa_sql::ColumnType>],
        _: bool,
    ) -> Result<Option<uqa_sql::ColumnType>, SQLError> {
        Ok(None)
    }
}

impl uqa_sql::routines::RoutineResolution for NoSetFunctions {}

impl uqa_sql::plan::AggregateClassifier for NoSetFunctions {
    fn is_registered_aggregate(&self, _: &str) -> bool {
        false
    }
}

#[test]
fn window_ordering_keys_survive_projection_before_sort_and_limit() {
    // Expected identities and row numbers were captured independently from PostgreSQL 18.6 using the same VALUES fixture and SELECT statements.
    for (target, ordering, limit, expected) in [
        ("rn", "price DESC NULLS LAST,id", 2, vec![2, 3]),
        ("rn", "price+1 DESC NULLS LAST,id", 2, vec![2, 3]),
        ("rn", "price DESC,id", 2, vec![4, 2]),
        ("rn", "price ASC NULLS FIRST,id", 3, vec![4, 1, 3]),
        ("price", "price DESC,id", 2, vec![4, 3]),
        ("rn", "2 DESC,id", 2, vec![4, 3]),
    ] {
        let sql = format!(
            "SELECT id,row_number() OVER (ORDER BY id) AS {target} FROM sample ORDER BY {ordering} LIMIT {limit}"
        );
        let uqa_sql::Statement::Select(select) = uqa_sql::compile(&sql).unwrap().remove(0) else {
            panic!("expected SELECT");
        };
        let RelationalPlan::QueryBlock(statement) = QueryPlan::lower(*select).root else {
            panic!("expected query block");
        };
        for deferred in [false, true] {
            let (columns, rows) = ordered_window_rows(&statement, target, limit, deferred);
            assert_eq!(columns, ["id", target], "{sql}");
            assert_eq!(rows.len(), expected.len(), "{sql}");
            for (row, id) in rows.iter().zip(&expected) {
                assert_eq!(row.len(), 2, "hidden ordering keys leaked: {sql}");
                assert_eq!(row["id"], Value::Int(*id), "{sql}");
                assert_eq!(row[target], Value::Int(*id), "{sql}");
            }
        }
    }
}

fn window_fixture() -> Window<'static> {
    let schema = RowSchema::with_types(
        vec!["id".into(), "price".into()],
        vec![Some(uqa_sql::ColumnType::Integer); 2],
    );
    let rows = [Some(10), Some(30), Some(20), None]
        .into_iter()
        .enumerate()
        .map(|(index, price)| {
            [
                ("id".into(), Value::Int(i64::try_from(index + 1).unwrap())),
                ("price".into(), price.map_or(Value::Null, Value::Int)),
            ]
            .into_iter()
            .collect()
        })
        .collect();
    let scan = crate::scan::TableScan::from_rows_with_schema(schema, rows);
    Window::new(
        Box::new(scan),
        WindowSpec {
            partition_by: Vec::new(),
            order_by: vec![crate::SortKey {
                expr: ScalarExpr::Column("id".into()),
                descending: false,
                nulls_first: None,
            }],
        },
        vec![("rn".into(), WindowKind::RowNumber)],
        Vec::new(),
    )
}

fn ordered_window_rows(
    statement: &QueryBlockPlan,
    target: &str,
    limit: u64,
    deferred: bool,
) -> (Vec<String>, Vec<uqa_sql::ResultRow>) {
    let window = window_fixture();
    let output = identity_order_columns(&["id".into(), target.into()]);
    let mut projections = vec![
        (
            ProjectionTarget::Column("id".into()),
            ScalarExpr::Column("id".into()),
        ),
        (
            ProjectionTarget::Column(target.into()),
            ScalarExpr::Column("rn".into()),
        ),
    ];
    let (prepared, hidden) = prepare_order_set_projections(
        &NoSetFunctions,
        &NoRoutines,
        statement,
        &output,
        &mut projections,
        window.row_schema(),
        &[],
    )
    .unwrap();
    let statement = prepared.as_ref().unwrap_or(statement);
    let evaluator = crate::relational::DefaultExpressionEvaluator::shared(Vec::new());
    let ordered: Box<dyn PhysicalOperator> = if deferred {
        let (sort_statement, sort_projections, projections) =
            prepare_deferred_order_projection(statement, &output, projections).unwrap();
        let projection = Project::appending_targets(Box::new(window), sort_projections, Vec::new());
        let keys = resolved_sort_keys(
            &sort_statement,
            &[],
            Some(projection.row_schema()),
            evaluator.as_ref(),
        )
        .unwrap();
        let sorted = Sort::with_work_mem(Box::new(projection), keys, Vec::new(), 1);
        let limited = Limit::new(Box::new(sorted), 0, Some(limit));
        Box::new(Project::with_targets(
            Box::new(limited),
            projections,
            Vec::new(),
        ))
    } else {
        let projection = Project::with_targets(Box::new(window), projections, Vec::new());
        let keys = resolved_sort_keys(
            statement,
            &output,
            Some(projection.row_schema()),
            evaluator.as_ref(),
        )
        .unwrap();
        let sorted = Sort::with_work_mem(Box::new(projection), keys, Vec::new(), 1);
        Box::new(Limit::new(Box::new(sorted), 0, Some(limit)))
    };
    let mut result = ColumnSelection::dropping_internal_attributes(
        ordered,
        &hidden
            .into_iter()
            .map(|(_, column)| column)
            .collect::<Vec<_>>(),
    );
    run_to_rows(&mut result).unwrap()
}
