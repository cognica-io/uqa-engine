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
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

struct Selection {
    calls: usize,
    selected: usize,
    most_in_one_call: usize,
    names: Result<Vec<String>, SQLError>,
}

struct WrittenFields<'a> {
    fields: std::slice::Iter<'a, crate::plan::ProjectionPlan>,
    observed: &'a AtomicUsize,
}

impl<'a> Iterator for WrittenFields<'a> {
    type Item = &'a crate::plan::ProjectionPlan;

    fn next(&mut self) -> Option<Self::Item> {
        let field = self.fields.next()?;
        self.observed.fetch_add(1, Ordering::Relaxed);
        Some(field)
    }
}

enum OutputInput {
    Written,
    Block,
}

fn select(sql: &str, steps: usize, cancel: Option<bool>, allowance: usize) -> Selection {
    observe(sql, steps, cancel, allowance, OutputInput::Written)
}

fn observe(
    sql: &str,
    steps: usize,
    cancel: Option<bool>,
    allowance: usize,
    input: OutputInput,
) -> Selection {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("expected SELECT");
    };
    let crate::plan::RelationalPlan::QueryBlock(block) = query.root else {
        panic!("expected query block");
    };
    let native = MemoryBudget::new(allowance);
    let retained = native.clone();
    let work = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let captured = original.clone();
    let invoking = CancellationToken::new();
    let selected = Arc::new(AtomicUsize::new(0));
    let observed = selected.clone();
    let mut decoder = ImageDecoder::new(&work, &invoking, |mut reader| async move {
        let mut channel = reader.input();
        let control = ProductionControl::new(&retained, &captured, channel.cancellation);
        let context = crate::binding::fixture::empty_binding_context();
        let mut scope =
            SchemaScope::for_analysis_with_control(context.copy_with_control(&control)?, &control)?;
        let mut analysis = Preparation {
            bound_condition_queries: None,
            routines: &NoRoutines,
            scope: &mut scope,
            parameters: ParameterTypes::fixed_with_control(&[], &control)?,
            schema_expression: (),
        };
        let row = RowSchema::with_column_metadata_with_control(
            Some("t"),
            [
                ("v", Some(&ColumnType::Integer), None),
                ("x", Some(&ColumnType::Json), None),
            ],
            &control,
        )?;
        let output = match input {
            OutputInput::Written => {
                let projections = WrittenFields {
                    fields: block.projections.iter(),
                    observed: &observed,
                };
                analysis
                    .projections_with_work(projections, &row, &row, &block.subqueries, &mut channel)
                    .await?
            }
            OutputInput::Block => {
                analysis
                    .block_with_source(&block, &row, None, false, &mut channel)
                    .await?
            }
        };
        Ok::<_, SQLError>(
            output
                .columns
                .iter()
                .map(|name| name.as_str().to_owned())
                .collect(),
        )
    })
    .unwrap();
    let mut calls = 0;
    let mut most_in_one_call = 0;
    let names = loop {
        calls += 1;
        let before = selected.load(Ordering::Relaxed);
        if before >= 3 {
            match cancel {
                Some(true) => original.cancel(),
                Some(false) => invoking.cancel(),
                None => (),
            }
        }
        let result = decoder.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps });
        most_in_one_call = most_in_one_call.max(selected.load(Ordering::Relaxed) - before);
        match result {
            Ok(Some(output)) => break Ok(output),
            Ok(None) => (),
            Err(error) => break Err(error),
        }
    };
    drop(decoder);
    assert_eq!(native.used(), 0);
    assert_eq!(work.used(), 0);
    Selection {
        calls,
        selected: selected.load(Ordering::Relaxed),
        most_in_one_call,
        names,
    }
}

#[test]
fn stored_projection_selection_bounds_written_fields_and_preserves_star_and_error_order() {
    let columns = |count, invalid| {
        let fields = (0..count)
            .map(|index| {
                if Some(index) == invalid {
                    "missing.*".to_owned()
                } else {
                    format!("v AS p{index}")
                }
            })
            .collect::<Vec<_>>();
        format!("SELECT {} FROM t", fields.join(","))
    };
    let short = select(&columns(2, None), 1, None, 1 << 20);
    let sql = columns(32, None);
    let wide = select(&sql, 1, None, 1 << 20);
    let batched = select(&sql, 32, None, 1 << 20);
    assert_eq!(short.names.unwrap(), ["p0", "p1"]);
    assert_eq!(wide.selected, 32);
    assert_eq!(wide.most_in_one_call, 1);
    assert!(wide.calls >= short.calls + 30);
    assert!(batched.calls < wide.calls && batched.most_in_one_call <= 32);
    let expected = (0..32).map(|index| format!("p{index}")).collect::<Vec<_>>();
    assert_eq!(wide.names.unwrap(), expected);
    assert_eq!(batched.names.unwrap(), expected);
    let fields = select("SELECT *, t.*, v AS named FROM t", 1, None, 1 << 20);
    assert_eq!(fields.names.unwrap(), ["v", "x", "v", "x", "named"]);
    let first = select(&columns(32, Some(0)), 1, None, 1 << 20);
    let last = select(&columns(32, Some(31)), 1, None, 1 << 20);
    assert_eq!((first.selected, last.selected), (1, 32));
    assert!(last.calls >= first.calls + 31);
    let first = first.names.unwrap_err();
    let last = last.names.unwrap_err();
    assert_eq!(first.sqlstate(), Some("42P01"));
    assert_eq!(first.to_string(), last.to_string());
    for original in [true, false] {
        let interrupted = select(&sql, 1, Some(original), 1 << 20);
        assert_eq!(interrupted.names.unwrap_err().sqlstate(), Some("57014"));
        assert_eq!(interrupted.selected, 3);
    }
    assert_eq!(
        select(&sql, 1, None, 1).names.unwrap_err().sqlstate(),
        Some("53200")
    );
}

#[test]
fn stored_output_keys_schedule_ordering_distinct_positions_and_equality_in_written_order() {
    const WIDTH: usize = 32;
    let cases = [
        (
            "ORDER BY",
            "42P10",
            "ORDER BY position 99 is not in select list",
        ),
        (
            "DISTINCT ON",
            "42P10",
            "DISTINCT ON position 99 is not in select list",
        ),
        (
            "DISTINCT",
            "42883",
            "could not identify an equality operator for type json",
        ),
    ];
    for (clause, code, message) in cases {
        let run = |invalid, steps| {
            let keys = (0..WIDTH)
                .map(|position| if position == invalid { "99" } else { "1" })
                .collect::<Vec<_>>()
                .join(",");
            let columns = (0..WIDTH)
                .map(|position| if position == invalid { "x" } else { "v" })
                .collect::<Vec<_>>()
                .join(",");
            let sql = match clause {
                "ORDER BY" => format!("SELECT v FROM t ORDER BY {keys}"),
                "DISTINCT ON" => format!("SELECT DISTINCT ON ({keys}) v FROM t"),
                _ => format!("SELECT DISTINCT {columns} FROM t"),
            };
            let selection = observe(&sql, steps, None, 1 << 20, OutputInput::Block);
            let error = selection.names.unwrap_err();
            assert_eq!(error.sqlstate(), Some(code), "{clause}");
            assert_eq!(error.to_string(), message, "{clause}");
            selection.calls
        };
        let first = run(0, 1);
        let last = run(WIDTH - 1, 1);
        let batch = run(WIDTH - 1, 32);
        assert!(
            last >= first + WIDTH - 1,
            "{clause}: {first} first, {last} last"
        );
        assert!(batch < last);
    }
}
