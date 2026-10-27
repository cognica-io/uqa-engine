//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_execution::{
    catalog::services::CatalogSession, statement::context::PhysicalExplainPlanning,
};

fn check(engine: &Engine) {
    fixture(engine);
    let query = "SELECT id FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],2)";
    let plan = uqa_sql::plan::UnifiedPlan::lower(uqa_sql::compile(query).unwrap().remove(0));
    let budget = MemoryBudget::new(256 * 1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let physical = engine
        .physical_explain_plan(&plan, &[], true, &control)
        .unwrap();
    assert_eq!(physical.nodes.len(), 1);
    assert!(budget.used() > 0);
    let before = budget.used();
    let constrained = budget.child(256);
    let insufficient = ProductionControl::new(&constrained, &original, &invoking);
    assert_eq!(
        engine
            .physical_explain_plan(&plan, &[], true, &insufficient)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(constrained.used(), 0);
    assert_eq!(budget.used(), before);
    for format in [
        uqa_sql::explain::ExplainFormat::Text,
        uqa_sql::explain::ExplainFormat::Json,
        uqa_sql::explain::ExplainFormat::Xml,
        uqa_sql::explain::ExplainFormat::Yaml,
    ] {
        let settings = uqa_sql::explain::ExplainSettings {
            format,
            ..Default::default()
        };
        let output = uqa_planner::explain::run_explain_with_control(
            &plan, &settings, None, &physical, &control,
        )
        .unwrap();
        assert!(!output.rows.is_empty());
        assert_eq!(
            budget.used(),
            before,
            "rendering lost a capture reservation"
        );
    }
    drop(physical);
    assert_eq!(budget.used(), 0);
    original.cancel();
    assert_eq!(
        engine
            .physical_explain_plan(&plan, &[], true, &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(budget.used(), 0);

    for format in ["TEXT", "JSON", "XML", "YAML"] {
        let result = sql(
            engine,
            &format!("EXPLAIN (COSTS FALSE, FORMAT {format}) {query}"),
        );
        for row in &result.rows {
            let text = match &row["QUERY PLAN"] {
                Value::Json(text) | Value::Str(text) => text,
                other => panic!("incorrect EXPLAIN carrier {other:?}"),
            };
            assert!(!text.contains("Estimated Work") && !text.contains("Estimated-Work"));
        }
    }

    let budget = MemoryBudget::new(128);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    sql(
        engine,
        "BEGIN; SET LOCAL search_path = 'one_very_long_identifier_for_admission, public'",
    );
    assert_eq!(
        engine.explain_settings(&control).unwrap_err().sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), 0);
    sql(engine, "ROLLBACK");
    assert!(explain(engine, query)["Physical Plans"].is_array());
}

#[test]
fn captured_physical_metadata_keeps_its_original_allowance_and_options_on_every_provider() {
    check(&Engine::new());
    for provider in 0..3 {
        let (_directory, engine, _peer) = sessions(provider);
        check(&engine);
    }
}
