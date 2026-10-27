//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::explain::{ExplainFormat, ExplainSettings};

#[test]
fn formats_retain_native_plan_data_and_sql_result_identity() {
    let body = UnifiedPlan::lower(
        uqa_sql::compile("SELECT '<&\"한글' AS value")
            .unwrap()
            .remove(0),
    );
    for (format, ty) in [
        (ExplainFormat::Text, uqa_sql::ColumnType::Text),
        (ExplainFormat::Json, uqa_sql::ColumnType::Json),
        (ExplainFormat::Xml, uqa_sql::ColumnType::Xml),
        (ExplainFormat::Yaml, uqa_sql::ColumnType::Text),
    ] {
        let settings = ExplainSettings {
            format,
            verbose: true,
            ..ExplainSettings::default()
        };
        let result =
            run_explain_with_physical(&body, &settings, None, &ExplainPhysicalPlan::default())
                .unwrap();
        assert_eq!(result.columns, ["QUERY PLAN"]);
        assert_eq!(result.column_types, [Some(ty)]);
        if format == ExplainFormat::Text {
            assert!(result.rows.len() > 1);
            continue;
        }
        assert_eq!(result.rows.len(), 1);
        match result.value_at(0, 0).unwrap() {
            Value::Json(text) => {
                let document: serde_json::Value = serde_json::from_str(text).unwrap();
                assert_eq!(document.as_array().unwrap().len(), 1);
                assert_eq!(document[0]["Plan"]["Node Type"], "Select");
                assert!(document[0]["Plan"]["UQA Plan"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|line| line.as_str().unwrap().contains("physical_plan=")));
            }
            Value::Str(text) if format == ExplainFormat::Xml => {
                uqa_sql::xml::validate(text, uqa_sql::xml::XmlOption::Document).unwrap();
                assert!(text.contains("<Node-Type>Select</Node-Type>"));
                assert!(text.contains("&lt;&amp;"));
            }
            Value::Str(text) if format == ExplainFormat::Yaml => {
                assert!(text.starts_with("-\n"));
                assert!(text.contains("\"Node Type\": \"Select\""));
            }
            other => panic!("incorrect {format:?} carrier: {other:?}"),
        }
    }
}

#[test]
fn controlled_formats_reject_large_borrowed_diagnostics_before_copying_the_payload() {
    use uqa_core::{memory::MemoryBudget, CancellationToken};
    let body = UnifiedPlan::lower(uqa_sql::compile("SELECT 1").unwrap().remove(0));
    let mut node = uqa_sql::result::explain::ExplainObject::new(ProductionControl::uncontrolled());
    node.text("Relation Name", &"<&한글\\\"".repeat(512 * 1024))
        .unwrap();
    let physical = ExplainPhysicalPlan {
        nodes: vec![node.finish().unwrap()],
        ..ExplainPhysicalPlan::default()
    };
    for format in [
        ExplainFormat::Text,
        ExplainFormat::Json,
        ExplainFormat::Xml,
        ExplainFormat::Yaml,
    ] {
        let budget = MemoryBudget::new(64 * 1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let settings = ExplainSettings {
            format,
            ..ExplainSettings::default()
        };
        let allocation = allocation_counter::measure(|| {
            let error =
                run_explain_with_control(&body, &settings, None, &physical, &control).unwrap_err();
            assert_eq!(error.sqlstate(), Some("53200"));
        });
        assert!(
            allocation.bytes_max < 256 * 1024,
            "{format:?}: {allocation:?}"
        );
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn controlled_formats_observe_both_cancellation_owners_and_release_scratch() {
    use uqa_core::{memory::MemoryBudget, CancellationToken};
    let body = UnifiedPlan::lower(
        uqa_sql::compile("SELECT '<&한글' AS value")
            .unwrap()
            .remove(0),
    );
    for format in [
        ExplainFormat::Text,
        ExplainFormat::Json,
        ExplainFormat::Xml,
        ExplainFormat::Yaml,
    ] {
        for cancel_original in [true, false] {
            let budget = MemoryBudget::new(1024 * 1024);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            if cancel_original {
                original.cancel();
            } else {
                invoking.cancel();
            }
            let control = ProductionControl::new(&budget, &original, &invoking);
            let settings = ExplainSettings {
                format,
                verbose: true,
                ..ExplainSettings::default()
            };
            let error = run_explain_with_control(
                &body,
                &settings,
                None,
                &ExplainPhysicalPlan::default(),
                &control,
            )
            .unwrap_err();
            assert_eq!(error.sqlstate(), Some("57014"));
            assert_eq!(budget.used(), 0);
        }
    }
}
