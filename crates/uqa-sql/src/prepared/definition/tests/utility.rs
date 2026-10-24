//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::prepared::utility::PreparedSessionMetadata;

struct Session {
    reads: RefCell<Vec<String>>,
}

fn schema(name: &str, ty: ColumnType) -> RowSchema {
    RowSchema::with_types(vec![name.into()], vec![Some(ty)])
}

impl PreparedSessionMetadata for Session {
    fn show_result_schema(&self, name: &str) -> Result<RowSchema, SQLError> {
        self.reads.borrow_mut().push(format!("show:{name}"));
        if name == "missing_setting" {
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: "unrecognized configuration parameter".into(),
            });
        }
        Ok(schema("TimeZone", ColumnType::Text))
    }

    fn portal_result_schema(&self, name: &str) -> Option<RowSchema> {
        self.reads.borrow_mut().push(format!("portal:{name}"));
        (name == "live").then(|| schema("cursor_value", ColumnType::Integer))
    }

    fn prepared_result_schema(&self, name: &str) -> Option<RowSchema> {
        self.reads.borrow_mut().push(format!("prepared:{name}"));
        (name == "saved").then(|| schema("prepared_value", ColumnType::BigInteger))
    }
}

#[test]
fn session_utilities_read_only_their_existing_descriptors() {
    for (sql, expected, reads) in [
        (
            "SHOW TIME ZONE",
            Some(schema("TimeZone", ColumnType::Text)),
            vec!["show:timezone"],
        ),
        (
            "FETCH ALL FROM live",
            Some(schema("cursor_value", ColumnType::Integer)),
            vec!["portal:live"],
        ),
        ("FETCH ALL FROM missing", None, vec!["portal:missing"]),
        ("MOVE ALL FROM live", None, vec![]),
        (
            "EXECUTE saved($1)",
            Some(schema("prepared_value", ColumnType::BigInteger)),
            vec!["prepared:saved"],
        ),
        ("EXECUTE missing", None, vec!["prepared:missing"]),
    ] {
        let scopes = Scopes {
            fail_at: Some(1),
            ..Scopes::default()
        };
        let session = Session {
            reads: RefCell::default(),
        };
        let context = PreparedDefinitionContext {
            types: &NoRoutines,
            routines: &NoRoutines,
            scopes: &scopes,
            session: &session,
            aliases: &NoRoutines,
        };
        let definition = analyze_definition(
            &context,
            UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0)),
            &[],
        )
        .unwrap();
        assert_eq!(definition.result_schema, expected, "{sql}");
        assert!(definition.parameter_types.is_empty(), "{sql}");
        assert_eq!(*session.reads.borrow(), reads, "{sql}");
        assert_eq!(scopes.captures.get(), 0, "{sql}");
        assert!(definition.effective_search_path.is_none());
        assert!(definition.dependency_snapshot.is_none());
        assert_eq!(
            definition.dependencies,
            PreparedAnalysisDependencies::default()
        );
    }
}

#[test]
fn utility_errors_and_explicit_parameters_do_not_capture_binding_scopes() {
    let scopes = Scopes::default();
    let session = Session {
        reads: RefCell::default(),
    };
    let context = PreparedDefinitionContext {
        types: &NoRoutines,
        routines: &NoRoutines,
        scopes: &scopes,
        session: &session,
        aliases: &NoRoutines,
    };
    let definition = analyze_definition(
        &context,
        UnifiedPlan::lower(crate::compile("SHOW timezone").unwrap().remove(0)),
        &[ColumnType::Integer],
    )
    .unwrap();
    assert_eq!(definition.parameter_types, vec![Some(ColumnType::Integer)]);
    let error = analyze_definition(
        &context,
        UnifiedPlan::lower(crate::compile("SHOW missing_setting").unwrap().remove(0)),
        &[],
    )
    .err()
    .unwrap();
    assert_eq!(error.sqlstate(), Some("42704"));
    assert_eq!(scopes.captures.get(), 0);
}

#[test]
fn raw_statements_and_lowered_plans_agree_on_analysis_ownership() {
    for (sql, expected) in [
        ("SELECT $1", true),
        ("INSERT INTO t VALUES ($1)", true),
        ("UPDATE t SET v = $1", true),
        ("DELETE FROM t", true),
        ("CALL p($1)", true),
        ("EXPLAIN SELECT $1", true),
        ("DECLARE c CURSOR FOR SELECT $1", true),
        ("CREATE TABLE t AS SELECT $1", true),
        ("CREATE MATERIALIZED VIEW m AS SELECT 1", true),
        ("CREATE VIEW v AS SELECT 1", false),
        ("SHOW timezone", false),
        ("FETCH ALL FROM c", false),
        ("EXECUTE saved($1)", false),
        ("BEGIN", false),
        ("ROLLBACK", false),
    ] {
        let statement = crate::compile(sql).unwrap().remove(0);
        assert_eq!(
            crate::prepared::utility::statement_requires_analysis(&statement),
            expected,
            "{sql}"
        );
        assert_eq!(
            crate::prepared::utility::requires_analysis(&UnifiedPlan::lower(statement)),
            expected,
            "{sql}"
        );
    }
}
