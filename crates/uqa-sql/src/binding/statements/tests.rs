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
impl crate::schema::dependencies::oid_alias::OidAliasInput for NoRoutines {
    fn resolve_oid_alias_input(&self, _: &ColumnType, _: &str) -> Result<Option<i64>, SQLError> {
        Ok(None)
    }
}
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
    let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
    analyze_executable_plan(
        &StatementAnalysisContext {
            scopes,
            routines: &NoRoutines,
            aliases: &NoRoutines,
        },
        &mut plan,
        &[],
    )
    .map(|_| ())
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
        "CREATE VIEW copied AS SELECT missing, 1 / 0",
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
fn view_input_analysis_precedes_aliases_and_target_validation() {
    for (sql, state, message) in [
        (
            "CREATE VIEW items(same,same) AS SELECT 'bad'::integer,2",
            "22P02",
            "invalid input syntax for type integer: \"bad\"",
        ),
        (
            "CREATE VIEW items(same,same) AS SELECT 'bad'::integer,2 FROM absent_view_source",
            "42P01",
            "relation \"absent_view_source\" does not exist",
        ),
        (
            "CREATE VIEW items(same,same) AS SELECT missing_column,'bad'::integer",
            "42703",
            "column \"missing_column\" does not exist",
        ),
        (
            "CREATE VIEW items(same,same) AS SELECT 'bad'::integer,missing_column",
            "22P02",
            "invalid input syntax for type integer: \"bad\"",
        ),
        (
            "CREATE OR REPLACE VIEW items(same,same) AS SELECT 'bad'::integer,2",
            "22P02",
            "invalid input syntax for type integer: \"bad\"",
        ),
        (
            "CREATE VIEW items(same,same) AS SELECT CASE WHEN false THEN 'bad'::integer ELSE 1 END,2",
            "22P02",
            "invalid input syntax for type integer: \"bad\"",
        ),
    ] {
        let error = analyze(&Scopes::default(), sql).unwrap_err();
        assert_eq!(error.sqlstate(), Some(state), "{sql}: {error}");
        assert_eq!(error.to_string(), message, "{sql}");
    }
    for sql in [
        "CREATE VIEW saved AS SELECT 'bad'::text::integer AS value",
        "CREATE VIEW saved AS SELECT 1 / 0 AS value",
    ] {
        analyze(&Scopes::default(), sql).unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
}

#[test]
fn mutation_parameter_analysis_precedes_result_schema_binding_in_one_retained_scope() {
    let scopes = Scopes::default();
    analyze(&scopes, "INSERT INTO items VALUES (1) RETURNING id").unwrap();
    assert_eq!(*scopes.events.borrow(), ["scope", "binding", "release"]);
    scopes.events.borrow_mut().clear();
    assert!(analyze(&scopes, "INSERT INTO missing_table VALUES (1) RETURNING id").is_err());
    assert_eq!(*scopes.events.borrow(), ["scope", "binding", "release"]);
}

#[test]
fn ordinary_inputs_are_read_after_sources_and_before_later_expressions() {
    for (sql, state, message) in [
        (
            "SELECT 'absent'::regclass FROM missing_source",
            "42P01",
            "relation \"missing_source\" does not exist",
        ),
        (
            "SELECT 'absent'::regclass, missing_column",
            "42P01",
            "relation \"absent\" does not exist",
        ),
        (
            "SELECT missing_column, 'absent'::regclass",
            "42703",
            "column \"missing_column\" does not exist",
        ),
        (
            "WITH q AS (SELECT 'absent'::regclass) SELECT missing_column",
            "42P01",
            "relation \"absent\" does not exist",
        ),
        (
            "SELECT ('absent'::text)::regclass, missing_column",
            "42703",
            "column \"missing_column\" does not exist",
        ),
    ] {
        let scopes = Scopes::default();
        let error = analyze(&scopes, sql).unwrap_err();
        assert_eq!(error.sqlstate(), Some(state), "{sql}: {error}");
        assert_eq!(error.to_string(), message, "{sql}");
        assert_eq!(*scopes.events.borrow(), ["scope", "binding", "release"]);
    }
}

#[test]
fn input_analysis_leaves_runtime_casts_and_expressions_unevaluated() {
    for sql in [
        "SELECT 1 / 0, ('absent'::text)::regclass",
        "SELECT CASE WHEN false THEN ('absent'::text)::regclass ELSE NULL END",
    ] {
        analyze(&Scopes::default(), sql).unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
}

struct DomainInputs(std::sync::atomic::AtomicUsize);

fn positive_type() -> ColumnType {
    ColumnType::Domain {
        schema: "public".into(),
        name: "positive".into(),
        oid: 16_384,
        array_oid: Some(16_385),
        base: Box::new(ColumnType::Integer),
    }
}

impl FunctionTypeResolver for DomainInputs {
    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        Ok(match name.replace('"', "").as_str() {
            "positive" | "domain#16384" => Some(positive_type()),
            "positive[]" | "domain#16384[]" => Some(ColumnType::Array(Box::new(positive_type()))),
            _ => None,
        })
    }

    fn catalog_input_functions(&self) -> Option<&dyn crate::expr::CatalogInputFunctions> {
        Some(self)
    }

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
impl RoutineResolution for DomainInputs {}
impl crate::expr::CatalogInputFunctions for DomainInputs {
    fn read_unknown_input(&self, text: &str, target: &ColumnType) -> Result<Value, SQLError> {
        assert_eq!(text, "{1,2}");
        assert_eq!(*target, ColumnType::Array(Box::new(positive_type())));
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        crate::expr::cast_value(&Value::Str(text.into()), "integer[]")
    }
}

#[test]
fn executable_input_constants_survive_query_and_command_analysis_once() {
    for sql in [
        "SELECT '{1,2}'::positive[]",
        "SELECT '{1,2}'::positive[] WHERE false",
        "SELECT CASE WHEN false THEN '{1,2}'::positive[] ELSE NULL END",
        "WITH input AS (SELECT '{1,2}'::positive[] AS value) SELECT value FROM input",
        "CREATE VIEW saved AS SELECT '{1,2}'::positive[]",
        "CREATE OR REPLACE VIEW saved AS SELECT '{1,2}'::positive[]",
        "CREATE TABLE saved AS SELECT '{1,2}'::positive[]",
        "CREATE MATERIALIZED VIEW saved AS SELECT '{1,2}'::positive[]",
        "DECLARE input CURSOR FOR SELECT '{1,2}'::positive[]",
        "EXPLAIN SELECT '{1,2}'::positive[]",
    ] {
        let inputs = DomainInputs(std::sync::atomic::AtomicUsize::new(0));
        let scopes = Scopes::default();
        let context = StatementAnalysisContext {
            scopes: &scopes,
            routines: &inputs,
            aliases: &NoRoutines,
        };
        let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        analyze_executable_plan(&context, &mut plan, &[]).unwrap();
        assert_eq!(
            inputs.0.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "{sql}"
        );
        let mut retained = 0;
        let mut observe = |root: &ScalarExpr| {
            root.visit(&mut |expression| {
                if let ScalarExpr::TypedLiteral {
                    value: Value::Array(_),
                    bound_type,
                    ..
                } = expression
                {
                    assert_eq!(
                        bound_type.as_ref(),
                        Some(&ColumnType::Array(Box::new(positive_type())))
                    );
                    retained += 1;
                }
            });
        };
        match &plan {
            UnifiedPlan::Command(command) => match command.as_ref() {
                CommandPlan::CreateView { query, .. }
                | CommandPlan::CreateTableAs { query, .. }
                | CommandPlan::CreateMaterializedView { query, .. }
                | CommandPlan::DeclareCursor { query, .. } => {
                    query.visit_scalar_expressions(&mut observe);
                }
                CommandPlan::Explain { body, .. } => {
                    body.visit_scalar_expressions(&mut observe);
                }
                _ => plan.visit_scalar_expressions(&mut observe),
            },
            UnifiedPlan::Query(_) => plan.visit_scalar_expressions(&mut observe),
        }
        assert_eq!(retained, 1, "{sql}");
        // Consumers may derive the result again; the admitted input is no longer text.
        analyze_executable_plan(&context, &mut plan, &[]).unwrap();
        assert_eq!(
            inputs.0.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "{sql}"
        );
    }
}

#[test]
fn scalar_domains_and_explicit_text_do_not_call_array_input_during_analysis() {
    let inputs = DomainInputs(std::sync::atomic::AtomicUsize::new(0));
    let scopes = Scopes::default();
    for sql in [
        "SELECT '0'::positive WHERE false",
        "SELECT '{0}'::text::positive[] WHERE false",
    ] {
        let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        analyze_executable_plan(
            &StatementAnalysisContext {
                scopes: &scopes,
                routines: &inputs,
                aliases: &NoRoutines,
            },
            &mut plan,
            &[],
        )
        .unwrap();
    }
    assert_eq!(inputs.0.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn canonical_window_inputs_are_owned_once_in_used_unused_and_subquery_definitions() {
    for sql in [
        "SELECT sum(id) OVER w,row_number() OVER w FROM items WINDOW w AS (ORDER BY '{1,2}'::positive[])",
        "SELECT 1 WINDOW unused AS (ORDER BY '{1,2}'::positive[])",
        "SELECT sum(id) OVER child,row_number() OVER base FROM items WINDOW base AS (ORDER BY '{1,2}'::positive[]), child AS (base ROWS CURRENT ROW)",
        "SELECT sum(id) OVER w,row_number() OVER w FROM items WINDOW w AS (ORDER BY (SELECT '{1,2}'::positive[]))",
        "CREATE VIEW kept AS SELECT sum(id) OVER w,row_number() OVER w FROM items WINDOW w AS (ORDER BY '{1,2}'::positive[])",
    ] {
        let inputs = DomainInputs(std::sync::atomic::AtomicUsize::new(0));
        let scopes = Scopes::default();
        let context = StatementAnalysisContext { scopes: &scopes, routines: &inputs, aliases: &NoRoutines };
        let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        analyze_executable_plan(&context, &mut plan, &[]).unwrap_or_else(|error|panic!("{sql}: {error}"));
        assert_eq!(inputs.0.load(std::sync::atomic::Ordering::SeqCst),1,"{sql}");
        analyze_executable_plan(&context, &mut plan, &[]).unwrap_or_else(|error|panic!("repeat {sql}: {error}"));
        assert_eq!(inputs.0.load(std::sync::atomic::Ordering::SeqCst),1,"repeat {sql}");
    }
}

#[test]
fn unused_windows_participate_in_name_input_and_grouping_validation() {
    for (sql, state) in [
        ("SELECT 1 WINDOW w AS (ORDER BY 'bad'::integer)", "22P02"),
        ("SELECT 1 WINDOW w AS (PARTITION BY missing)", "42703"),
        (
            "SELECT id FROM items WINDOW w AS (ORDER BY sum(id))",
            "42803",
        ),
    ] {
        let error = analyze(&Scopes::default(), sql).unwrap_err();
        assert_eq!(error.sqlstate(), Some(state), "{sql}: {error}");
    }
    analyze(
        &Scopes::default(),
        "SELECT 1 FROM items WINDOW w AS (ORDER BY sum(id))",
    )
    .unwrap();
}
