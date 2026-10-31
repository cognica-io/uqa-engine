//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_sql::ColumnType;

struct Input {
    schema: RowSchema,
    closes: Arc<AtomicUsize>,
}
impl PhysicalOperator for Input {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }
    fn open(&mut self) -> ExecResult<()> {
        panic!("construction cannot open inputs")
    }
    fn next(&mut self) -> ExecResult<Option<Batch>> {
        panic!("construction cannot pull rows")
    }
    fn close(&mut self) -> ExecResult<()> {
        self.closes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

#[test]
fn join_output_and_alias_construction_close_owned_inputs_on_rejection_and_cancellation() {
    let schema = RowSchema::with_types(vec!["id".into()], vec![Some(ColumnType::Integer)]);
    let wide = "x".repeat(1 << 20);
    let columns = vec![(
        wide.clone(),
        ColumnIdentity::unqualified(&wide),
        JoinOutputSource::Input(0),
    )];
    for alias in [false, true] {
        for failure in 0..3 {
            let memory = MemoryBudget::new(32 << 10);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&memory, &original, &invoking);
            let closes = Arc::new(AtomicUsize::new(0));
            let mut input = Some(Box::new(Input {
                schema: schema.clone(),
                closes: closes.clone(),
            }) as Box<dyn PhysicalOperator>);
            if failure == 1 {
                original.cancel();
            }
            if failure == 2 {
                invoking.cancel();
            }
            let mut error = None;
            let measured = allocation_counter::measure(|| {
                error = Some(if alias {
                    crate::query::source_projection::alias_join_operator_with_control(
                        input.take().unwrap(),
                        Some(&wide),
                        &[],
                        &control,
                    )
                    .err()
                    .unwrap()
                } else {
                    crate::physical::physical_exec_error(
                        JoinOutput::with_control(input.take().unwrap(), &columns, &[], &control)
                            .err()
                            .unwrap(),
                    )
                });
            });
            assert_eq!(
                error.unwrap().sqlstate(),
                Some(if failure == 0 { "53200" } else { "57014" })
            );
            assert!(measured.bytes_total < 128 << 10, "{measured:?}");
            assert_eq!(closes.load(Ordering::Relaxed), 1);
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn join_output_wrapper_admission_preserves_cleanup_and_releases_computed_owners() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let closes = Arc::new(AtomicUsize::new(0));
    let output = JoinOutput::with_control(
        Box::new(Input {
            schema: RowSchema::with_types(vec!["id".into()], vec![Some(ColumnType::Integer)]),
            closes: closes.clone(),
        }),
        &[(
            "id".into(),
            ColumnIdentity::unqualified("id"),
            JoinOutputSource::Cast {
                input: 0,
                ty: ColumnType::BigInteger,
            },
        )],
        &[],
        &control,
    )
    .unwrap();
    assert!(memory.used() > 0);
    let occupied = memory.reserve(memory.available()).unwrap();
    let error = output.into_boxed(&control).err().unwrap();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(closes.load(Ordering::Relaxed), 1);
    assert_eq!(memory.used(), occupied.bytes());
    drop(occupied);
    assert_eq!(memory.used(), 0);
}

#[test]
fn join_output_batches_retain_schema_owners_and_preserve_cast_order() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let schema = RowSchema::with_types(
        vec!["l".into(), "r".into()],
        vec![Some(ColumnType::Text); 2],
    );
    let columns = [(
        "id".into(),
        ColumnIdentity::unqualified("id"),
        JoinOutputSource::Coalesce {
            left: 0,
            right: 1,
            ty: ColumnType::Integer,
        },
    )];
    let source = crate::TableScan::from_physical_rows(
        schema.clone(),
        vec![crate::PhysicalRow::from_values(vec![
            Value::Str("7".into()),
            Value::Str("bad".into()),
        ])],
    );
    let mut output = JoinOutput::with_control(Box::new(source), &columns, &[], &control)
        .unwrap()
        .into_boxed(&control)
        .unwrap();
    output.open().unwrap();
    let batch = output.next().unwrap().unwrap();
    assert_eq!(
        batch.schema.view(&batch.rows[0]).value_at(0),
        Some(&Value::Int(7))
    );
    output.close().unwrap();
    drop(output);
    assert!(memory.used() > 0);
    drop(batch);
    assert_eq!(memory.used(), 0);
    for values in [
        vec![Value::Str("bad".into()), Value::Str("7".into())],
        vec![Value::Null, Value::Str("bad".into())],
    ] {
        let source = crate::TableScan::from_physical_rows(
            schema.clone(),
            vec![crate::PhysicalRow::from_values(values)],
        );
        let mut output =
            JoinOutput::with_control(Box::new(source), &columns, &[], &control).unwrap();
        output.open().unwrap();
        assert_eq!(
            crate::physical::physical_exec_error(output.next().unwrap_err()).sqlstate(),
            Some("22P02")
        );
        output.close().unwrap();
        drop(output);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn fresh_join_aliases_establish_relation_order_without_copying_repeated_payloads() {
    let schema = RowSchema::from_physical_layout(uqa_sql::schema::PhysicalLayout {
        columns: ["a", "b", "c", "d", "e"].map(String::from).into(),
        identities: ["a", "b", "c", "d", "e"]
            .map(ColumnIdentity::unqualified)
            .into(),
        types: vec![None; 5],
        slots: vec![Some(2), Some(0), Some(2), None, Some(1)],
        physical_width: 3,
        aliases: vec![],
        internal: vec![],
        score_sources: vec![],
        wildcard_hidden: std::collections::HashSet::new(),
    })
    .unwrap();
    for controlled in [false, true] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let row = crate::PhysicalRow::from_values(vec![
            Value::Int(11),
            Value::Int(22),
            Value::Str("retained".into()),
        ]);
        let source = Box::new(crate::TableScan::from_physical_rows(
            schema.clone(),
            vec![row],
        ));
        let mut output: Box<dyn PhysicalOperator> = if controlled {
            crate::query::source_projection::alias_join_operator_with_control(
                source,
                Some("j"),
                &[],
                &control,
            )
            .unwrap()
        } else {
            Box::new(crate::ColumnSelection::with_fresh_identities(
                source,
                schema
                    .columns()
                    .iter()
                    .enumerate()
                    .map(|(position, name)| {
                        (name.clone(), ColumnIdentity::qualified("j", name), position)
                    })
                    .collect(),
            ))
        };
        output.open().unwrap();
        let batch = output.next().unwrap().unwrap();
        assert!(batch.schema.is_identity_layout());
        let layout = batch.schema.qualified_star_position_layout("j");
        assert_eq!(
            layout
                .iter()
                .map(|(name, ..)| name.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c", "d", "e"]
        );
        let row = batch.schema.view(&batch.rows[0]);
        for (position, expected) in [
            Value::Str("retained".into()),
            Value::Int(11),
            Value::Str("retained".into()),
            Value::Null,
            Value::Int(22),
        ]
        .iter()
        .enumerate()
        {
            assert_eq!(row.value_at(position), Some(expected));
        }
        let (Some(Value::Str(first)), Some(Value::Str(second))) =
            (row.value_at(0), row.value_at(2))
        else {
            panic!("text attributes");
        };
        assert_eq!(first.as_ptr(), second.as_ptr());
        output.close().unwrap();
        drop(output);
        assert_eq!(memory.used() > 0, controlled);
        drop(batch);
        assert_eq!(memory.used(), 0);
    }
}
