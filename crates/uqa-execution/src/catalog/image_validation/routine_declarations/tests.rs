//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{BorrowedTypes, Cursor};
use crate::catalog::{image_validation::tests::snapshot, CatalogReadView};
use std::cell::Cell;
use uqa_core::{
    memory::{MemoryBudget, RetainedValue},
    CancellationToken,
};
use uqa_sql::{
    ast::{ColumnDef, Statement},
    routines::{declaration::RoutineTypeCatalog, RoutineBody, SQLRoutine, SQLUserFunction},
    ColumnType, SQLError,
};

fn routine(parameters: &str) -> SQLRoutine {
    let sql = format!("CREATE FUNCTION public.f({parameters}) RETURNS integer LANGUAGE sql AS 'uncompiled_missing($999)'");
    let Statement::CreateFunction(definition) = uqa_sql::compile(&sql).unwrap().remove(0) else {
        panic!("routine declaration");
    };
    SQLUserFunction::new(*definition, RoutineBody::Source).shared()
}

fn catalog(count: usize, last: SQLRoutine) -> CatalogReadView {
    let mut snapshot = snapshot();
    let valid = routine("");
    let mut group = vec![valid; count - 1];
    group.push(last);
    RetainedValue::make_mut(&mut snapshot.definitions.sql_user_functions)
        .insert("public.f".into(), group)
        .unwrap();
    CatalogReadView::new(snapshot)
}

struct Lookup<'a> {
    calls: &'a Cell<usize>,
    answer: bool,
}

impl RoutineTypeCatalog for Lookup<'_> {
    fn resolve_catalog_column_type(&self, canonical: &str) -> Option<ColumnType> {
        assert_eq!(canonical, "array_alias");
        self.calls.set(self.calls.get() + 1);
        self.answer.then_some(ColumnType::AnyArray)
    }
    fn try_describe_table(&self, _: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        panic!("declaration must not describe tables")
    }
    fn resolve_catalog_column_type_name(&self, _: &str) -> Result<ColumnType, SQLError> {
        panic!("declaration needs only an optional type")
    }
    fn resolve_catalog_user_type_by_oid(&self, _: u32) -> Option<ColumnType> {
        panic!("declaration must not resolve unrelated identities")
    }
    fn require_type_usage(&self, _: &ColumnType) -> Result<(), SQLError> {
        panic!("declaration must not recheck creation privileges")
    }
    fn format_type(&self, _: &ColumnType) -> Result<String, SQLError> {
        panic!("declaration must not format types")
    }
}

fn drive(
    catalog: &CatalogReadView,
    steps: usize,
    answer: bool,
    expected_calls: usize,
) -> (usize, Result<(), SQLError>) {
    let memory = MemoryBudget::new(16 << 10);
    let token = CancellationToken::new();
    let calls = Cell::new(0);
    let mut cursor = Cursor::new(catalog, &memory).unwrap();
    for window in 1..2_000_000 {
        // Each call receives a new, short-lived lookup borrowing this counter.
        let service = Lookup {
            calls: &calls,
            answer,
        };
        let result =
            cursor
                .as_mut()
                .advance(catalog, &BorrowedTypes(&service), &memory, &token, steps);
        if !matches!(result, Ok(false)) {
            drop(cursor);
            assert_eq!(calls.get(), expected_calls);
            assert_eq!(memory.used(), 0);
            return (window, result.map(|_| ()));
        }
    }
    panic!("declaration validation did not finish");
}

#[test]
fn captured_overload_validation_yields_until_the_selected_variadic_lookup() {
    let last = routine("VARIADIC p array_alias");
    let mut one_step = 0;
    for steps in [1, 97] {
        let (short, error) = drive(&catalog(1, last.clone()), steps, false, 1);
        assert_eq!(error.unwrap_err().sqlstate(), Some("42P13"));
        let (late, error) = drive(&catalog(4096, last.clone()), steps, false, 1);
        assert_eq!(
            error.unwrap_err().to_string(),
            "VARIADIC parameter must be an array"
        );
        assert!(late >= short + 4095 / steps);
        if steps == 1 {
            one_step = late;
        } else {
            assert_eq!(late, one_step.div_ceil(steps));
        }
        let (_, valid) = drive(&catalog(1, last.clone()), steps, true, 1);
        valid.unwrap();
        let (_, builtin) = drive(
            &catalog(1, routine("VARIADIC p integer[]")),
            steps,
            false,
            0,
        );
        builtin.unwrap();
    }
}

#[test]
fn captured_overload_validation_releases_cancelled_denied_and_abandoned_work() {
    let catalog = catalog(64, routine("VARIADIC p array_alias"));
    let calls = Cell::new(0);
    let lookup = Lookup {
        calls: &calls,
        answer: true,
    };
    let small = MemoryBudget::new(1);
    assert!(Cursor::new(&catalog, &small).is_err());
    assert_eq!(small.used(), 0);
    let memory = MemoryBudget::new(2 << 20);
    let sibling = memory.reserve(23).unwrap();
    for stop in [0, 11, 71] {
        for cancel in [false, true] {
            let token = CancellationToken::new();
            let mut cursor = Cursor::new(&catalog, &memory).unwrap();
            for _ in 0..stop {
                assert!(!cursor
                    .as_mut()
                    .advance(&catalog, &BorrowedTypes(&lookup), &memory, &token, 1)
                    .unwrap());
            }
            if cancel {
                token.cancel();
                let error = cursor
                    .as_mut()
                    .advance(&catalog, &BorrowedTypes(&lookup), &memory, &token, 1)
                    .unwrap_err();
                assert_eq!(error.sqlstate(), Some("57014"));
            }
            drop(cursor);
            assert_eq!(memory.used(), sibling.bytes());
        }
    }
    assert_eq!(calls.get(), 0);
}
