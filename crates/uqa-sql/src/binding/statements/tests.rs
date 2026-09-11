//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::FunctionBinding, ColumnType, FunctionTypeResolver};
use std::{cell::RefCell, collections::BTreeMap};

struct NoRoutines;
impl FunctionTypeResolver for NoRoutines {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}
impl RoutineResolution for NoRoutines {}
#[derive(Default)]
struct Scopes {
    events: RefCell<Vec<&'static str>>,
}
struct Scope<'a>(&'a Scopes);
impl Drop for Scope<'_> {
    fn drop(&mut self) {
        self.0.events.borrow_mut().push("release");
    }
}
impl StatementBindingScope for Scope<'_> {
    fn binding_context(&self) -> Result<BindingContext<'_>, SQLError> {
        self.0.events.borrow_mut().push("binding");
        let crate::Statement::CreateTable(table) =
            crate::compile("CREATE TABLE items (id integer)")
                .unwrap()
                .remove(0)
        else {
            panic!("expected table")
        };
        Ok(BindingContext {
            catalog: crate::binding::fixture::catalog(BTreeMap::from([(
                crate::RelationIdentity::new("public", "items"),
                crate::binding::fixture::table_definition(table.columns),
            )])),
            resolution: crate::binding::fixture::resolution(
                vec!["public".into()],
                "pg_temp_1".into(),
            ),
            ctes: BTreeMap::new(),
            deferred_ctes: BTreeMap::new(),
            non_returning_ctes: std::collections::BTreeSet::new(),
            scalar_subqueries: &[],
        })
    }
}
impl StatementAnalysisScopes for Scopes {
    fn with_scope(&self, analyze: StatementAnalysisOperation<'_>) -> Result<(), SQLError> {
        self.events.borrow_mut().push("scope");
        let scope = Scope(self);
        analyze(&scope)
    }
}
fn analyze(scopes: &Scopes, sql: &str) -> Result<(), SQLError> {
    let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
    analyze_executable_plan(
        &StatementAnalysisContext {
            scopes,
            routines: &NoRoutines,
        },
        &plan,
        &[],
    )
}

#[test]
fn query_schema_errors_do_not_evaluate_constants() {
    let scopes = Scopes::default();
    let error = analyze(&scopes, "SELECT missing, 1 / 0").unwrap_err();
    assert!(matches!(error, SQLError::UnknownColumn(name) if name == "missing"));
    assert_eq!(*scopes.events.borrow(), ["scope", "binding", "release"]);
}
#[test]
fn explain_retains_its_scope_and_borrows_metadata_only_for_its_body() {
    let scopes = Scopes::default();
    let error = analyze(&scopes, "EXPLAIN SELECT missing, 1 / 0").unwrap_err();
    assert!(matches!(error, SQLError::UnknownColumn(name) if name == "missing"));
    assert_eq!(
        *scopes.events.borrow(),
        ["scope", "scope", "binding", "release", "release"]
    );
}
#[test]
fn query_bearing_commands_validate_their_query_schema() {
    for sql in [
        "CREATE TABLE copied AS SELECT missing, 1 / 0",
        "CREATE MATERIALIZED VIEW saved AS SELECT missing, 1 / 0",
        "DECLARE c CURSOR FOR SELECT missing, 1 / 0",
    ] {
        let scopes = Scopes::default();
        let error = analyze(&scopes, sql).unwrap_err();
        assert!(
            matches!(error, SQLError::UnknownColumn(name) if name == "missing"),
            "{sql}"
        );
        assert_eq!(*scopes.events.borrow(), ["scope", "binding", "release"]);
    }
}
#[test]
fn mutation_parameter_analysis_precedes_result_schema_binding_in_one_retained_scope() {
    let scopes = Scopes::default();
    analyze(&scopes, "INSERT INTO items VALUES (1) RETURNING id").unwrap();
    assert_eq!(
        *scopes.events.borrow(),
        ["scope", "binding", "binding", "release"]
    );
    scopes.events.borrow_mut().clear();
    assert!(analyze(&scopes, "INSERT INTO missing_table VALUES (1) RETURNING id").is_err());
    assert_eq!(*scopes.events.borrow(), ["scope", "binding", "release"]);
}
