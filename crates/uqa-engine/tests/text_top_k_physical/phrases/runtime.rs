//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_engine::SQLParam;

#[test]
fn empty_phrase_graph_still_uses_current_session_budget_for_complete_analysis() {
    let directory = tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("analysis.sqlite3")).unwrap();
    fixture(&engine);
    engine.register_named_analyzer("phrase_stop", r#"{"tokenizer":{"type":"whitespace"},"token_filters":[{"type":"stop","language":"none","custom_words":["red"]}]}"#).unwrap();
    engine
        .set_table_field_analyzer("phrases", "body", "phrase_stop", "search")
        .unwrap();
    engine
        .sql(
            "PREPARE ignored_phrase(text) AS SELECT id FROM phrases WHERE fts_match(body, $1)",
            &[],
        )
        .unwrap();
    let parameters = [SQLParam::Scalar(Value::Str(format!(
        "\"{}\"",
        "red ".repeat(4096)
    )))];
    for limit in ["64kB", "16MB", "64kB"] {
        engine
            .sql(&format!("SET work_mem = '{limit}'"), &[])
            .unwrap();
        let result = engine.sql("EXECUTE ignored_phrase($1)", &parameters);
        if limit == "64kB" {
            assert_eq!(result.unwrap_err().sqlstate(), Some("53200"));
        } else {
            assert!(result.unwrap().rows.is_empty());
        }
    }
    let observer = engine.new_session().unwrap();
    observer.sql("SET work_mem = '16MB'", &[]).unwrap();
    let sql = "SELECT id FROM phrases WHERE fts_match(body, $1)";
    assert!(observer.sql(sql, &parameters).unwrap().rows.is_empty());
    assert_eq!(
        engine.sql(sql, &parameters).unwrap_err().sqlstate(),
        Some("53200")
    );
    assert!(engine
        .sql("EXECUTE ignored_phrase('\"red\"')", &[])
        .unwrap()
        .rows
        .is_empty());
}
