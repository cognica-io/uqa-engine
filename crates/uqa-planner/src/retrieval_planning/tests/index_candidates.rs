//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{CatalogIndexRow, IndexCatalog};
use crate::retrieval_planning::indexes::index_candidates;
use std::cell::{Cell, RefCell};
use uqa_core::{Predicate, RelationIdentity, Value};
use uqa_operators::OperatorTree;
use uqa_sql::SQLError;

fn index() -> CatalogIndexRow {
    CatalogIndexRow {
        relation: RelationIdentity::new("public", "docs_id"),
        index_type: "btree".into(),
        table_name: "public.docs".into(),
        columns_json: "[\"id\"]".into(),
        parameters_json: "{}".into(),
        definition_json: None,
    }
}

fn filter(field: impl Into<String>, value: Value) -> OperatorTree {
    OperatorTree::Filter {
        field: field.into(),
        predicate: Predicate::Equals(value),
        source: None,
    }
}

#[test]
fn unindexed_filter_discovery_does_not_copy_payloads_or_collect_nodes() {
    let catalog = IndexCatalog {
        events: RefCell::new(Vec::with_capacity(3)),
        rows: vec![index()].into(),
        probe: None,
    };
    let mut children: Vec<_> = (0..8192)
        .map(|_| filter("unindexed", Value::Null))
        .collect();
    children.push(filter(
        "x".repeat(4 * 1024 * 1024),
        Value::Str("y".repeat(4 * 1024 * 1024)),
    ));
    let tree = OperatorTree::Union(children);
    let allocation = allocation_counter::measure(|| {
        assert!(index_candidates(&catalog, "docs", &tree)
            .unwrap()
            .is_empty());
    });
    assert!(allocation.bytes_total < 4096, "{allocation:?}");
    assert_eq!(
        *catalog.events.borrow(),
        ["has table", "resolve table", "list indexes"]
    );
}

#[test]
fn index_cardinality_borrows_the_original_filter_until_selection() {
    let tree = filter("id", Value::Str("x".repeat(4 * 1024 * 1024)));
    let OperatorTree::Filter {
        field, predicate, ..
    } = &tree
    else {
        unreachable!()
    };
    let probe = |observed_field: &str, observed_predicate: &Predicate| {
        assert_eq!(observed_field.as_ptr(), field.as_ptr());
        assert!(std::ptr::eq(observed_predicate, predicate));
        Ok(None)
    };
    let catalog = IndexCatalog {
        events: RefCell::new(Vec::with_capacity(4)),
        rows: vec![index()].into(),
        probe: Some(&probe),
    };
    let allocation = allocation_counter::measure(|| {
        assert!(index_candidates(&catalog, "docs", &tree)
            .unwrap()
            .is_empty());
    });
    assert!(allocation.bytes_total < 4096, "{allocation:?}");
    assert_eq!(catalog.events.borrow().last(), Some(&"cardinality"));
}

#[test]
fn selected_candidate_retains_one_independent_payload_after_borrowed_costing() {
    let tree = filter("id", Value::Str("x".repeat(4 * 1024 * 1024)));
    let OperatorTree::Filter { predicate, .. } = &tree else {
        unreachable!()
    };
    let probe = |_: &str, observed: &Predicate| {
        assert!(std::ptr::eq(observed, predicate));
        Ok(Some(7))
    };
    let catalog = IndexCatalog {
        events: RefCell::new(Vec::with_capacity(4)),
        rows: vec![index()].into(),
        probe: Some(&probe),
    };
    let mut output = None;
    let allocation = allocation_counter::measure(|| {
        output = Some(index_candidates(&catalog, "docs", &tree).unwrap());
    });
    assert!(
        allocation.bytes_total < 4 * 1024 * 1024 + 4096,
        "{allocation:?}"
    );
    let output = output.unwrap();
    assert_eq!(output.len(), 1);
    assert_eq!(output[0].index_name, "public.docs_id");
    assert_eq!(&output[0].predicate, predicate);
    let Predicate::Equals(Value::Str(text)) = &output[0].predicate else {
        unreachable!()
    };
    let address = text.as_ptr();
    drop(tree);
    let Predicate::Equals(Value::Str(text)) = &output[0].predicate else {
        unreachable!()
    };
    assert_eq!(text.len(), 4 * 1024 * 1024);
    assert_eq!(text.as_ptr(), address);
}

#[test]
fn index_candidate_failure_preserves_order_and_drops_partial_output_without_later_copies() {
    for code in ["53200", "57014", "XX001"] {
        let tree = OperatorTree::Union(vec![
            filter("id", Value::Int(1)),
            filter("id", Value::Int(2)),
            filter("id", Value::Str("x".repeat(4 * 1024 * 1024))),
        ]);
        let calls = Cell::new(0);
        let probe = |_: &str, predicate: &Predicate| {
            calls.set(calls.get() + 1);
            match predicate {
                Predicate::Equals(Value::Int(1)) => Ok(Some(7)),
                Predicate::Equals(Value::Int(2)) => Err(SQLError::Routine {
                    sqlstate: code.into(),
                    message: "candidate read failed".into(),
                }),
                _ => panic!("first error must stop later cardinality callbacks"),
            }
        };
        let catalog = IndexCatalog {
            events: RefCell::new(Vec::with_capacity(5)),
            rows: vec![index()].into(),
            probe: Some(&probe),
        };
        let allocation = allocation_counter::measure(|| {
            let error = index_candidates(&catalog, "docs", &tree).unwrap_err();
            assert_eq!(error.sqlstate(), Some(code));
            assert_eq!(error.to_string(), "candidate read failed");
        });
        assert!(allocation.bytes_total < 4096, "{code}: {allocation:?}");
        assert_eq!(allocation.bytes_current, 0, "{code}: {allocation:?}");
        assert_eq!(calls.get(), 2);
    }
}

#[test]
fn index_candidate_unwind_drops_partial_output_without_later_copies() {
    let tree = OperatorTree::Union(vec![
        filter("id", Value::Int(1)),
        filter("id", Value::Int(2)),
        filter("id", Value::Str("x".repeat(4 * 1024 * 1024))),
    ]);
    let calls = Cell::new(0);
    let probe = |_: &str, _: &Predicate| {
        calls.set(calls.get() + 1);
        if calls.get() == 2 {
            std::panic::resume_unwind(Box::new("candidate read unwind"));
        }
        Ok(Some(7))
    };
    let catalog = IndexCatalog {
        events: RefCell::new(Vec::with_capacity(5)),
        rows: vec![index()].into(),
        probe: Some(&probe),
    };
    let allocation = allocation_counter::measure(|| {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            index_candidates(&catalog, "docs", &tree)
        }));
        assert!(outcome.is_err());
    });
    assert!(allocation.bytes_total < 16 * 1024, "{allocation:?}");
    assert_eq!(allocation.bytes_current, 0, "{allocation:?}");
    assert_eq!(calls.get(), 2);
}
