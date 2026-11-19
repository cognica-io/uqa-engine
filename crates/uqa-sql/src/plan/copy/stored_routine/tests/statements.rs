//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::work::inline;

fn body(source: &str) -> SQLRoutine {
    let sql =
        format!("CREATE FUNCTION saved() RETURNS integer LANGUAGE SQL BEGIN ATOMIC {source}; END");
    let Statement::CreateFunction(definition) = crate::compile(&sql).unwrap().remove(0) else {
        panic!("function")
    };
    SQLUserFunction::new(*definition, RoutineBody::Source).shared()
}

fn statements(definition: &StoredRoutineDefinition) -> &[Statement] {
    let FunctionBody::Statements(body) = &definition.declaration().body else {
        panic!("body")
    };
    body
}

fn capture_windows(
    source: SQLRoutine,
    control: &ProductionControl<'_>,
) -> (StoredRoutineDefinition, usize) {
    let mut stored = StoredRoutineDefinition::new(source, control).unwrap();
    let mut calls = 1;
    while !stored.copy_next_field(control).unwrap() {
        assert!(statements(&stored).is_empty());
        calls += 1;
    }
    assert_eq!(statements(&stored).len(), 1);
    (stored, calls)
}

#[test]
fn stored_routine_statements_copy_queries_and_dml_under_the_same_field_windows() {
    let statements = |width| {
        let rows = std::iter::repeat_n("(7)", width)
            .collect::<Vec<_>>()
            .join(",");
        let keys = std::iter::repeat_n("7", width)
            .collect::<Vec<_>>()
            .join(",");
        [
            format!("VALUES {rows}"),
            format!("WITH v(n) AS (VALUES {rows}) SELECT n FROM v"),
            format!("INSERT INTO t(v) VALUES {rows} RETURNING v"),
            format!("UPDATE t SET v = 3 WHERE v IN ({keys}) RETURNING v"),
            format!("DELETE FROM t WHERE v IN ({keys}) RETURNING v"),
            format!("MERGE INTO t USING (VALUES {rows}) s(v) ON t.v=s.v WHEN MATCHED THEN DELETE RETURNING t.v"),
        ]
    };
    let token = CancellationToken::new();
    for (short, wide) in statements(2).into_iter().zip(statements(1024)) {
        let mut data = Vec::new();
        for (query, limit) in [(short, 16 << 10), (wide, 32 << 20)] {
            let memory = MemoryBudget::new(limit);
            let control = ProductionControl::new(&memory, &token, &token);
            let source = body(&query);
            let (mut stored, initial) = capture_windows(source.clone(), &control);
            let initial_peak = memory.peak();
            let mut reading = Checkpoints::default();
            let edit = inline(stored.statement_edit_from(0, &control, &mut reading))
                .unwrap()
                .unwrap();
            let mut writing = Checkpoints::default();
            assert!(inline(edit.finish_from(true, &control, &mut writing)).unwrap());
            let reference = MemoryBudget::new(32 << 20);
            let (expected, descendants) = capture_stored_with_footprint(
                &source.def,
                &ProductionControl::new(&reference, &token, &token),
            )
            .unwrap();
            let (actual, children) = stored.into_definition(&control).unwrap();
            assert_eq!(
                serde_json::to_value(&*actual).unwrap(),
                serde_json::to_value(&*expected).unwrap()
            );
            assert_eq!(
                (actual.reserved_bytes(), children),
                (expected.reserved_bytes(), descendants)
            );
            println!(
                "{} {} initial={initial} initial_peak={initial_peak} edit={} publish={} retained={} peak={}",
                query.split_ascii_whitespace().next().unwrap(),
                query.len(),
                reading.0,
                writing.0,
                actual.reserved_bytes() + children,
                memory.peak()
            );
            data.push((initial, reading.0, writing.0));
            drop((actual, expected));
            assert_eq!((memory.used(), reference.used()), (0, 0));
        }
        let (short, wide) = (data[0], data[1]);
        assert_eq!(short, (1, 0, 0));
        assert!(
            wide.0 >= short.0 + 31 && wide.1 >= short.1 + 1010 && wide.2 >= short.2 + 1010,
            "short {short:?}, wide {wide:?}"
        );
    }
}

#[test]
fn stored_statement_edits_preserve_prior_changes_and_release_unpublished_fields() {
    let values = std::iter::repeat_n("(7)", 4096)
        .collect::<Vec<_>>()
        .join(",");
    let source = body(&format!("VALUES {values}"));
    let memory = MemoryBudget::new(32 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let mut pending = StoredRoutineDefinition::new(source.clone(), &control).unwrap();
    let initial = memory.used();
    assert!(!pending.copy_next_field(&control).unwrap());
    assert!(statements(&pending).is_empty());
    assert!(memory.used() > initial);
    let stopped = CancellationToken::new();
    stopped.cancel();
    assert_eq!(
        pending
            .copy_next_field(&ProductionControl::new(&memory, &stopped, &invoking))
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert!(statements(&pending).is_empty());
    assert_eq!(memory.used(), initial);
    drop(pending);
    assert_eq!(memory.used(), 0);

    let (mut stored, _) = capture_windows(source, &control);
    let alternate = || crate::compile("SELECT 81").unwrap().remove(0);
    assert!(inline(stored.statement_edit_from(1, &control, &mut ()))
        .unwrap()
        .is_none());
    let held = memory.used();
    for mode in 0..4 {
        let mut edit = inline(stored.statement_edit_from(0, &control, &mut ()))
            .unwrap()
            .unwrap();
        *edit.value_mut() = alternate();
        match mode {
            0 => drop(edit),
            1 => assert!(!inline(edit.finish_from(false, &control, &mut ())).unwrap()),
            2 => {
                let denied = memory.reserve(memory.available()).unwrap();
                assert_eq!(
                    inline(edit.finish_from(true, &control, &mut ()))
                        .unwrap_err()
                        .sqlstate(),
                    Some("53200")
                );
                drop(denied);
            }
            _ => {
                let cancelled = ProductionControl::new(&memory, &original, &stopped);
                assert_eq!(
                    inline(edit.finish_from(true, &cancelled, &mut ()))
                        .unwrap_err()
                        .sqlstate(),
                    Some("57014")
                );
            }
        }
        assert_ne!(
            serde_json::to_value(&statements(&stored)[0]).unwrap(),
            serde_json::to_value(alternate()).unwrap()
        );
        assert_eq!(memory.used(), held);
    }
    let denied = memory.reserve(memory.available()).unwrap();
    assert_eq!(
        inline(stored.statement_edit_from(0, &control, &mut ()))
            .err()
            .unwrap()
            .sqlstate(),
        Some("53200")
    );
    drop(denied);
    assert_eq!(memory.used(), held);
    stored
        .edit_statement(0, &control, |value| {
            *value = alternate();
            Ok(true)
        })
        .unwrap();
    let mut edit = inline(stored.statement_edit_from(0, &control, &mut ()))
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(edit.value_mut()).unwrap(),
        serde_json::to_value(alternate()).unwrap()
    );
    *edit.value_mut() = crate::compile("SELECT 74").unwrap().remove(0);
    assert!(inline(edit.finish_from(true, &control, &mut ())).unwrap());
    assert_eq!(
        serde_json::to_value(&statements(&stored)[0]).unwrap(),
        serde_json::to_value(crate::compile("SELECT 74").unwrap().remove(0)).unwrap()
    );
    assert!(!stored.statements[0].has_merge_targets);
    drop(stored);
    assert_eq!(memory.used(), 0);
}

#[test]
fn stored_statement_edits_classify_the_current_text_on_every_copy() {
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(4 << 20);
    let control = ProductionControl::new(&memory, &token, &token);
    let (mut stored, calls) = capture_windows(body("SELECT 'small'"), &control);
    assert_eq!(calls, 1);
    let baseline = memory.used();
    let large = format!("SELECT '{}'", "자료🙂".repeat(13108));
    let parse: fn(&str) -> Statement = |sql| crate::compile(sql).unwrap().remove(0);
    let pair = "界".repeat(800);
    let combined = parse(&format!("SELECT '{pair}', '{pair}'"));
    assert!(
        crate::plan::copy::syntax::native::Field::capture_inline(&combined, &control)
            .unwrap()
            .is_none()
    );
    for cancel in [true, false] {
        let stop = CancellationToken::new();
        let production = ProductionControl::new(&memory, &token, &stop);
        let mut work = Checkpoints::default();
        let mut edit = inline(stored.statement_edit_from(0, &production, &mut work))
            .unwrap()
            .unwrap();
        assert_eq!(work.0, 0);
        *edit.value_mut() = parse(&large);
        if cancel {
            work.1 = Some((6, stop.clone()));
        }
        let result = inline(edit.finish_from(true, &production, &mut work));
        if cancel {
            assert_eq!(result.unwrap_err().sqlstate(), Some("57014"));
            assert_eq!(memory.used(), baseline);
        } else {
            assert!(result.unwrap());
            assert!(work.0 > 30);
        }
    }
    let mut work = Checkpoints::default();
    let mut edit = inline(stored.statement_edit_from(0, &control, &mut work))
        .unwrap()
        .unwrap();
    assert!(work.0 > 30);
    *edit.value_mut() = parse("SELECT 'small'");
    let mut publish = Checkpoints::default();
    assert!(inline(edit.finish_from(true, &control, &mut publish)).unwrap());
    assert_eq!(publish.0, 0);
    assert_eq!(memory.used(), baseline);
    drop(stored);
    assert_eq!(memory.used(), 0);
}
