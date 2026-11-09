//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::test_support::{empty_catalog, table_snapshot};
use uqa_core::{
    memory::{MemoryBudget, RetainedValue},
    CancellationToken, RelationIdentity,
};
use uqa_sql::{
    ast::{ColumnDef, TableConstraintSet},
    routines::{RoutineBody, SQLUserFunction},
};

fn catalog(argument: &str) -> CatalogReadView {
    let mut snapshot = empty_catalog().snapshot().clone();
    let Statement::CreateTable(table) = uqa_sql::compile(&format!(
        "CREATE TABLE items(value bigint DEFAULT nextval({argument}))"
    ))
    .unwrap()
    .remove(0) else {
        panic!("table")
    };
    snapshot.tables.insert(
        RelationIdentity::new("public", "items"),
        table_snapshot([1; 16], table.columns, TableConstraintSet::default()),
    );
    CatalogReadView::new(snapshot)
}

fn check(catalog: &CatalogReadView) -> Result<(), SQLError> {
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let result = validate(catalog, &ProductionControl::new(&memory, &token, &token));
    assert_eq!(memory.used(), 0);
    result
}

#[test]
fn native_sequence_function_names_require_relations_without_evaluating_runtime_inputs() {
    for argument in [
        "'public.items'",
        "'pg_catalog.pg_class'::regclass",
        "2147483647::regclass",
        "'public.absent'::text",
        "'2147483647'",
    ] {
        check(&catalog(argument)).unwrap();
    }
    for argument in [
        "'public.absent'",
        "'public.absent'::regclass",
        "'unqualified'",
    ] {
        assert!(check(&catalog(argument)).is_err(), "{argument}");
    }
}

#[test]
fn native_sequence_function_references_cover_bound_plans_and_preserve_source_bodies() {
    for bound in [false, true] {
        let mut snapshot = empty_catalog().snapshot().clone();
        snapshot.tables.insert(
            RelationIdentity::new("public", "items"),
            table_snapshot(
                [1; 16],
                vec![ColumnDef::nullable("id", uqa_sql::ColumnType::Integer)],
                TableConstraintSet::default(),
            ),
        );
        let Statement::CreateFunction(definition) = uqa_sql::compile("CREATE FUNCTION public.lazy() RETURNS bigint LANGUAGE SQL AS 'SELECT nextval(''public.absent'')'").unwrap().remove(0) else { panic!("function") };
        let body = if bound {
            let plan = UnifiedPlan::lower(
                uqa_sql::compile("SELECT nextval('public.absent')")
                    .unwrap()
                    .remove(0),
            );
            RoutineBody::Bound(uqa_sql::routines::CompiledFunctionBody::SQL(vec![plan]).shared())
        } else {
            RoutineBody::Source
        };
        RetainedValue::make_mut(&mut snapshot.definitions.sql_user_functions)
            .insert(
                "public.lazy".into(),
                vec![SQLUserFunction::new(*definition, body).shared()],
            )
            .unwrap();
        assert_eq!(check(&CatalogReadView::new(snapshot)).is_err(), bound);
    }
}

#[test]
fn native_sequence_function_validation_releases_denied_and_cancelled_workspace() {
    let catalog = catalog("'public.items'");
    let memory = MemoryBudget::new(0);
    let statement = CancellationToken::new();
    let session = CancellationToken::new();
    let control = ProductionControl::new(&memory, &statement, &session);
    assert_eq!(
        validate(&catalog, &control).unwrap_err().sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), 0);
    for token in [&statement, &session] {
        token.cancel();
        assert_eq!(
            validate(&catalog, &control).unwrap_err().sqlstate(),
            Some("57014")
        );
        token.reset();
    }
    assert_eq!(memory.used(), 0);
}
