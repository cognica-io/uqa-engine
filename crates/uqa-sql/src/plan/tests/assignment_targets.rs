//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{one, CommandPlan, ScalarExpr, UnifiedPlan};
use crate::ast::{AssignmentStep, Expr};

const COMMANDS: [(&str, usize); 4] = [
    ("UPDATE t SET a[$1:$2] = $3", 3),
    ("INSERT INTO t (a[$1:$2]) VALUES ($3)", 3),
    ("INSERT INTO t (id) VALUES (1) ON CONFLICT (id) DO UPDATE SET a[$1:$2] = $3", 3),
    ("MERGE INTO t USING s ON true WHEN MATCHED THEN UPDATE SET a[$1:$2] = $3 WHEN NOT MATCHED THEN INSERT (a[$4]) VALUES ($5)", 5),
];

#[test]
fn assignment_boundaries_survive_rendering_and_both_scalar_visitors() {
    for (sql, count) in COMMANDS {
        let mut statement = crate::compile(sql).unwrap().remove(0);
        let encoded = serde_json::to_value(&statement).unwrap();
        let rendered = crate::render::statement_sql(&statement).unwrap();
        let reparsed = crate::compile(&rendered).unwrap().remove(0);
        assert_eq!(serde_json::to_value(reparsed).unwrap(), encoded, "{sql}");
        let mut seen = Vec::new();
        crate::catalog::stored_ast::visit_stored_statement_expressions(
            &mut statement,
            &mut |expr| {
                if let Expr::Param(index) = expr {
                    seen.push(*index);
                    *index += 10;
                }
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(seen, (1..=count).collect::<Vec<_>>(), "{sql}");
        let mut plan = UnifiedPlan::lower(statement);
        let mut seen = Vec::new();
        plan.rewrite_scalar_expressions(&mut |expr| {
            if let ScalarExpr::Param(index) = expr {
                seen.push(*index);
                *index += 10;
            }
        });
        assert_eq!(seen, (11..=count + 10).collect::<Vec<_>>(), "{sql}");
        let UnifiedPlan::Command(command) = plan else {
            panic!("expected mutation command");
        };
        let parameters = command
            .expressions()
            .into_iter()
            .filter_map(|expr| match expr {
                ScalarExpr::Param(index) => Some(*index),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(parameters, (21..=count + 20).collect::<Vec<_>>(), "{sql}");
    }
}

#[test]
fn omitted_bounds_and_composite_fields_remain_distinct_from_element_indexes() {
    let UnifiedPlan::Command(command) = one("UPDATE t SET a.items[:$1][$2:] = $3, b[$4] = $5")
    else {
        panic!("expected command");
    };
    let CommandPlan::Update(update) = command.as_ref() else {
        panic!("expected UPDATE");
    };
    let steps = &update.assignments[0].target.indirection;
    assert!(matches!(&steps[0], AssignmentStep::Field(name) if name == "items"));
    assert!(matches!(
        &steps[1],
        AssignmentStep::Slice {
            lower: None,
            upper: Some(_)
        }
    ));
    assert!(matches!(
        &steps[2],
        AssignmentStep::Slice {
            lower: Some(_),
            upper: None
        }
    ));
    assert!(matches!(
        &update.assignments[1].target.indirection[0],
        AssignmentStep::Index(_)
    ));
}

#[test]
fn subqueries_owned_by_assignment_bounds_remain_query_children() {
    for sql in [
        "UPDATE t SET a[(SELECT 1):(SELECT 2)] = ARRAY[3,4]",
        "INSERT INTO t (a[(SELECT 1):(SELECT 2)]) VALUES (ARRAY[3,4])",
        "INSERT INTO t (id) VALUES (1) ON CONFLICT (id) DO UPDATE SET a[(SELECT 1):(SELECT 2)] = ARRAY[3,4]",
        "MERGE INTO t USING s ON true WHEN MATCHED THEN UPDATE SET a[(SELECT 1):(SELECT 2)] = ARRAY[3,4]",
    ] {
        let UnifiedPlan::Command(command) = one(sql) else {
            panic!("expected command");
        };
        assert_eq!(command.query_inputs().len(), 2, "{sql}");
    }
}
