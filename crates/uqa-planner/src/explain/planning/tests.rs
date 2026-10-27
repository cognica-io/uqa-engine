//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::time::Duration;
use uqa_core::{
    io_usage::{BlockUsage, IOUsage},
    memory::{BudgetedVec, MemoryBudget},
    Value,
};
use uqa_sql::{
    explain::{ExplainFormat, ExplainSerialization},
    plan::UnifiedPlan,
    result::explain::{ExplainNodeMetrics, ExplainPlanningMetrics, ExplainSerializationMetrics},
};

fn usage(reads: u64) -> IOUsage {
    IOUsage {
        temporary: BlockUsage {
            reads,
            read_time: Duration::from_millis(reads),
            ..BlockUsage::default()
        },
        ..IOUsage::default()
    }
}

#[test]
fn planning_and_serialization_formats_keep_separate_totals_and_final_clock_visibility() {
    let body = UnifiedPlan::lower(uqa_sql::compile("SELECT 1").unwrap().remove(0));
    let budget = MemoryBudget::new(65_536);
    for io_timing in [false, true] {
        let physical = ExplainPhysicalPlan {
            planning: Some(ExplainPlanningMetrics {
                io: usage(3),
                io_timing: !io_timing,
            }),
            planning_elapsed: Some(Duration::from_millis(7)),
            ..ExplainPhysicalPlan::default()
        };
        let analysis = ExplainAnalysis {
            io: usage(11),
            io_timing,
            vector_searches: BudgetedVec::new(&budget),
            node_executions: BudgetedVec::new(&budget),
            elapsed: Duration::from_millis(15),
            root: ExplainNodeMetrics {
                loops: 2,
                ..ExplainNodeMetrics::default()
            },
            affected_rows: 0,
            serialization: ExplainSerializationMetrics {
                bytes: 2048,
                elapsed: Duration::from_millis(7),
                io: usage(5),
            },
        };
        for buffers in [false, true] {
            for format in [
                ExplainFormat::Text,
                ExplainFormat::Json,
                ExplainFormat::Xml,
                ExplainFormat::Yaml,
            ] {
                let settings = ExplainSettings {
                    analyze: true,
                    buffers,
                    timing: false,
                    summary: false,
                    serialize: ExplainSerialization::Text,
                    format,
                    ..ExplainSettings::default()
                };
                let result = crate::explain::run_explain_with_physical(
                    &body,
                    &settings,
                    Some(&analysis),
                    &physical,
                )
                .unwrap();
                let output = (0..result.rows.len())
                    .map(|i| match result.value_at(i, 0).unwrap() {
                        Value::Str(text) | Value::Json(text) => text.as_str(),
                        other => panic!("{other:?}"),
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if format == ExplainFormat::Text {
                    assert_eq!(
                        output.contains("Planning:\n  Buffers: temp read=3"),
                        buffers,
                        "{output}"
                    );
                    assert_eq!(
                        output.contains(
                            "Serialization: output=2kB  format=text\n  Buffers: temp read=5"
                        ),
                        buffers,
                        "{output}"
                    );
                    assert_eq!(
                        output.contains("I/O Timings: temp read=3.000"),
                        buffers,
                        "{output}"
                    );
                    assert!(!output.contains("Planning Time:"), "{output}");
                } else {
                    let field = if format == ExplainFormat::Xml {
                        "Temp-I-O-Read-Time"
                    } else {
                        "Temp I/O Read Time"
                    };
                    assert_eq!(output.contains(field), buffers && io_timing, "{output}");
                    if format == ExplainFormat::Json {
                        let doc: serde_json::Value = serde_json::from_str(&output).unwrap();
                        assert_eq!(doc[0].get("Planning").is_some(), buffers, "{output}");
                        if buffers {
                            assert_eq!(doc[0]["Planning"]["Temp Read Blocks"], 3);
                            assert_eq!(doc[0]["Plan"]["Temp Read Blocks"], 11);
                            assert_eq!(doc[0]["Serialization"]["Temp Read Blocks"], 5);
                        }
                        assert!(doc[0].get("Planning Time").is_none());
                        assert!(doc[0]["Serialization"].get("Time").is_none());
                    }
                }
            }
        }
    }
}

#[test]
fn unexecuted_planning_uses_its_own_clock_and_text_omits_empty_groups() {
    let control = ProductionControl::uncontrolled();
    let settings = ExplainSettings {
        buffers: true,
        ..ExplainSettings::default()
    };
    for io_timing in [false, true] {
        let physical = ExplainPhysicalPlan {
            planning: Some(ExplainPlanningMetrics {
                io: IOUsage::default(),
                io_timing,
            }),
            ..ExplainPhysicalPlan::default()
        };
        let mut text = ProductionString::new(control);
        append_text(&mut text, &settings, &physical).unwrap();
        assert!(text.finish().unwrap().is_empty());
        let property = properties(&settings, &physical, None, control)
            .unwrap()
            .unwrap();
        let rendered =
            crate::explain::formats::render(&property, ExplainFormat::Json, None, control).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(doc[0]["Temp Read Blocks"], 0);
        assert_eq!(doc[0]["Temp Written Blocks"], 0);
        assert_eq!(doc[0].get("Temp I/O Read Time").is_some(), io_timing);
        let body = UnifiedPlan::lower(uqa_sql::compile("SELECT 1").unwrap().remove(0));
        let settings = ExplainSettings {
            format: ExplainFormat::Json,
            ..settings.clone()
        };
        let result =
            crate::explain::run_explain_with_physical(&body, &settings, None, &physical).unwrap();
        let Some(Value::Json(text)) = result.value_at(0, 0) else {
            panic!("JSON result")
        };
        let doc: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(doc[0]["Plan"]["Temp Read Blocks"], 0);
        assert_eq!(doc[0]["Plan"]["Temp Written Blocks"], 0);
        assert!(doc[0]["Plan"].get("Actual Loops").is_none());
        assert_eq!(
            doc[0]["Plan"].get("Temp I/O Read Time").is_some(),
            io_timing
        );
    }
}
