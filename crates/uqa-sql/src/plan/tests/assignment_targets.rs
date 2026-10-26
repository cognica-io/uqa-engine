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
fn borrowed_command_inspection_preserves_analysis_order_without_child_vectors() {
    for (sql, expected) in [
        ("WITH c AS (SELECT $1) INSERT INTO t (a[$2]) SELECT $5 RETURNING $3, (SELECT $4)", &[1, 2, 3, 5, 4][..]),
        ("WITH c AS (SELECT $1) UPDATE t SET a[$2]=$3 FROM (SELECT $6) s WHERE v=$4 RETURNING (SELECT $5)", &[1, 2, 3, 4, 5, 6]),
        ("WITH c AS (SELECT $1) DELETE FROM t USING (SELECT $4) s WHERE v=$2 RETURNING (SELECT $3)", &[1, 2, 3, 4]),
        ("WITH c AS (SELECT $1) MERGE INTO t USING (SELECT $5) s ON v=$2 WHEN MATCHED THEN UPDATE SET v=$3 RETURNING (SELECT $4)", &[1, 2, 3, 4, 5]),
    ] {
        let plan = one(sql);
        let mut seen = Vec::with_capacity(expected.len());
        let allocated = allocation_counter::measure(|| {
            plan.visit_scalar_expressions(&mut |expression| {
                expression.visit(&mut |expression| {
                    if let ScalarExpr::Param(index) = expression {
                        seen.push(*index);
                    }
                });
            });
        });
        assert_eq!(seen, expected, "{sql}");
        assert_eq!(allocated.count_total, 0, "{sql}: {allocated:?}");
    }
}

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
    let steps = &update.assignments[0].target.targets()[0].indirection;
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
        &update.assignments[1].target.targets()[0].indirection[0],
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
        assert_eq!(command.query_inputs().count(), 2, "{sql}");
    }
}

#[test]
fn grouped_assignments_keep_one_source_through_rendering_serialization_and_visitors() {
    for sql in [
        "UPDATE t SET (a,b) = (SELECT $1,$2)",
        "INSERT INTO t(id) VALUES(1) ON CONFLICT(id) DO UPDATE SET (a,b)=(SELECT $1,$2)",
        "MERGE INTO t USING s ON true WHEN MATCHED THEN UPDATE SET (a,b)=(SELECT $1,$2)",
    ] {
        let mut statement = crate::compile(sql).unwrap().remove(0);
        let encoded = serde_json::to_value(&statement).unwrap();
        let rendered = crate::render::statement_sql(&statement).unwrap();
        assert_eq!(
            serde_json::to_value(crate::compile(&rendered).unwrap().remove(0)).unwrap(),
            encoded,
            "{sql}"
        );
        let mut seen = Vec::new();
        crate::catalog::stored_ast::visit_stored_statement_expressions(
            &mut statement,
            &mut |expr| {
                if let Expr::Param(index) = expr {
                    seen.push(*index);
                }
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(seen, [1, 2], "{sql}");
        let mut plan = UnifiedPlan::lower(statement);
        let mut seen = Vec::new();
        plan.rewrite_scalar_expressions(&mut |expr| {
            if let ScalarExpr::Param(index) = expr {
                seen.push(*index);
            }
        });
        assert_eq!(seen, [1, 2], "{sql}");
        let UnifiedPlan::Command(command) = plan else {
            panic!("mutation command")
        };
        assert_eq!(command.query_inputs().count(), 1, "{sql}");
        let encoded = serde_json::to_value(&command).unwrap();
        let restored: Box<CommandPlan> = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(serde_json::to_value(restored).unwrap(), encoded);
    }
}

#[test]
fn separate_query_assignment_groups_keep_distinct_source_identities() {
    let UnifiedPlan::Command(command) = one("UPDATE t SET (a,b)=(SELECT 1,2), (c,d)=(SELECT 1,2)")
    else {
        panic!("command")
    };
    let CommandPlan::Update(update) = command.as_ref() else {
        panic!("UPDATE")
    };
    assert_eq!(update.assignments.len(), 2);
    assert_eq!(update.subqueries.len(), 2);
    assert!(matches!(
        update.assignments[0].value,
        ScalarExpr::ScalarSubquery(0)
    ));
    assert!(matches!(
        update.assignments[1].value,
        ScalarExpr::ScalarSubquery(1)
    ));
    assert_eq!(update.assignments[0].target.targets().len(), 2);
}

#[test]
fn command_query_children_borrow_their_original_order_for_reading_and_rewriting() {
    for (sql, expected) in [
        ("INSERT INTO t SELECT $1 RETURNING (SELECT $2), (SELECT $3)", &[1, 2, 3][..]),
        ("UPDATE t SET v=(SELECT $1) FROM (SELECT $4) s WHERE v=(SELECT $2) RETURNING (SELECT $3)", &[1, 2, 3]),
        ("DELETE FROM t USING (SELECT $3) s WHERE v=(SELECT $1) RETURNING (SELECT $2)", &[1, 2]),
        ("MERGE INTO t USING (SELECT $4) s ON v=(SELECT $1) WHEN MATCHED THEN UPDATE SET v=(SELECT $2) RETURNING (SELECT $3)", &[1, 2, 3]),
        ("SHOW work_mem", &[]),
    ] {
        let UnifiedPlan::Command(mut command) = one(sql) else {
            panic!("expected command");
        };
        let mut seen = Vec::with_capacity(expected.len());
        let allocations = allocation_counter::measure(|| {
            for query in command.query_inputs() {
                query.visit_scalar_expressions(&mut |expression| {
                    expression.visit(&mut |expression| {
                        if let ScalarExpr::Param(index) = expression {
                            seen.push(*index);
                        }
                    });
                });
            }
        });
        assert_eq!(seen, expected, "{sql}");
        assert_eq!(allocations.count_total, 0, "{sql}: {allocations:?}");
        seen.clear();
        let allocations = allocation_counter::measure(|| {
            for query in command.query_inputs_mut() {
                query.rewrite_scalar_expressions(&mut |expression| {
                    if let ScalarExpr::Param(index) = expression {
                        seen.push(*index);
                        *index += 10;
                    }
                });
            }
        });
        assert_eq!(seen, expected, "{sql}");
        assert_eq!(allocations.count_total, 0, "{sql}: {allocations:?}");
        seen.clear();
        for query in command.query_inputs() {
            query.visit_scalar_expressions(&mut |expression| {
                expression.visit(&mut |expression| {
                    if let ScalarExpr::Param(index) = expression {
                        seen.push(*index);
                    }
                });
            });
        }
        assert_eq!(seen, expected.iter().map(|index| index + 10).collect::<Vec<_>>(), "{sql}");
    }
}

#[test]
fn direct_mutation_roots_preserve_order_and_mutable_identity_without_allocating() {
    for (sql, expected) in [
        ("WITH c AS (SELECT $90) INSERT INTO t (a[$1]) VALUES ($2) ON CONFLICT ((id + $3)) WHERE id > $4 DO UPDATE SET a[$5:$6] = $7 WHERE id > $8 RETURNING $9", &[1,2,3,4,5,6,7,8,9][..]),
        ("WITH c AS (SELECT $90) UPDATE t SET a[$1:$2] = $3 FROM (SELECT $91) s WHERE id > $4 RETURNING $5, (SELECT $92)", &[1,2,3,4,5]),
        ("WITH c AS (SELECT $90) DELETE FROM t USING (SELECT $91) s WHERE id > $1 RETURNING $2, (SELECT $92)", &[1,2]),
        ("WITH c AS (SELECT $90) MERGE INTO t USING (SELECT $91) s ON id > $1 WHEN MATCHED AND id > $2 THEN UPDATE SET a[$3:$4] = $5 WHEN NOT MATCHED AND id > $6 THEN INSERT (a[$7]) VALUES ($8) RETURNING $9, (SELECT $92)", &[1,2,3,4,5,6,7,8,9]),
        ("SHOW ALL", &[]),
    ] {
        let UnifiedPlan::Command(mut command) = one(sql) else { panic!("command"); };
        let mut seen = Vec::with_capacity(32);
        let mut addresses = Vec::with_capacity(32);
        let allocations = allocation_counter::measure(|| {
            for expression in command.expressions() {
                addresses.push(std::ptr::from_ref(expression));
                expression.visit(&mut |node| {
                    if let ScalarExpr::Param(index) = node { seen.push(*index); }
                });
            }
        });
        assert_eq!(seen, expected, "{sql}");
        assert_eq!(allocations.count_total, 0, "{sql}: {allocations:?}");
        seen.clear();
        let mut roots = 0;
        let allocations = allocation_counter::measure(|| {
            for expression in command.expressions_mut() {
                assert_eq!(std::ptr::from_ref(expression), addresses[roots]);
                roots += 1;
                crate::plan::rewrite_scalar_expression(expression, &mut |node| {
                    if let ScalarExpr::Param(index) = node { seen.push(*index); *index += 10; }
                });
            }
        });
        assert_eq!(roots, addresses.len());
        assert_eq!(seen, expected, "{sql}");
        assert_eq!(allocations.count_total, 0, "{sql}: {allocations:?}");
        seen.clear();
        for expression in command.expressions() {
            expression.visit(&mut |node| {
                if let ScalarExpr::Param(index) = node { seen.push(*index); }
            });
        }
        assert_eq!(seen, expected.iter().map(|index| index+10).collect::<Vec<_>>(), "{sql}");
    }
}

#[test]
fn query_roots_borrow_the_original_scan_aggregate_and_window_initialization_order() {
    for (sql, expected) in [
        ("SELECT $1 FROM t WHERE id > $2 ORDER BY $3 LIMIT $4 OFFSET $5", &[1,2,3,4,5][..]),
        ("SELECT sum($2) FROM t WHERE id > $1 GROUP BY $3 HAVING sum(id) > $4 ORDER BY $5 LIMIT $6 OFFSET $7", &[1,2,3,4,5,6,7]),
        // The call keeps resolved inputs; the query also owns the canonical window definition.
        ("SELECT sum($2) OVER (PARTITION BY $3 ORDER BY $4 ROWS BETWEEN $5 PRECEDING AND $6 FOLLOWING) FROM t WHERE id > $1", &[1,2,3,4,5,6,3,4,5,6]),
    ] {
        let UnifiedPlan::Query(query) = one(sql) else { panic!("query"); };
        let crate::plan::RelationalPlan::QueryBlock(block) = &query.root else { panic!("block"); };
        let mut seen = Vec::with_capacity(32);
        let allocations = allocation_counter::measure(|| {
            for expression in block.query_expressions() {
                expression.visit(&mut |node| {
                    if let ScalarExpr::Param(index) = node { seen.push(*index); }
                });
            }
        });
        assert_eq!(seen, expected, "{sql}");
        assert_eq!(allocations.count_total, 0, "{sql}: {allocations:?}");
    }
}

#[test]
fn source_roots_stop_at_the_first_failure_and_keep_derived_query_scopes_separate() {
    for sql in [
        "SELECT 1 FROM left_fn($1) l CROSS JOIN (SELECT $90) q LEFT JOIN right_fn($2) r ON $3",
        "SELECT 1 FROM ROWS FROM (left_fn($1), right_fn($2, $3)) f",
    ] {
        let UnifiedPlan::Query(query) = one(sql) else {
            panic!("query");
        };
        let crate::plan::RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("block");
        };
        let source = block.from.as_ref().unwrap();
        for stop in 1..=4 {
            let mut seen = Vec::with_capacity(3);
            let mut result = None;
            let allocations = allocation_counter::measure(|| {
                result = Some(source.try_visit_expressions(&mut |expression| {
                    let ScalarExpr::Param(index) = expression else {
                        panic!("parameter root");
                    };
                    seen.push(*index);
                    if *index == stop {
                        Err(stop)
                    } else {
                        Ok(true)
                    }
                }));
            });
            assert_eq!(seen, (1..=stop.min(3)).collect::<Vec<_>>(), "{sql}");
            assert_eq!(
                result.unwrap(),
                if stop <= 3 { Err(stop) } else { Ok(true) }
            );
            assert_eq!(allocations.count_total, 0, "{sql}: {allocations:?}");
            seen.clear();
            let allocations = allocation_counter::measure(|| {
                let completed = source
                    .try_visit_expressions(&mut |expression| {
                        let ScalarExpr::Param(index) = expression else {
                            panic!("parameter root");
                        };
                        seen.push(*index);
                        Ok::<_, std::convert::Infallible>(*index != stop)
                    })
                    .unwrap();
                assert_eq!(completed, stop > 3);
            });
            assert_eq!(seen, (1..=stop.min(3)).collect::<Vec<_>>(), "{sql}");
            assert_eq!(allocations.count_total, 0, "{sql}: {allocations:?}");
        }
    }
}

#[test]
fn query_block_roots_preserve_source_order_and_error_boundaries_without_allocating() {
    let UnifiedPlan::Query(query) = one(
        "SELECT $4, (SELECT $90) FROM left_fn($1) l JOIN right_fn($2) r ON $3 WHERE id > $5 ORDER BY $6 LIMIT $7 OFFSET $8",
    ) else {
        panic!("query");
    };
    let crate::plan::RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("block");
    };
    let mut seen = Vec::with_capacity(8);
    let allocations = allocation_counter::measure(|| {
        block.visit_expressions(&mut |expression| {
            expression.visit(&mut |node| {
                if let ScalarExpr::Param(index) = node {
                    seen.push(*index);
                }
            });
        });
    });
    assert_eq!(seen, [1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(allocations.count_total, 0, "{allocations:?}");
    seen.clear();
    let mut result = None;
    let allocations = allocation_counter::measure(|| {
        result = Some(block.try_visit_expressions(&mut |expression| {
            expression.visit(&mut |node| {
                if let ScalarExpr::Param(index) = node {
                    seen.push(*index);
                }
            });
            if matches!(expression, ScalarExpr::Param(4)) {
                Err(())
            } else {
                Ok(true)
            }
        }));
    });
    assert_eq!(result.unwrap(), Err(()));
    assert_eq!(seen, [1, 2, 3, 4]);
    assert_eq!(allocations.count_total, 0, "{allocations:?}");
    for stop in [2, 4] {
        seen.clear();
        let allocations = allocation_counter::measure(|| {
            let completed = block
                .try_visit_expressions(&mut |expression| {
                    expression.visit(&mut |node| {
                        if let ScalarExpr::Param(index) = node {
                            seen.push(*index);
                        }
                    });
                    Ok::<_, std::convert::Infallible>(
                        !matches!(expression, ScalarExpr::Param(index) if *index == stop),
                    )
                })
                .unwrap();
            assert!(!completed);
        });
        assert_eq!(seen, (1..=stop).collect::<Vec<_>>());
        assert_eq!(allocations.count_total, 0, "{allocations:?}");
    }
}
