//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Render observed physical instances without equating them with cost-tree identities.

use super::{
    measurements,
    properties::{array, Object, Property},
};
use std::fmt::Write as _;
use uqa_core::memory::{ProductionControl, ProductionString};
use uqa_sql::{result::explain::ExplainNodeExecution, SQLError};

pub(super) fn structured<'a>(
    nodes: &'a [ExplainNodeExecution],
    timing: bool,
    control: ProductionControl<'_>,
) -> Result<Property<'a>, SQLError> {
    array(
        control,
        nodes.iter().enumerate().map(|(index, node)| {
            let mut record = Object::new(control);
            record.insert(
                "Execution ID",
                Property::Unsigned(u64::try_from(index).map_err(|_| {
                    SQLError::Internal("EXPLAIN execution index exceeds u64".into())
                })?),
            )?;
            record.insert("Node Type", Property::Text(node.kind))?;
            measurements::properties(&mut record, node.metrics, timing)?;
            record.finish()
        }),
    )
}

pub(super) fn append_text(
    text: &mut ProductionString<'_>,
    nodes: &[ExplainNodeExecution],
    timing: bool,
) -> Result<(), SQLError> {
    if nodes.is_empty() {
        return Ok(());
    }
    text.push_str("\nNode Executions:")?;
    for (index, node) in nodes.iter().enumerate() {
        let _ = write!(text, "\n  [{index}] {}", node.kind);
        measurements::append_text(text, node.metrics, timing)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use uqa_sql::{explain::ExplainFormat, result::explain::ExplainNodeMetrics};

    #[test]
    fn every_format_reports_the_same_instance_cycles_and_timing_visibility() {
        let nodes = [
            ExplainNodeExecution {
                kind: "Relation Scan",
                metrics: ExplainNodeMetrics {
                    rows: 7,
                    loops: 2,
                    startup: Duration::from_millis(4),
                    total: Duration::from_millis(12),
                },
            },
            ExplainNodeExecution {
                kind: "Unpulled Scan",
                metrics: ExplainNodeMetrics::default(),
            },
        ];
        let control = ProductionControl::uncontrolled();
        for timing in [false, true] {
            let property = structured(&nodes, timing, control).unwrap();
            for format in [ExplainFormat::Json, ExplainFormat::Xml, ExplainFormat::Yaml] {
                let rendered =
                    super::super::formats::render(&property, format, None, control).unwrap();
                let label = if format == ExplainFormat::Xml {
                    "Actual-Total-Time"
                } else {
                    "Actual Total Time"
                };
                assert_eq!(
                    rendered.contains(label),
                    timing,
                    "{format:?}: {}",
                    rendered.as_str()
                );
                if format == ExplainFormat::Json {
                    let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
                    let records = &value[0];
                    assert_eq!(records[0]["Execution ID"], 0);
                    assert_eq!(records[0]["Actual Rows"], 3.5);
                    assert_eq!(records[0]["Actual Loops"], 2);
                    assert_eq!(records[1]["Actual Loops"], 0);
                    if timing {
                        assert_eq!(records[0]["Actual Total Time"], 6.0);
                    }
                }
            }
            let mut text = ProductionString::new(control);
            append_text(&mut text, &nodes, timing).unwrap();
            let text = text.finish().unwrap();
            assert!(text.contains("[0] Relation Scan"));
            assert!(text.contains("[1] Unpulled Scan"));
            assert!(text.contains("never executed"));
            assert_eq!(text.contains("actual_time_ms="), timing);
        }
    }
}
