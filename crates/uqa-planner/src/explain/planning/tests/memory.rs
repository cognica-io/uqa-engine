//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::CancellationToken;
use uqa_sql::result::explain::ExplainPlanningMemory;

fn physical(memory: Option<ExplainPlanningMemory>) -> ExplainPhysicalPlan {
    ExplainPhysicalPlan {
        planning: Some(ExplainPlanningMetrics {
            memory,
            ..ExplainPlanningMetrics::default()
        }),
        ..ExplainPhysicalPlan::default()
    }
}

fn render(
    settings: &ExplainSettings,
    physical: &ExplainPhysicalPlan,
    control: ProductionControl<'_>,
) -> Result<String, SQLError> {
    if settings.format == ExplainFormat::Text {
        let mut text = ProductionString::new(control);
        append_text(&mut text, settings, physical)?;
        Ok(text.finish()?.to_string())
    } else {
        let Some(property) = properties(settings, physical, None, control)? else {
            return Ok(String::new());
        };
        Ok(crate::explain::formats::render(&property, settings.format, None, control)?.to_string())
    }
}

#[test]
fn memory_fields_match_postgresql_option_visibility_in_every_format() {
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../../tests/parity/pg18/explain_memory.expected.json"
    ))
    .unwrap();
    let physical = physical(Some(ExplainPlanningMemory {
        used_bytes: 1025,
        allocated_bytes: 4096,
    }));
    let mut checked = 0;
    for case in reference["cases"].as_array().unwrap() {
        let Some(format) = case.get("format") else {
            continue;
        };
        if !case["sql"]
            .as_str()
            .unwrap()
            .ends_with("SELECT nextval('planning_memory_effect')")
        {
            continue;
        }
        let options = &case["settings"];
        let settings = ExplainSettings {
            memory: options["memory"].as_bool().unwrap(),
            analyze: options["analyze"].as_bool().unwrap(),
            buffers: options["buffers"].as_bool().unwrap(),
            timing: options["timing"].as_bool().unwrap(),
            summary: options["summary"].as_bool().unwrap(),
            format: match format.as_str().unwrap() {
                "TEXT" => ExplainFormat::Text,
                "JSON" => ExplainFormat::Json,
                "XML" => ExplainFormat::Xml,
                "YAML" => ExplainFormat::Yaml,
                other => panic!("unknown reference format: {other}"),
            },
            ..ExplainSettings::default()
        };
        let output = render(&settings, &physical, ProductionControl::uncontrolled()).unwrap();
        let expected = case["observation"]["memory"].as_bool().unwrap();
        match settings.format {
            ExplainFormat::Text => {
                assert_eq!(
                    output.contains("Memory: used=2kB  allocated=4kB"),
                    expected,
                    "{case}: {output}"
                );
            }
            ExplainFormat::Json => {
                let value = if output.is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::from_str::<serde_json::Value>(&output).unwrap()[0].clone()
                };
                assert_eq!(
                    value.get("Memory Used").is_some(),
                    expected,
                    "{case}: {output}"
                );
                assert_eq!(
                    value.get("Memory Allocated").is_some(),
                    expected,
                    "{case}: {output}"
                );
                if expected {
                    assert_eq!(value["Memory Used"], 2);
                    assert_eq!(value["Memory Allocated"], 4);
                }
                assert_eq!(
                    value.get("Temp Read Blocks").is_some(),
                    settings.buffers,
                    "{output}"
                );
            }
            ExplainFormat::Xml => {
                assert_eq!(
                    output.contains("<Memory-Used>2</Memory-Used>"),
                    expected,
                    "{case}: {output}"
                );
                assert_eq!(
                    output.contains("<Memory-Allocated>4</Memory-Allocated>"),
                    expected,
                    "{case}: {output}"
                );
            }
            ExplainFormat::Yaml => {
                assert_eq!(
                    output.contains("\"Memory Used\": 2"),
                    expected,
                    "{case}: {output}"
                );
                assert_eq!(
                    output.contains("\"Memory Allocated\": 4"),
                    expected,
                    "{case}: {output}"
                );
            }
        }
        checked += 1;
    }
    assert_eq!(checked, 96);
}

#[test]
fn absent_planning_memory_is_omitted_and_byte_rounding_does_not_overflow() {
    let control = ProductionControl::uncontrolled();
    let settings = ExplainSettings {
        memory: true,
        format: ExplainFormat::Json,
        ..ExplainSettings::default()
    };
    assert!(render(&settings, &physical(None), control)
        .unwrap()
        .is_empty());
    let text = render(
        &settings,
        &physical(Some(ExplainPlanningMemory {
            used_bytes: usize::MAX - 1023,
            allocated_bytes: usize::MAX,
        })),
        control,
    )
    .unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        value[0]["Memory Used"],
        ((usize::MAX - 1023).div_ceil(1024)) as u64
    );
    assert_eq!(
        value[0]["Memory Allocated"],
        usize::MAX.div_ceil(1024) as u64
    );
}

#[test]
fn planning_memory_properties_release_admission_on_failure_and_cancellation() {
    let physical = physical(Some(ExplainPlanningMemory {
        used_bytes: 16,
        allocated_bytes: 64,
    }));
    for format in [
        ExplainFormat::Text,
        ExplainFormat::Json,
        ExplainFormat::Xml,
        ExplainFormat::Yaml,
    ] {
        let settings = ExplainSettings {
            memory: true,
            format,
            ..ExplainSettings::default()
        };
        for limit in [0, 1, 32, 4096] {
            let budget = MemoryBudget::new(limit);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&budget, &original, &invoking);
            let result = render(&settings, &physical, control);
            assert_eq!(result.is_ok(), limit == 4096, "limit={limit}: {result:?}");
            drop(result);
            assert_eq!(budget.used(), 0);
            invoking.cancel();
            assert!(render(&settings, &physical, control).is_err());
            assert_eq!(budget.used(), 0);
        }
    }
}
