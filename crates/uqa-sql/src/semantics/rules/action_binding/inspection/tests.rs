//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{ColumnDef, ColumnType},
    plpgsql::ResolvedVariable,
};
use std::cell::RefCell;

#[derive(Default)]
struct Catalog {
    calls: RefCell<Vec<String>>,
}
impl RuleSourceCatalog for Catalog {
    fn query_source_columns(
        &self,
        name: &str,
        bound: bool,
    ) -> Result<Option<Vec<String>>, SQLError> {
        self.calls.borrow_mut().push(format!("{name}:{bound}"));
        if name == "missing" {
            return Err(SQLError::UnknownTable(name.into()));
        }
        Ok(Some(vec!["n".into(), "old".into()]))
    }
    fn rule_relation_definitions(&self, _: &str) -> Result<Vec<ColumnDef>, SQLError> {
        Ok(vec![
            ColumnDef::nullable("n", ColumnType::Integer),
            ColumnDef::nullable("old", ColumnType::Integer),
        ])
    }
}
#[derive(Default)]
struct Recorder {
    calls: Vec<String>,
    fail: Option<usize>,
}
impl Recorder {
    fn record(&mut self, name: String) -> Result<(), SQLError> {
        self.calls.push(name);
        if self.fail == Some(self.calls.len()) {
            return Err(SQLError::UnknownColumn("first failure".into()));
        }
        Ok(())
    }
}
impl VariableResolver for Recorder {
    fn resolve_name(&mut self, name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        self.record(format!("name:{name}"))?;
        Ok(None)
    }
    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        self.record(format!("field:{qualifier}.{column}"))?;
        Ok(None)
    }
    fn resolve_param(&mut self, index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        self.record(format!("param:{index}"))?;
        Ok(None)
    }
    fn rewrite_qualified_star(&mut self, qualifier: &str) -> Result<Option<Vec<Expr>>, SQLError> {
        self.record(format!("star:{qualifier}"))?;
        Ok(None)
    }
    fn rewrite_qualified_whole_row(&mut self, qualifier: &str) -> Result<Option<Expr>, SQLError> {
        self.record(format!("row:{qualifier}"))?;
        Ok(None)
    }
}

fn assert_same_observation(sql: &str) {
    let statement = crate::compile(sql).unwrap().remove(0);
    let columns = BTreeSet::from(["n".to_string(), "old".to_string()]);
    for fail in [None, Some(1), Some(3)] {
        let catalog = Catalog::default();
        let mut binding = Recorder {
            fail,
            ..Recorder::default()
        };
        let expected =
            super::super::bind_rule_action(&catalog, &statement, &columns, &mut binding).map(drop);
        let expected_catalog = catalog.calls.take();
        let mut inspection = Recorder {
            fail,
            ..Recorder::default()
        };
        let actual = action(&catalog, &statement, &columns, &mut inspection);
        assert_eq!(
            actual.as_ref().err().map(ToString::to_string),
            expected.as_ref().err().map(ToString::to_string),
            "{sql}"
        );
        assert_eq!(inspection.calls, binding.calls, "{sql}");
        assert_eq!(catalog.calls.take(), expected_catalog, "{sql}");
    }
}

#[test]
fn action_inspection_preserves_scope_callback_and_failure_order() {
    for sql in [
        "SELECT new.n, old.n, new, old.*, ROW(new.*, old.n)",
        "SELECT (new).n, (old).n, old.*, new.*",
        "SELECT new.n, old.n FROM t AS new(n, old)",
        "SELECT new.n, old.n FROM t NATURAL JOIN t AS other",
        "SELECT new.n, old.n FROM t AS new JOIN t AS old USING (n) AS merged",
        "SELECT new.n FROM (t AS new JOIN t AS old USING (n)) AS hidden",
        "SELECT new.n FROM t AS new, LATERAL (SELECT new.n, old.n) AS x",
        "SELECT new.n FROM t AS new JOIN LATERAL (SELECT new.n) AS x ON old.n = x.n",
        "SELECT new.n FROM ROWS FROM (f(new.n), g(old.n)) AS new",
        "SELECT old.n FROM (VALUES (new.n, old.n)) AS new(n, old)",
        "SELECT (SELECT new.n FROM t AS new), new.n, EXISTS(SELECT old.n)",
        "SELECT sum(new.n) FILTER (WHERE old.n > 0) OVER (PARTITION BY new.n ORDER BY old.n ROWS BETWEEN 1 PRECEDING AND CURRENT ROW)",
        "SELECT new.n GROUP BY new.n HAVING old.n > 0 ORDER BY new.n LIMIT old.n OFFSET new.n",
        "SELECT DISTINCT ON (new.n) old.n ORDER BY new.n",
        "SELECT new.n UNION ALL SELECT old.n ORDER BY 1 LIMIT 2",
        "WITH x AS (SELECT new.n AS n) SELECT * FROM x",
        "WITH RECURSIVE x(n) AS (SELECT new.n UNION ALL SELECT n FROM x) SELECT * FROM x",
        "WITH x AS (UPDATE t SET n = 1 RETURNING new.n) SELECT new.n FROM x",
        "SELECT new.n FROM missing",
        "INSERT INTO t(n) VALUES (new.n), (old.n) RETURNING old.n, new.n",
        "INSERT INTO t(n) SELECT new.n ON CONFLICT (n) DO UPDATE SET n = old.n WHERE new.n > 0 RETURNING new.n",
        "INSERT INTO t(n) VALUES (new.n) RETURNING WITH (OLD AS o, NEW AS r) new.n",
        "UPDATE t SET n = new.n, old = old.n FROM t AS new WHERE old.n > 0 RETURNING new.n, old.n",
        "UPDATE t SET n = new.n RETURNING WITH (OLD AS old, NEW AS new) new.n, old.n",
        "UPDATE t SET n = new.n RETURNING WITH (OLD AS old, NEW AS new) new.missing",
        "UPDATE t AS new SET n = old.n WHERE new.n > 0 RETURNING new.n",
        "DELETE FROM t USING t AS new WHERE old.n > 0 RETURNING old.n, new.n",
        "WITH x AS (SELECT new.n) DELETE FROM t WHERE old.n > 0 RETURNING new.n",
    ] { assert_same_observation(sql); }
}

#[test]
fn reference_inspection_borrows_unused_large_action_payloads() {
    let mut statement = crate::compile("INSERT INTO t(n) VALUES ('payload') RETURNING new.n")
        .unwrap()
        .remove(0);
    let Statement::Insert(insert) = &mut statement else {
        panic!("insert")
    };
    insert.rows[0][0] = Expr::Literal(uqa_core::Value::Str("x".repeat(4 * 1024 * 1024)));
    let columns = BTreeSet::from(["n".to_string()]);
    let catalog = Catalog::default();
    let mut result = None;
    let measured = allocation_counter::measure(|| {
        result = Some(super::super::rule_statement_references_row(
            &catalog, &statement, &columns,
        ));
    });
    assert!(!result.unwrap().unwrap());
    assert!(measured.bytes_total < 16 * 1024, "{measured:?}");
    assert_eq!(measured.bytes_current, 0);
}

#[test]
fn condition_inspection_borrows_nested_executable_payloads() {
    let Statement::Select(mut query) = crate::compile("SELECT (SELECT 'payload'), new.n")
        .unwrap()
        .remove(0)
    else {
        panic!("select")
    };
    let Expr::ScalarSubquery(child) = &mut query.projections[0].expr else {
        panic!("subquery")
    };
    child.projections[0].expr = Expr::Literal(uqa_core::Value::Str("x".repeat(4 * 1024 * 1024)));
    let crate::plan::UnifiedPlan::Query(mut plan) =
        crate::plan::UnifiedPlan::lower(Statement::Select(query))
    else {
        panic!("query")
    };
    let crate::plan::RelationalPlan::QueryBlock(block) = &mut plan.root else {
        panic!("block")
    };
    let binding =
        crate::catalog::events::RuleConditionBinding::for_event(&["n".into()], RuleEvent::Update);
    let child = &mut block.subqueries[0];
    let crate::plan::RelationalPlan::QueryBlock(child) = &mut child.root else {
        panic!("child")
    };
    child.projections.push(crate::plan::ProjectionPlan {
        expr: crate::ScalarExpr::InternalColumn(binding.new_column("n").unwrap()),
        alias: None,
    });
    let condition = crate::plan::ExpressionPlan {
        scalar: crate::ScalarExpr::Column("old".into()),
        subqueries: block.subqueries.clone(),
    };
    let measured = allocation_counter::measure(|| {
        assert!(super::super::rule_condition_plan_references_whole_row(
            &condition
        ));
        assert_eq!(
            super::super::rule_condition_plan_row_columns(&condition, &binding),
            BTreeSet::from(["n".to_string()])
        );
    });
    assert!(measured.bytes_total < 16 * 1024, "{measured:?}");
    assert_eq!(measured.bytes_current, 0);
}

mod ownership;
