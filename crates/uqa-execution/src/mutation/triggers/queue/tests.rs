//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn row_event(id: i64) -> AfterRowTriggerEvent {
    AfterRowTriggerEvent {
        table: "public.events".into(),
        event: TriggerEvent::Update,
        old: Value::Record(
            vec![
                ("id".into(), Value::Int(id)),
                ("body".into(), Value::Str("o".repeat(16 * 1024))),
            ]
            .into(),
        ),
        new: Value::Record(
            vec![
                ("id".into(), Value::Int(id)),
                ("body".into(), Value::Str("n".repeat(16 * 1024))),
            ]
            .into(),
        ),
        captured: true,
        triggers: Vec::new(),
        foreign_keys: Vec::new(),
    }
}

#[test]
fn after_event_queues_bound_live_images_and_preserve_command_order() {
    let allocated = allocation_counter::measure(|| {
        let token = uqa_core::CancellationToken::new();
        let mut command = AfterRowEvents::default();
        for id in 0..512 {
            command
                .push(row_event(id), || Ok(16 * 1024), &token)
                .unwrap();
        }
        let mut state = QueueState::default();
        while let Some(event) = command.take_next(&token).unwrap() {
            state
                .entries
                .push(
                    &QueueEntry::Row { event, table: None },
                    || Ok(16 * 1024),
                    &token,
                )
                .unwrap();
        }
        for id in 0..512 {
            let Some(QueueEntry::Row { event, table }) = state.take_next(&token).unwrap() else {
                panic!("missing original event {id}");
            };
            let expected = row_event(id);
            assert!(event.old.has_same_representation(&expected.old));
            assert!(event.new.has_same_representation(&expected.new));
            assert!(table.is_none());
        }
        assert!(state.take_next(&token).unwrap().is_none());
    });
    // Both queue stages must retain a bounded prefix, not all 16MiB of row images.
    assert!(allocated.bytes_max < 1024 * 1024, "{allocated:?}");
}

fn table() -> TableState {
    TableState {
        relation: "public.events".into(),
        event: TriggerEvent::Update,
        sources: BTreeSet::new(),
        capture: TransitionCapture::default(),
        closed: false,
        before_fired: false,
        statement_position: None,
        old_rows: TransitionImages::default(),
        new_rows: TransitionImages::default(),
        transitions: None,
    }
}

#[test]
fn after_statement_replacement_skips_only_superseded_events_and_accepts_later_rows() {
    for bytes in [1, 1024 * 1024] {
        let token = uqa_core::CancellationToken::new();
        let mut state = QueueState::default();
        state.tables.extend([table(), table()]);
        let statement = StatementEvent::new("public.events", TriggerEvent::Update, &["id".into()]);
        for table in [0, 1, 0] {
            let position = state.entries.len();
            state
                .entries
                .push(
                    &QueueEntry::Statement {
                        statement: statement.clone(),
                        table,
                    },
                    || Ok(bytes),
                    &token,
                )
                .unwrap();
            state.tables[table].statement_position = Some(position);
            state
                .entries
                .push(
                    &QueueEntry::Row {
                        event: row_event(position as i64),
                        table: Some(table),
                    },
                    || Ok(bytes),
                    &token,
                )
                .unwrap();
        }
        for expected in [None, Some(1), None, Some(0), None] {
            let entry = state.take_next(&token).unwrap().unwrap();
            match (entry, expected) {
                (
                    QueueEntry::Statement {
                        table,
                        statement: actual,
                    },
                    Some(expected),
                ) => {
                    assert_eq!(table, expected);
                    assert_eq!(actual.columns, statement.columns);
                }
                (QueueEntry::Row { .. }, None) => {}
                _ => panic!("changed event order"),
            }
        }
        assert!(state.take_next(&token).unwrap().is_none());
        state
            .entries
            .push(
                &QueueEntry::Row {
                    event: row_event(99),
                    table: None,
                },
                || Ok(bytes),
                &token,
            )
            .unwrap();
        assert!(matches!(
            state.take_next(&token).unwrap(),
            Some(QueueEntry::Row { .. })
        ));
        assert!(state.take_next(&token).unwrap().is_none());
    }
}

#[test]
fn after_event_queue_rejects_invalid_relation_state() {
    let token = uqa_core::CancellationToken::new();
    for entry in [
        QueueEntry::Statement {
            statement: StatementEvent::new("public.events", TriggerEvent::Update, &[]),
            table: usize::MAX,
        },
        QueueEntry::Row {
            event: row_event(1),
            table: Some(0),
        },
    ] {
        let mut state = QueueState::default();
        state.entries.push(&entry, || Ok(1), &token).unwrap();
        assert!(state
            .take_next(&token)
            .err()
            .unwrap()
            .to_string()
            .contains("missing relation state"));
    }
}
