//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mutation::rules::RuleRowImage;
use crate::query::sources::retained_source::build_retained_source;
use crate::PhysicalOperator;
use uqa_core::Value;
use uqa_sql::ColumnType;

fn columns() -> BTreeMap<String, RuleColumnMetadata> {
    BTreeMap::from([(
        "payload".into(),
        RuleColumnMetadata {
            ty: ColumnType::Text,
            collation_oid: Some(950),
            uses_document_id: false,
            position: 0,
        },
    )])
}

fn select() -> Statement {
    uqa_sql::compile("SELECT old.payload, new.payload")
        .unwrap()
        .remove(0)
}

#[test]
fn rule_action_input_spills_without_copying_a_complete_values_plan() {
    let rows: Vec<_> = (0..1024)
        .map(|index| {
            let mut row = RuleRowImage::empty();
            row.old = Some(BTreeMap::from([(
                "payload".into(),
                Value::Str(format!("{index:04}{}", "o".repeat(8188))),
            )]));
            row.new = Some(BTreeMap::from([(
                "payload".into(),
                Value::Str(format!("{index:04}{}", "n".repeat(8188))),
            )]));
            row
        })
        .collect();
    let matching: Vec<_> = (0..rows.len()).rev().collect();
    let action = select();
    let columns = columns();
    let cancellation = CancellationToken::new();
    let allocated = allocation_counter::measure(|| {
        let (bound, scope) = bind_retained_action(
            &matching,
            &rows,
            &columns,
            64 * 1024,
            &cancellation,
            &|resolver| uqa_sql::plpgsql::bind_statement(&action, resolver),
        )
        .unwrap();
        let Statement::Select(select) = bound.statement else {
            panic!("SELECT action")
        };
        assert!(matches!(
            select.from,
            Some(uqa_sql::ast::FromClause::RetainedRows(_))
        ));
        assert_eq!(scope.descriptor().rows(), 1024);
        for _ in 0..2 {
            let mut scan = build_retained_source(scope.descriptor()).unwrap();
            scan.open().unwrap();
            let mut count = 0;
            while let Some(batch) = scan.next().unwrap() {
                for row in batch.rows {
                    let index = 1023 - count;
                    for (slot, byte) in [(0, b'o'), (1, b'n')] {
                        let Some(Value::Str(text)) = row.value(slot) else {
                            panic!("text input")
                        };
                        assert_eq!(&text[..4], format!("{index:04}"));
                        assert!(text.as_bytes()[4..].iter().all(|actual| *actual == byte));
                    }
                    assert_eq!(row.value(4), Some(&Value::Int(index)));
                    count += 1;
                }
            }
            assert_eq!(count, 1024);
            scan.close().unwrap();
        }
    });
    // Original event ownership is excluded; the action's 32 MiB of field/whole-row copies stays bounded.
    assert!(allocated.bytes_max < 2 * 1024 * 1024, "{allocated:?}");
}

#[test]
fn rule_action_input_preserves_empty_null_and_selected_original_ordinals() {
    let columns = columns();
    let action = select();
    let rows = [
        RuleRowImage::empty(),
        RuleRowImage::empty(),
        RuleRowImage::empty(),
    ];
    for matching in [&[][..], &[2, 0][..]] {
        let (_, scope) = bind_retained_action(
            matching,
            &rows,
            &columns,
            1,
            &CancellationToken::new(),
            &|resolver| uqa_sql::plpgsql::bind_statement(&action, resolver),
        )
        .unwrap();
        let descriptor = scope.descriptor().clone();
        let mut scan = build_retained_source(&descriptor).unwrap();
        scan.open().unwrap();
        let mut actual = Vec::new();
        while let Some(batch) = scan.next().unwrap() {
            for row in batch.rows {
                assert_eq!(row.value(0), Some(&Value::Null));
                assert_eq!(row.value(1), Some(&Value::Null));
                actual.push(row.value(4).unwrap().clone());
            }
        }
        assert_eq!(
            actual,
            matching
                .iter()
                .map(|index| Value::Int(*index as i64))
                .collect::<Vec<_>>()
        );
        drop(scope);
        assert!(build_retained_source(&descriptor).is_err());
    }
}

#[test]
fn rule_action_input_cancellation_and_bad_ordinals_do_not_bind_or_keep_a_source() {
    let columns = columns();
    let rows = [RuleRowImage::empty()];
    let cancel = CancellationToken::new();
    cancel.cancel();
    let binder = |_: &mut dyn VariableResolver| -> Result<Statement, SQLError> {
        panic!("must not bind a failed input")
    };
    assert!(bind_retained_action(&[0], &rows, &columns, 1, &cancel, &binder).is_err());
    assert!(
        bind_retained_action(&[1], &rows, &columns, 1, &CancellationToken::new(), &binder).is_err()
    );
    let action = select();
    let (_, scope) = bind_retained_action(
        &[0],
        &rows,
        &columns,
        1,
        &CancellationToken::new(),
        &|resolver| uqa_sql::plpgsql::bind_statement(&action, resolver),
    )
    .unwrap();
    let descriptor = scope.descriptor().clone();
    drop(scope);
    assert!(build_retained_source(&descriptor).is_err());
}
