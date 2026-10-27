//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};
use uqa_sql::{
    explain::{ExplainFormat, ExplainSettings},
    plan::UnifiedPlan,
    result::ExplainPhysicalPlan,
};

fn description(relation: String) -> ExplainPhysicalPlan {
    let estimate = ExplainPlanEstimate {
        startup: 0.25,
        total: 2.5,
        rows: 3.0,
        width: Some(4),
    };
    ExplainPhysicalPlan {
        estimate: Some(estimate),
        estimates: vec![
            ExplainNodeEstimate {
                parent: None,
                kind: "Select",
                relation: None,
                estimate,
            },
            ExplainNodeEstimate {
                parent: Some(0),
                kind: "Relation",
                relation: Some(relation),
                estimate: ExplainPlanEstimate {
                    width: None,
                    ..estimate
                },
            },
        ],
        ..ExplainPhysicalPlan::default()
    }
}

#[test]
fn node_estimates_keep_parentage_numeric_fields_and_unknown_width_in_every_format() {
    let body = UnifiedPlan::lower(uqa_sql::compile("SELECT 1").unwrap().remove(0));
    let physical = description("<quoted\"한글>".into());
    for costs in [false, true] {
        for format in [
            ExplainFormat::Text,
            ExplainFormat::Json,
            ExplainFormat::Xml,
            ExplainFormat::Yaml,
        ] {
            let settings = ExplainSettings {
                format,
                costs,
                ..ExplainSettings::default()
            };
            let result =
                crate::explain::run_explain_with_physical(&body, &settings, None, &physical)
                    .unwrap();
            match result.value_at(0, 0).unwrap() {
                Value::Json(text) => {
                    let value: serde_json::Value = serde_json::from_str(text).unwrap();
                    assert_eq!(value[0].get("Node Estimates").is_some(), costs);
                    if costs {
                        let nodes = &value[0]["Node Estimates"];
                        assert_eq!(nodes[0]["Node ID"], 0);
                        assert!(nodes[0].get("Parent Node ID").is_none());
                        assert_eq!(nodes[1]["Node ID"], 1);
                        assert_eq!(nodes[1]["Parent Node ID"], 0);
                        assert_eq!(nodes[1]["Relation Name"], "<quoted\"한글>");
                        assert_eq!(nodes[1]["Total Cost"], 2.5);
                        assert_eq!(nodes[1]["Plan Rows"], 3.0);
                        assert_eq!(nodes[0]["Plan Width"], 4);
                        assert!(nodes[1].get("Plan Width").is_none());
                    }
                }
                Value::Str(text) if format == ExplainFormat::Xml => {
                    uqa_sql::xml::validate(text, uqa_sql::xml::XmlOption::Document).unwrap();
                    assert_eq!(text.contains("<Node-Estimates>"), costs);
                    assert_eq!(text.contains("&lt;quoted"), costs);
                }
                Value::Str(text) if format == ExplainFormat::Yaml => {
                    assert_eq!(text.contains("\"Node Estimates\""), costs);
                    assert_eq!(text.contains("\"Parent Node ID\": 0"), costs);
                }
                Value::Str(_) if format == ExplainFormat::Text => {
                    let lines = result
                        .rows
                        .iter()
                        .filter_map(|row| row.get("QUERY PLAN"))
                        .filter_map(|value| match value {
                            Value::Str(text) => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(
                        lines.iter().any(|line| line.contains("Node Estimates:")),
                        costs
                    );
                    assert_eq!(
                        lines.iter().any(|line| line
                            .contains("Relation \"<quoted\"\"한글>\" cost=0.25..2.50 rows=3")),
                        costs
                    );
                }
                other => panic!("unexpected {format:?}: {other:?}"),
            }
        }
    }
}

#[test]
fn node_estimate_rendering_admits_large_names_and_costs_false_skips_the_payload() {
    let body = UnifiedPlan::lower(uqa_sql::compile("SELECT 1").unwrap().remove(0));
    let physical = description("<&\"한글".repeat(256 * 1024));
    for format in [
        ExplainFormat::Text,
        ExplainFormat::Json,
        ExplainFormat::Xml,
        ExplainFormat::Yaml,
    ] {
        let memory = MemoryBudget::new(64 * 1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let mut settings = ExplainSettings {
            format,
            ..ExplainSettings::default()
        };
        let allocation = allocation_counter::measure(|| {
            let error = crate::explain::run_explain_with_control(
                &body, &settings, None, &physical, &control,
            )
            .unwrap_err();
            assert_eq!(error.sqlstate(), Some("53200"));
        });
        assert!(
            allocation.bytes_max < 256 * 1024,
            "{format:?}: {allocation:?}"
        );
        assert_eq!(memory.used(), 0);
        settings.costs = false;
        let result =
            crate::explain::run_explain_with_control(&body, &settings, None, &physical, &control)
                .unwrap();
        drop(result);
        assert_eq!(memory.used(), 0);
    }
}
