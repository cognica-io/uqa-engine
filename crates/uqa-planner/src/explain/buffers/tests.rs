//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::time::Duration;
use uqa_core::memory::{MemoryBudget, ProductionControl};
use uqa_sql::explain::ExplainFormat;

#[test]
fn temporary_fields_keep_totals_and_effective_timing_in_structured_formats() {
    let control = ProductionControl::uncontrolled();
    for usage in [
        BlockUsage::default(),
        BlockUsage {
            reads: 3,
            written: 7,
            read_time: Duration::from_millis(2),
            write_time: Duration::from_millis(5),
            ..BlockUsage::default()
        },
    ] {
        for timing in [false, true] {
            let mut plan = Object::new(control);
            properties(&mut plan, usage, timing).unwrap();
            let plan = plan.finish().unwrap();
            for format in [ExplainFormat::Json, ExplainFormat::Xml, ExplainFormat::Yaml] {
                let rendered =
                    crate::explain::formats::render(&plan, format, None, control).unwrap();
                let key = if format == ExplainFormat::Xml {
                    "Temp-I-O-Read-Time"
                } else {
                    "Temp I/O Read Time"
                };
                assert_eq!(rendered.contains(key), timing, "{format:?}: {rendered:?}");
                if format == ExplainFormat::Json {
                    let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
                    assert_eq!(value[0]["Temp Read Blocks"], usage.reads);
                    assert_eq!(value[0]["Temp Written Blocks"], usage.written);
                    if timing {
                        assert_eq!(
                            value[0]["Temp I/O Read Time"].as_f64(),
                            Some(usage.read_time.as_secs_f64() * 1_000.0)
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn text_omits_zero_values_and_keeps_already_measured_times() {
    let control = ProductionControl::uncontrolled();
    let mut text = ProductionString::new(control);
    append_text(&mut text, BlockUsage::default()).unwrap();
    assert!(text.finish().unwrap().is_empty());
    let mut text = ProductionString::new(control);
    append_text(
        &mut text,
        BlockUsage {
            written: 7,
            write_time: Duration::from_micros(1234),
            ..BlockUsage::default()
        },
    )
    .unwrap();
    assert_eq!(
        &*text.finish().unwrap(),
        "\n  Buffers: temp written=7\n  I/O Timings: temp write=1.234"
    );
}

#[test]
fn buffer_rendering_preserves_admission_and_cancellation() {
    let budget = MemoryBudget::new(0);
    let original = uqa_core::CancellationToken::new();
    let invoking = uqa_core::CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let mut plan = Object::new(control);
    assert_eq!(
        properties(&mut plan, BlockUsage::default(), true)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    drop(plan);
    assert_eq!(budget.used(), 0);
    for token in [&original, &invoking] {
        token.cancel();
        let mut plan = Object::new(control);
        assert_eq!(
            properties(&mut plan, BlockUsage::default(), true)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        token.reset();
    }
}
