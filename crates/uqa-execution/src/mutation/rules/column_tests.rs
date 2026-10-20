//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::CancellationToken;
use uqa_sql::ast::{CreateRule, EventEnableMode};
use uqa_sql::catalog::events::StoredRule;

const COUNT: usize = 16_384;
const MAX_LIVE_BYTES: u64 = 512 * 1024;

fn batch(count: usize) -> PreparedRuleBatch {
    let cancellation = CancellationToken::new();
    let rows = RuleRows::from_rows(
        (0..count).map(|index| {
            let mut row = RuleRowImage::empty();
            row.old = Some(BTreeMap::from([("already".into(), Value::Int(1))]));
            if index % 2 == 0 {
                row.new = Some(BTreeMap::from([("other".into(), Value::Int(2))]));
            }
            Ok(row)
        }),
        // This fixture isolates column metadata: original images are prepared outside the measured scope.
        16 * 1024 * 1024,
        cancellation.clone(),
    )
    .unwrap();
    let mut sparse = RuleIndexBuffer::new(16 * 1024, cancellation.clone()).unwrap();
    for index in (0..count).step_by(3) {
        sparse.push(index).unwrap();
    }
    let rule = |matching_rows, action_row_columns| PreparedRule {
        rule: StoredRule {
            definition: CreateRule {
                name: "columns".into(),
                table: "public.items".into(),
                event: RuleEvent::Update,
                instead: false,
                condition: None,
                actions: Vec::new(),
                or_replace: false,
            },
            enabled: EventEnableMode::default(),
            condition_plan: None,
            condition_binding: None,
            dependencies: None,
            catalog_oid: None,
        },
        matching_rows,
        condition_references_row: true,
        action_references_row: Vec::new(),
        action_row_columns,
        action_columns: Vec::new(),
    };
    PreparedRuleBatch {
        table: "public.items".into(),
        event: RuleEvent::Update,
        rows,
        rules: vec![
            rule(
                RuleIndices::all(count, cancellation.clone()),
                vec![BTreeSet::from(["already".into(), "common".into()])],
            ),
            rule(
                sparse.finish().unwrap(),
                vec![
                    BTreeSet::from(["other".into()]),
                    BTreeSet::from(["common".into(), "sparse".into()]),
                ],
            ),
        ],
        suppress_original: RuleSuppression::new(count, cancellation.clone()),
        action_qualification_count: None,
        work_mem: 16 * 1024,
        cancellation,
    }
}

fn expected(index: usize) -> BTreeSet<String> {
    let mut columns = BTreeSet::from(["already".into(), "common".into()]);
    if index.is_multiple_of(3) {
        columns.extend(["other".into(), "sparse".into()]);
    }
    columns
}

#[test]
fn matched_rule_columns_bound_memory_and_preserve_sparse_action_unions() {
    let batch = batch(COUNT);
    let allocated = allocation_counter::measure(|| {
        let columns = batch.matched_action_row_columns().unwrap();
        let mut count = 0;
        for (index, actual) in columns.reader().unwrap().enumerate() {
            let [actual] = actual.unwrap();
            assert_eq!(actual, expected(index));
            count += 1;
        }
        assert_eq!(count, COUNT);
    });
    assert!(allocated.bytes_max < MAX_LIVE_BYTES, "{allocated:?}");
}

#[test]
fn missing_rule_columns_bound_memory_and_keep_old_new_presence() {
    let batch = batch(COUNT);
    let allocated = allocation_counter::measure(|| {
        let columns = batch.missing_action_row_columns().unwrap();
        let mut count = 0;
        for (index, actual) in columns.reader().unwrap().enumerate() {
            let [old, new] = actual.unwrap();
            let mut expected_old = expected(index);
            expected_old.remove("already");
            let mut expected_new = expected(index);
            expected_new.remove("other");
            assert_eq!(old, expected_old);
            assert_eq!(
                new,
                if index % 2 == 0 {
                    expected_new
                } else {
                    BTreeSet::new()
                }
            );
            count += 1;
        }
        assert_eq!(count, COUNT);
    });
    assert!(allocated.bytes_max < MAX_LIVE_BYTES, "{allocated:?}");
}

#[test]
fn rule_column_projection_rejects_invalid_qualification_order_and_bounds() {
    for indices in [[2, 1], [1, 1], [0, 4]] {
        let mut batch = batch(4);
        let mut selection = RuleIndexBuffer::new(1, batch.cancellation.clone()).unwrap();
        for index in indices {
            selection.push(index).unwrap();
        }
        batch.rules[0].matching_rows = selection.finish().unwrap();
        assert!(batch.matched_action_row_columns().is_err());
    }
}
