//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::order_statement_ctes;
use crate::plan::{CommandPlan, CtePlan, UnifiedPlan};
use crate::semantics::{primary_command_cte_references, primary_query_cte_references};

fn names(ctes: &[&CtePlan]) -> Vec<String> {
    ctes.iter().map(|cte| cte.name.clone()).collect()
}

/// The items of a statement that run before and after its primary query.
fn statement_order(sql: &str) -> (Vec<String>, Vec<String>) {
    match UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0)) {
        UnifiedPlan::Query(plan) => {
            let references = primary_query_cte_references(&plan);
            let order = order_statement_ctes(&plan.ctes, plan.ctes.iter().collect(), &references);
            (names(&order.primary), names(&order.postponed))
        }
        UnifiedPlan::Command(command) => {
            let CommandPlan::Insert(plan) = command.as_ref() else {
                panic!("expected an INSERT statement: {sql}");
            };
            let references = primary_command_cte_references(&plan.ctes, &plan.query_inputs(), None);
            let order = order_statement_ctes(&plan.ctes, plan.ctes.iter().collect(), &references);
            (names(&order.primary), names(&order.postponed))
        }
    }
}

#[test]
fn unread_items_run_after_the_primary_query_last_defined_first() {
    assert_eq!(
        statement_order(
            "WITH i1 AS (INSERT INTO t1 VALUES (1)), i2 AS (INSERT INTO t2 VALUES (2)) \
             INSERT INTO t3 VALUES (3)"
        ),
        (Vec::new(), vec!["i2".to_string(), "i1".to_string()])
    );
}

#[test]
fn items_the_primary_query_reads_run_before_it() {
    assert_eq!(
        statement_order(
            "WITH i1 AS (INSERT INTO t1 VALUES (1) RETURNING a), \
             i2 AS (INSERT INTO t2 VALUES (2) RETURNING a) SELECT * FROM i2"
        ),
        (vec!["i2".to_string()], vec!["i1".to_string()])
    );
    assert_eq!(
        statement_order(
            "WITH i1 AS (INSERT INTO t1 VALUES (1) RETURNING a), q AS (SELECT a FROM i1), \
             i2 AS (INSERT INTO t2 VALUES (2) RETURNING a) INSERT INTO t3 SELECT a FROM q"
        ),
        (
            vec!["i1".to_string(), "q".to_string()],
            vec!["i2".to_string()]
        )
    );
}

#[test]
fn a_postponed_item_runs_after_the_items_it_reads() {
    assert_eq!(
        statement_order(
            "WITH i1 AS (INSERT INTO t1 VALUES (1) RETURNING a), \
             i2 AS (INSERT INTO t2 SELECT a FROM i1 RETURNING a) INSERT INTO t3 VALUES (5)"
        ),
        (Vec::new(), vec!["i1".to_string(), "i2".to_string()])
    );
    assert_eq!(
        statement_order(
            "WITH q AS (SELECT 1 AS a), i1 AS (INSERT INTO t1 SELECT a FROM q), \
             s AS (SELECT 2 AS b) SELECT * FROM s"
        ),
        (
            vec!["s".to_string()],
            vec!["q".to_string(), "i1".to_string()]
        )
    );
}

#[test]
fn a_statement_without_data_modifying_items_keeps_its_order() {
    assert_eq!(
        statement_order("WITH a AS (SELECT 1 AS x), b AS (SELECT x FROM a) SELECT * FROM b"),
        (vec!["a".to_string(), "b".to_string()], Vec::new())
    );
}
