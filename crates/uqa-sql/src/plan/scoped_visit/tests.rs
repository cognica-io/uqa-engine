//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn plan(sql: &str) -> UnifiedPlan {
    UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
}

#[derive(Default)]
struct Tables(Vec<(String, bool, bool)>);

impl ScopedPlanVisitor for Tables {
    fn source(
        &mut self,
        source: &mut SourcePlan,
        visible: &dyn Fn(&str) -> bool,
    ) -> Result<bool, SQLError> {
        if let SourcePlan::Table { name, .. } = source {
            self.0.push((name.clone(), visible("a"), visible("b")));
        }
        Ok(true)
    }
}

#[test]
fn cte_definitions_see_preceding_siblings_and_recursive_forward_names() {
    for (sql, expected) in [
        ("WITH a AS (SELECT * FROM external_source),b AS (SELECT * FROM a) SELECT * FROM b", vec![("external_source", false, false), ("a", true, false), ("b", true, true)]),
        ("WITH RECURSIVE a AS (SELECT * FROM b),b AS (VALUES(1)) SELECT * FROM a", vec![("b", true, true), ("a", true, true)]),
        ("WITH a AS (VALUES(1)) SELECT * FROM (WITH b AS (SELECT * FROM a) SELECT * FROM b) nested", vec![("a", true, false), ("b", true, true)]),
    ] {
        let mut visitor = Tables::default();
        plan(sql).visit_scoped_mut(&mut visitor).unwrap();
        assert_eq!(visitor.0, expected.into_iter().map(|(name, a, b)| (name.to_owned(), a, b)).collect::<Vec<_>>(), "{sql}");
    }
}

#[test]
fn command_query_and_source_inputs_keep_the_commands_ctes() {
    for sql in [
        "WITH a AS (VALUES(1)) INSERT INTO target SELECT (SELECT * FROM a)",
        "WITH a AS (VALUES(1)) UPDATE target SET n=(SELECT * FROM a) FROM a",
        "WITH a AS (VALUES(1)) DELETE FROM target USING a WHERE n=(SELECT * FROM a)",
        "WITH a AS (VALUES(1)) MERGE INTO target USING a ON true WHEN MATCHED THEN DELETE",
        "EXPLAIN WITH a AS (VALUES(1)) SELECT * FROM a",
        "CREATE TABLE target AS WITH a AS (VALUES(1)) SELECT * FROM a",
    ] {
        let mut visitor = Tables::default();
        plan(sql).visit_scoped_mut(&mut visitor).unwrap();
        assert!(!visitor.0.is_empty(), "{sql}");
        assert!(
            visitor
                .0
                .iter()
                .all(|(name, a, b)| name == "a" && *a && !*b),
            "{sql}: {:?}",
            visitor.0
        );
    }
}

#[test]
fn scoped_traversal_borrows_names_and_restores_the_enclosing_scope() {
    struct Check;
    impl ScopedPlanVisitor for Check {
        fn source(
            &mut self,
            source: &mut SourcePlan,
            visible: &dyn Fn(&str) -> bool,
        ) -> Result<bool, SQLError> {
            if let SourcePlan::Table { name, .. } = source {
                assert!(visible("a"));
                assert_eq!(visible("b"), name == "b");
            }
            Ok(true)
        }
    }
    let mut query = plan("WITH a AS (VALUES(1)) SELECT (SELECT * FROM (WITH b AS (VALUES(2)) SELECT * FROM b) nested),(SELECT * FROM a)");
    let observed = allocation_counter::measure(|| query.visit_scoped_mut(&mut Check).unwrap());
    assert_eq!(observed.count_total, 0);
}
