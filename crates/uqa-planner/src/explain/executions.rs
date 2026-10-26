//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Render observed physical instances without equating them with cost-tree identities.

use super::{
    buffers, measurements,
    properties::{array, Object, Property},
};
use std::fmt::Write as _;
use uqa_core::memory::{ProductionControl, ProductionString};
use uqa_sql::{explain::ExplainSettings, result::explain::ExplainNodeExecution, SQLError};

pub(super) fn structured<'a>(
    nodes: &'a [ExplainNodeExecution],
    settings: &ExplainSettings,
    io_timing: bool,
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
            measurements::properties(&mut record, node.metrics, settings.timing)?;
            if settings.buffers {
                buffers::properties(&mut record, node.io.temporary, io_timing)?;
            }
            record.finish()
        }),
    )
}

pub(super) fn append_text(
    text: &mut ProductionString<'_>,
    nodes: &[ExplainNodeExecution],
    settings: &ExplainSettings,
) -> Result<(), SQLError> {
    if nodes.is_empty() {
        return Ok(());
    }
    text.push_str("\nNode Executions:")?;
    for (index, node) in nodes.iter().enumerate() {
        let _ = write!(text, "\n  [{index}] {}", node.kind);
        measurements::append_text(text, node.metrics, settings.timing)?;
        if settings.buffers {
            buffers::append_text(text, node.io.temporary, "    ")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use uqa_sql::{explain::ExplainFormat, result::explain::ExplainNodeMetrics};

    #[test]
    fn node_buffers_keep_operation_totals_and_independent_clock_visibility() {
        let nodes = [
            ExplainNodeExecution {
                kind: "Spilled Scan",
                io: uqa_core::io_usage::IOUsage {
                    temporary: uqa_core::io_usage::BlockUsage {
                        reads: 7,
                        written: 11,
                        read_time: Duration::from_millis(2),
                        write_time: Duration::from_millis(3),
                        ..uqa_core::io_usage::BlockUsage::default()
                    },
                    ..uqa_core::io_usage::IOUsage::default()
                },
                metrics: ExplainNodeMetrics {
                    rows: 9,
                    loops: 3,
                    ..ExplainNodeMetrics::default()
                },
            },
            ExplainNodeExecution {
                kind: "Empty Scan",
                io: uqa_core::io_usage::IOUsage::default(),
                metrics: ExplainNodeMetrics::default(),
            },
        ];
        let control = ProductionControl::uncontrolled();
        for buffers in [false, true] {
            for timing in [false, true] {
                let settings = ExplainSettings {
                    buffers,
                    timing,
                    ..ExplainSettings::default()
                };
                for io_timing in [false, true] {
                    let property = structured(&nodes, &settings, io_timing, control).unwrap();
                    for format in [ExplainFormat::Json, ExplainFormat::Xml, ExplainFormat::Yaml] {
                        let rendered =
                            super::super::formats::render(&property, format, None, control)
                                .unwrap();
                        let (blocks, clock) = if format == ExplainFormat::Xml {
                            ("Temp-Read-Blocks", "Temp-I-O-Read-Time")
                        } else {
                            ("Temp Read Blocks", "Temp I/O Read Time")
                        };
                        assert_eq!(rendered.contains(blocks), buffers, "{rendered:?}");
                        assert_eq!(
                            rendered.contains(clock),
                            buffers && io_timing,
                            "{rendered:?}"
                        );
                        if format == ExplainFormat::Json && buffers {
                            let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
                            assert_eq!(value[0][0]["Temp Read Blocks"], 7);
                            assert_eq!(value[0][0]["Temp Written Blocks"], 11);
                            assert_eq!(value[0][1]["Temp Read Blocks"], 0);
                            if io_timing {
                                assert_eq!(value[0][0]["Temp I/O Read Time"], 2.0);
                            }
                        }
                    }
                    let mut text = ProductionString::new(control);
                    append_text(&mut text, &nodes, &settings).unwrap();
                    let text = text.finish().unwrap();
                    assert_eq!(
                        text.contains("\n    Buffers: temp read=7 written=11"),
                        buffers
                    );
                    assert_eq!(
                        text.contains("I/O Timings: temp read=2.000 write=3.000"),
                        buffers
                    );
                }
            }
        }
    }

    #[test]
    fn every_format_reports_the_same_instance_cycles_and_timing_visibility() {
        let nodes = [
            ExplainNodeExecution {
                kind: "Relation Scan",
                io: uqa_core::io_usage::IOUsage::default(),
                metrics: ExplainNodeMetrics {
                    rows: 7,
                    loops: 2,
                    startup: Duration::from_millis(4),
                    total: Duration::from_millis(12),
                },
            },
            ExplainNodeExecution {
                kind: "Unpulled Scan",
                io: uqa_core::io_usage::IOUsage::default(),
                metrics: ExplainNodeMetrics::default(),
            },
        ];
        let control = ProductionControl::uncontrolled();
        for timing in [false, true] {
            let settings = ExplainSettings {
                timing,
                ..ExplainSettings::default()
            };
            let property = structured(&nodes, &settings, false, control).unwrap();
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
            append_text(&mut text, &nodes, &settings).unwrap();
            let text = text.finish().unwrap();
            assert!(text.contains("[0] Relation Scan"));
            assert!(text.contains("[1] Unpulled Scan"));
            assert!(text.contains("never executed"));
            assert_eq!(text.contains("actual_time_ms="), timing);
        }
    }
}
