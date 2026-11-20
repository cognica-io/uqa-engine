//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Predicate, RelationshipValidation, Scheduled};
use crate::catalog::{
    image_validation::tests::{snapshot, windows_until_rejected},
    test_support::{table_snapshot, NoRoutines},
    CatalogReadView,
};
use uqa_core::{
    catalog_index::CatalogIndexRow,
    memory::{MemoryBudget, RetainedValue},
    CancellationToken, RelationIdentity,
};
use uqa_sql::{
    ast::{
        AutoIncrement, ColumnDef, RelationPersistence, TableConstraintSet, TableHierarchy,
        TableKeyConstraint, TableKeyConstraintKind,
    },
    ColumnType,
};

const WIDTH: usize = 256;

const EXPRESSION_WIDTH: usize = 4096;

fn sequence_function_default(relation: &str, width: usize, position: usize) -> CatalogReadView {
    use uqa_sql::ast::{Expr, Statement};
    let Statement::CreateTable(mut table) = uqa_sql::compile(&format!(
        "CREATE TABLE items(value bigint[] DEFAULT ARRAY[nextval('{relation}')])"
    ))
    .unwrap()
    .remove(0) else {
        unreachable!("table")
    };
    let Some(Expr::Array(items)) = &mut table.columns[0].default else {
        unreachable!("array default")
    };
    let reference = items.pop().unwrap();
    items.resize(width, Expr::Literal(uqa_core::Value::Int(7)));
    items[position] = reference;
    let mut catalog = snapshot();
    catalog.tables.insert(
        RelationIdentity::new("public", "items"),
        table_snapshot([1; 16], table.columns, TableConstraintSet::default()),
    );
    CatalogReadView::new(catalog)
}

#[test]
fn private_catalog_validation_yields_within_named_sequence_function_defaults() {
    let early = sequence_function_default("public.absent", EXPRESSION_WIDTH, 1);
    let late = sequence_function_default("public.absent", EXPRESSION_WIDTH, EXPRESSION_WIDTH - 1);
    for steps in [1, 64] {
        let diagnostic = "missing native sequence function relation";
        let first = windows_until_rejected(&early, steps, diagnostic);
        let last = windows_until_rejected(&late, steps, diagnostic);
        eprintln!("{steps} steps per call: second argument failed on {first}, last on {last}");
        assert!(
            last >= first + (EXPRESSION_WIDTH - 2) / steps,
            "{steps} steps per call: second argument failed on {first}, last on {last}"
        );
    }
}

#[test]
fn a_small_named_sequence_predicate_fits_a_16_kib_allowance() {
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let view = sequence_function_default("public.items", 1, 0);
    let mut cursor = RelationshipValidation::new(view, &memory, &token);
    while !matches!(
        cursor.remaining.first(),
        Some(Predicate::Scheduled(Scheduled::SequenceFunctions))
    ) {
        cursor = cursor
            .advance_with_steps(&NoRoutines, &NoRoutines, 16)
            .unwrap()
            .expect("sequence functions follow the earlier catalog checks");
    }
    assert_eq!(memory.used(), 0);
    let held = memory.reserve(48 << 10).unwrap();
    assert_eq!(memory.available(), 16 << 10);
    while let Some(next) = cursor
        .advance_with_steps(&NoRoutines, &NoRoutines, 1)
        .unwrap()
    {
        if !matches!(
            next.remaining.first(),
            Some(Predicate::Scheduled(Scheduled::SequenceFunctions))
        ) {
            break;
        }
        cursor = next;
    }
    drop(held);
    assert_eq!(memory.used(), 0);
}

#[test]
fn small_routine_reference_registries_fit_the_private_catalog_allowance() {
    use uqa_sql::{
        ast::{EventEnableMode, Statement},
        catalog::events::StoredTrigger,
        routines::{native::NativeRoutineReferences, RoutineBody, SQLUserFunction},
    };

    let predicate = super::ORDERED
        .iter()
        .find(|predicate| matches!(predicate, Predicate::Scheduled(Scheduled::Routines)))
        .unwrap();
    let mut previous_calls = 1;
    for registered in [false, true] {
        let mut snapshot = sequence_function_default("public.items", 1, 0)
            .snapshot()
            .clone();
        if registered {
            let sql = "CREATE FUNCTION public.fire() RETURNS trigger LANGUAGE plpgsql AS 'deferred source'";
            let Statement::CreateFunction(mut handler) = uqa_sql::compile(sql).unwrap().remove(0)
            else {
                unreachable!("trigger function");
            };
            handler.object_id = Some([4; 16]);
            RetainedValue::make_mut(&mut snapshot.definitions.sql_user_functions)
                .insert(
                    handler.name.clone(),
                    vec![SQLUserFunction::new(*handler, RoutineBody::Source).shared()],
                )
                .unwrap();
            let sql = "CREATE TRIGGER saved BEFORE INSERT ON public.items FOR EACH ROW EXECUTE FUNCTION public.fire()";
            let Statement::CreateTrigger(definition) = uqa_sql::compile(sql).unwrap().remove(0)
            else {
                unreachable!("trigger");
            };
            RetainedValue::make_mut(&mut snapshot.definitions.triggers)
                .insert_event(
                    RelationIdentity::new("public", "items"),
                    "saved".into(),
                    StoredTrigger {
                        definition,
                        function_object_id: None,
                        enabled: EventEnableMode::default(),
                        object_id: None,
                        constraint_name: None,
                        catalog_oid: None,
                        constraint_catalog_oid: None,
                    },
                )
                .unwrap();
        }
        let memory = MemoryBudget::new(16 << 10);
        let token = CancellationToken::new();
        let registry = NativeRoutineReferences::new(
            snapshot
                .definitions
                .sql_user_functions
                .values()
                .flatten()
                .map(|f| &f.def),
            &memory,
            &token,
        )
        .unwrap();
        let inline = MemoryBudget::new(memory.used());
        drop(registry);
        super::validate_routines(&snapshot, &inline, &token).unwrap();
        assert_eq!(inline.used(), 0);
        let view = CatalogReadView::new(snapshot);
        let mut cursor = RelationshipValidation::new(view, &memory, &token);
        cursor.remaining = std::slice::from_ref(predicate);
        let mut calls = 1;
        while let Some(next) = cursor
            .advance_with_steps(&NoRoutines, &NoRoutines, 1)
            .unwrap()
        {
            cursor = next;
            calls += 1;
        }
        eprintln!(
            "indexed trigger: {registered}, {calls} routine validation calls, peak {} bytes",
            memory.peak()
        );
        assert!(calls > previous_calls);
        previous_calls = calls;
        assert_eq!(memory.used(), 0);
    }
}

fn inheritance(missing: usize) -> CatalogReadView {
    let mut catalog = snapshot();
    let mut parents = Vec::new();
    for index in 0..WIDTH {
        let name = format!("p{index:04}");
        parents.push(if index == missing {
            "public.absent".into()
        } else {
            format!("public.{name}")
        });
        catalog.tables.insert(
            RelationIdentity::new("public", name),
            table_snapshot(
                (index as u128).to_be_bytes(),
                vec![],
                TableConstraintSet::default(),
            ),
        );
    }
    catalog.tables.insert(
        RelationIdentity::new("public", "child"),
        table_snapshot(
            (WIDTH as u128).to_be_bytes(),
            vec![],
            TableConstraintSet {
                hierarchy: TableHierarchy {
                    parents,
                    ..TableHierarchy::default()
                },
                ..TableConstraintSet::default()
            },
        ),
    );
    CatalogReadView::new(catalog)
}

fn sequences(missing: usize) -> CatalogReadView {
    let mut catalog = snapshot();
    let columns = (0..WIDTH)
        .map(|index| {
            let mut column = ColumnDef::nullable(format!("v{index:04}"), ColumnType::Integer);
            column.object_id = Some((index as u128).to_be_bytes());
            if index == missing {
                column.auto_increment = Some(AutoIncrement {
                    sequence: Some("public.absent".into()),
                    ..AutoIncrement::serial()
                });
            }
            column
        })
        .collect();
    catalog.tables.insert(
        RelationIdentity::new("public", "items"),
        table_snapshot([1; 16], columns, TableConstraintSet::default()),
    );
    CatalogReadView::new(catalog)
}

fn columns_with_missing_reference(missing: usize) -> (Vec<ColumnDef>, Vec<String>) {
    let columns: Vec<_> = (0..WIDTH)
        .map(|index| ColumnDef::nullable(format!("v{index:04}"), ColumnType::Integer))
        .collect();
    let mut references: Vec<_> = columns.iter().map(|column| column.name.clone()).collect();
    references[missing] = "absent".into();
    (columns, references)
}

fn index_columns(missing: usize) -> CatalogReadView {
    let mut catalog = snapshot();
    let table = RelationIdentity::new("public", "items");
    let (columns, references) = columns_with_missing_reference(missing);
    let row = CatalogIndexRow {
        relation: RelationIdentity::new("public", "by_value"),
        table_name: table.qualified_name(),
        index_type: "btree".into(),
        columns_json: serde_json::to_string(&references).unwrap(),
        parameters_json: "{}".into(),
        definition_json: None,
    };
    catalog.tables.insert(
        table,
        table_snapshot([1; 16], columns, TableConstraintSet::default()),
    );
    RetainedValue::make_mut(&mut catalog.definitions.catalog_indexes)
        .insert(row, RelationPersistence::Permanent)
        .unwrap();
    CatalogReadView::new(catalog)
}

fn key_columns(missing: usize) -> CatalogReadView {
    let mut catalog = snapshot();
    let (columns, references) = columns_with_missing_reference(missing);
    let key = TableKeyConstraint {
        kind: TableKeyConstraintKind::Unique,
        columns: references,
        catalog_identity: None,
        index_identity: None,
        name: None,
        included_columns: vec![],
        nulls_not_distinct: false,
        without_overlaps: false,
    };
    catalog.tables.insert(
        RelationIdentity::new("public", "items"),
        table_snapshot(
            [1; 16],
            columns,
            TableConstraintSet {
                key_constraints: vec![key],
                ..TableConstraintSet::default()
            },
        ),
    );
    CatalogReadView::new(catalog)
}

fn check_windows(fixture: fn(usize) -> CatalogReadView, diagnostic: &str) {
    let first = fixture(0);
    let last = fixture(WIDTH - 1);
    for steps in [1, 32] {
        let early = windows_until_rejected(&first, steps, diagnostic);
        let late = windows_until_rejected(&last, steps, diagnostic);
        assert!(
            late >= early + (WIDTH - 1) / steps,
            "{steps} steps per call: first relationship failed on call {early}, last on {late}"
        );
    }
}

#[test]
fn private_catalog_validation_yields_between_inheritance_parents() {
    check_windows(inheritance, "missing parent table");
}

#[test]
fn private_catalog_validation_yields_between_implicit_sequence_columns() {
    check_windows(sequences, "missing implicit sequence");
}

#[test]
fn private_catalog_validation_yields_between_index_columns() {
    check_windows(
        index_columns,
        "invalid native index reference: missing live column",
    );
}

#[test]
fn private_catalog_validation_yields_between_key_constraint_columns() {
    check_windows(
        key_columns,
        "invalid native constraint reference: missing live column",
    );
}
