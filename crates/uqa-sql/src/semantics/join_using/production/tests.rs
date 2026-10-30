//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ColumnType;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn schema(alias: &str, names: &[&str], ty: &ColumnType) -> RowSchema {
    RowSchema::with_qualified_types(
        alias,
        names.iter().map(|name| (*name).into()).collect(),
        vec![Some(ty.clone()); names.len()],
    )
}

#[test]
fn controlled_using_retains_only_its_output_and_borrows_physical_demand() {
    let ty = ColumnType::Array(Box::new(ColumnType::Varchar(Some(12))));
    let left = schema("l", &["b", "a", "unused"], &ty);
    let right = schema("r", &["a", "b"], &ty);
    let parent = MemoryBudget::new(32768);
    let sibling = parent.reserve(512).unwrap();
    let memory = parent.child(16384);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let explicit = JoinUsing {
        columns: vec!["a".into(), "b".into()],
        alias: Some("merged".into()),
    };
    for using in [None, Some(&explicit)] {
        let output =
            resolve_join_using_with_control(using, using.is_none(), &left, &right, &control)
                .unwrap()
                .unwrap();
        let names: Vec<_> = output
            .columns
            .iter()
            .map(|column| column.name.as_str())
            .collect();
        assert_eq!(
            names,
            if using.is_some() {
                ["a", "b"]
            } else {
                ["b", "a"]
            }
        );
        assert_eq!(
            output.alias.as_deref(),
            using.and_then(|using| using.alias.as_deref())
        );
        for column in &output.columns {
            assert_eq!(column.comparison_type, Some(ty.clone()));
            assert_eq!(column.output_type, Some(ty.clone()));
        }
        let mut slots = Vec::new();
        output
            .visit_input_slots(&left, &right, &control, &mut |slot| {
                slots.push(slot);
                Ok(())
            })
            .unwrap();
        assert_eq!(
            slots,
            if using.is_some() {
                [1, 3, 0, 4]
            } else {
                [0, 4, 1, 3]
            }
        );
        assert_eq!(memory.used(), output.reserved_bytes());
        let expected = output.columns.capacity() * size_of::<ResolvedJoinColumn>()
            + output
                .columns
                .iter()
                .map(|column| column.name.capacity() + 2 * size_of::<ColumnType>())
                .sum::<usize>()
            + output.alias.as_ref().map_or(0, String::capacity);
        assert_eq!(memory.used(), expected);
        drop(output);
        assert_eq!(parent.used(), sibling.bytes());
    }
}

#[test]
fn controlled_using_preserves_duplicate_alias_missing_ambiguity_and_type_error_order() {
    let left = schema("l", &["id", "repeated", "repeated"], &ColumnType::Integer);
    let right = schema("r", &["id", "repeated"], &ColumnType::Boolean);
    let memory = MemoryBudget::new(16384);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for (names, alias, state, message) in [
        (
            vec!["missing", "missing"],
            Some("l"),
            "42701",
            "appears more than once in USING",
        ),
        (vec!["missing"], Some("l"), "42712", "table name \"l\""),
        (vec!["missing"], None, "42703", "left table"),
        (vec!["repeated"], None, "42702", "left table"),
        (
            vec!["id", "missing"],
            None,
            "42883",
            "operator does not exist",
        ),
    ] {
        let using = JoinUsing {
            columns: names.into_iter().map(str::to_string).collect(),
            alias: alias.map(str::to_string),
        };
        let error = resolve_join_using_with_control(Some(&using), false, &left, &right, &control)
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some(state), "{error}");
        assert!(error.to_string().contains(message), "{error}");
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn controlled_using_rejects_wide_payloads_and_releases_partial_columns() {
    let wide = "x".repeat(64 * 1024);
    let ty = ColumnType::Domain {
        schema: "public".into(),
        name: wide.clone(),
        oid: 40001,
        array_oid: None,
        collation_oid: None,
        base: Box::new(ColumnType::Integer),
    };
    for (names, ty, alias) in [
        (vec!["a", wide.as_str()], ColumnType::Integer, None),
        (vec!["a"], ty, None),
        (vec!["a"], ColumnType::Integer, Some(wide.as_str())),
    ] {
        let left = schema("l", &names, &ty);
        let right = schema("r", &names, &ty);
        let using = JoinUsing {
            columns: names.iter().map(|s| (*s).into()).collect(),
            alias: alias.map(str::to_string),
        };
        let parent = MemoryBudget::new(16384);
        let sibling = parent.reserve(512).unwrap();
        let memory = parent.child(4096);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let mut result = None;
        let allocation = allocation_counter::measure(|| {
            result = Some(resolve_join_using_with_control(
                Some(&using),
                false,
                &left,
                &right,
                &control,
            ));
        });
        assert!(allocation.bytes_total < 8192, "{allocation:?}");
        assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("53200"));
        assert_eq!(parent.used(), sibling.bytes());
    }
}

#[test]
fn controlled_using_observes_both_cancellation_owners_before_scratch() {
    let left = schema("l", &["id"], &ColumnType::Integer);
    let right = schema("r", &["id"], &ColumnType::Integer);
    for cancel_original in [false, true] {
        let memory = MemoryBudget::new(0);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&memory, &original, &invoking);
        let mut error = None;
        let allocation = allocation_counter::measure(|| {
            error = Some(
                resolve_join_using_with_control(None, true, &left, &right, &control).unwrap_err(),
            );
        });
        assert_eq!(error.unwrap().sqlstate(), Some("57014"));
        assert!(allocation.bytes_total < 1024, "{allocation:?}");
        assert_eq!(memory.peak(), 0);
    }
}

#[test]
fn controlled_using_input_observation_checks_empty_pairs_and_callback_failures() {
    let empty = RowSchema::default();
    let left = RowSchema::join(
        &schema("l", &["id"], &ColumnType::Integer),
        &empty,
        ["next".into()],
    );
    let right = RowSchema::join(&empty, &empty, ["id".into(), "next".into()]);
    assert_eq!(left.slot(1), None);
    assert_eq!(right.physical_width(), 0);
    let output = resolve_join_using_with_control(
        None,
        true,
        &left,
        &right,
        &ProductionControl::uncontrolled(),
    )
    .unwrap()
    .unwrap();
    for cancel_original in [false, true] {
        let memory = MemoryBudget::new(0);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let mut calls = 0;
        let error = output
            .visit_input_slots(&left, &right, &control, &mut |_| {
                calls += 1;
                if cancel_original {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                Ok(())
            })
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(calls, 1);
        assert_eq!(memory.peak(), 0);

        let empty = RowSchema::default();
        let nulls = RowSchema::join(&empty, &empty, ["id".into()]);
        assert_eq!(nulls.physical_width(), 0);
        let null_output = resolve_join_using_with_control(
            None,
            true,
            &nulls,
            &nulls,
            &ProductionControl::uncontrolled(),
        )
        .unwrap()
        .unwrap();
        let error = null_output
            .visit_input_slots(&nulls, &nulls, &control, &mut |_| {
                panic!("NULL physical slot")
            })
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
    }
    let mut calls = 0;
    let error = output
        .visit_input_slots(
            &left,
            &right,
            &ProductionControl::uncontrolled(),
            &mut |_| {
                calls += 1;
                Err(SQLError::Routine {
                    sqlstate: "P0001".into(),
                    message: "stop".into(),
                })
            },
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("P0001"));
    assert_eq!(calls, 1);
}

#[test]
fn controlled_join_output_names_share_logical_order_and_retain_exact_payload() {
    let left = schema("l", &["b", "a", "l_only"], &ColumnType::Integer);
    let right = schema("r", &["a", "b", "r_only"], &ColumnType::Integer);
    let using = JoinUsing {
        columns: vec!["a".into(), "b".into()],
        alias: None,
    };
    let resolved = super::super::resolve_join_using(Some(&using), false, &left, &right)
        .unwrap()
        .unwrap();
    for kind in [
        crate::ast::JoinKind::Inner,
        crate::ast::JoinKind::Left,
        crate::ast::JoinKind::Right,
        crate::ast::JoinKind::Full,
    ] {
        let budget = MemoryBudget::new(4096);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let mut output = None;
        let measured = allocation_counter::measure(|| {
            output = Some(
                join_using_column_names_with_control(kind, &left, &right, &resolved, &control)
                    .unwrap(),
            );
        });
        let output = output.unwrap();
        assert_eq!(*output, ["a", "b", "l_only", "r_only"]);
        assert_eq!(measured.bytes_current, output.reserved_bytes() as i64);
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}
