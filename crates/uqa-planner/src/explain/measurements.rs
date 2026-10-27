//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Render measurements from executed native boundaries independently of summary timing.

use super::properties::{Object, Property};
use std::fmt::Write as _;
use uqa_core::memory::ProductionString;
use uqa_sql::{result::explain::ExplainNodeMetrics, SQLError};

pub(super) fn properties(
    plan: &mut Object<'_, '_>,
    metrics: ExplainNodeMetrics,
    timing: bool,
) -> Result<(), SQLError> {
    let rows = if metrics.loops == 0 {
        Property::Unsigned(0)
    } else if metrics.rows.is_multiple_of(metrics.loops) {
        Property::Unsigned(metrics.rows / metrics.loops)
    } else {
        Property::Float(metrics.rows as f64 / metrics.loops as f64)
    };
    plan.insert("Actual Rows", rows)?;
    plan.insert("Actual Loops", Property::Unsigned(metrics.loops))?;
    if timing {
        plan.insert(
            "Actual Startup Time",
            Property::Float(milliseconds(metrics.startup, metrics.loops)),
        )?;
        plan.insert(
            "Actual Total Time",
            Property::Float(milliseconds(metrics.total, metrics.loops)),
        )?;
    }
    Ok(())
}

pub(super) fn append_text(
    text: &mut ProductionString<'_>,
    metrics: ExplainNodeMetrics,
    timing: bool,
) -> Result<(), SQLError> {
    if metrics.loops == 0 {
        text.push_str("\n  (never executed)")?;
    }
    let rows = if metrics.loops == 0 {
        0.0
    } else {
        metrics.rows as f64 / metrics.loops as f64
    };
    let _ = write!(
        text,
        "\n  actual_rows={rows:.2}\n  actual_loops={}",
        metrics.loops
    );
    if timing {
        let _ = write!(
            text,
            "\n  actual_time_ms={:.3}..{:.3}",
            milliseconds(metrics.startup, metrics.loops),
            milliseconds(metrics.total, metrics.loops)
        );
    }
    Ok(())
}

fn milliseconds(elapsed: std::time::Duration, loops: u64) -> f64 {
    if loops == 0 {
        0.0
    } else {
        elapsed.as_secs_f64() * 1_000.0 / loops as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::memory::ProductionControl;
    use uqa_sql::explain::ExplainFormat;

    #[test]
    fn node_fields_average_completed_loops_and_obey_timing_in_every_format() {
        let control = ProductionControl::uncontrolled();
        for timing in [false, true] {
            for loops in [0, 2] {
                let metrics = ExplainNodeMetrics {
                    rows: 3,
                    loops,
                    startup: std::time::Duration::from_millis(4),
                    total: std::time::Duration::from_millis(10),
                };
                let mut object = Object::new(control);
                properties(&mut object, metrics, timing).unwrap();
                let object = object.finish().unwrap();
                for format in [ExplainFormat::Json, ExplainFormat::Xml, ExplainFormat::Yaml] {
                    let text =
                        super::super::formats::render(&object, format, None, control).unwrap();
                    let key = if format == ExplainFormat::Xml {
                        "Actual-Startup-Time"
                    } else {
                        "Actual Startup Time"
                    };
                    assert_eq!(text.contains(key), timing, "{format:?}: {}", text.as_str());
                    if format == ExplainFormat::Json {
                        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
                        assert_eq!(value[0]["Actual Loops"], loops);
                        assert_eq!(
                            value[0]["Actual Rows"].as_f64(),
                            Some(if loops == 0 { 0.0 } else { 1.5 })
                        );
                        if timing {
                            assert_eq!(
                                value[0]["Actual Startup Time"].as_f64(),
                                Some(if loops == 0 { 0.0 } else { 2.0 })
                            );
                            assert_eq!(
                                value[0]["Actual Total Time"].as_f64(),
                                Some(if loops == 0 { 0.0 } else { 5.0 })
                            );
                        }
                    }
                }
                let mut text = ProductionString::new(control);
                append_text(&mut text, metrics, timing).unwrap();
                let text = text.finish().unwrap();
                assert_eq!(text.contains("actual_time_ms="), timing);
                assert_eq!(text.contains("never executed"), loops == 0);
                assert!(text.contains(&format!("actual_loops={loops}")));
                assert!(text.contains(if loops == 0 {
                    "actual_rows=0.00"
                } else {
                    "actual_rows=1.50"
                }));
            }
        }
    }
}
