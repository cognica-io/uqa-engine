//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_engine::SQLParam;

#[test]
fn prepared_highlighting_uses_current_session_work_mem_for_complete_rendering() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("highlight.db")).unwrap();
    engine.sql("PREPARE limited_snippet(text, text) AS SELECT uqa_highlight('fox', 'fox', $1, ']', NULL, NULL, $2) AS h", &[]).unwrap();
    let tag = "[".repeat(100_000);
    let expected = Value::Str(format!("{tag}fox]"));
    for analyzer in [Value::Null, Value::Str("keyword".into())] {
        let parameters = [Value::Str(tag.clone()), analyzer.clone()].map(SQLParam::Scalar);
        engine.sql("SET work_mem = '64kB'", &[]).unwrap();
        let error = engine
            .sql("EXECUTE limited_snippet($1, $2)", &parameters)
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert!(
            error.to_string().contains("highlight analysis failed"),
            "{error}"
        );
        engine.sql("SET work_mem = '1MB'", &[]).unwrap();
        assert_eq!(
            engine
                .sql("EXECUTE limited_snippet($1, $2)", &parameters)
                .unwrap()
                .rows[0]["h"],
            expected
        );
        let observer = engine.new_session().unwrap();
        observer.sql("SET work_mem = '1MB'", &[]).unwrap();
        engine.sql("SET work_mem = '64kB'", &[]).unwrap();
        let sql = "SELECT uqa_highlight('fox', 'fox', $1, ']', NULL, NULL, $2) AS h";
        assert_eq!(
            observer.sql(sql, &parameters).unwrap().rows[0]["h"],
            expected
        );
        assert_eq!(
            engine.sql(sql, &parameters).unwrap_err().sqlstate(),
            Some("53200")
        );
    }
}

#[test]
fn scalar_argument_cancellation_reaches_highlighting_and_can_be_reset() {
    let engine = Engine::new();
    let token = engine.cancellation_token();
    engine
        .register_scalar_function("cancel_snippet", move |_: &[Value]| {
            token.cancel();
            Ok(Value::Str("fox".into()))
        })
        .unwrap();
    for analyzer in ["NULL", "'keyword'"] {
        let error = engine.sql(&format!("SELECT uqa_highlight('fox', cancel_snippet(), NULL, NULL, NULL, NULL, {analyzer})"), &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        engine.reset_cancellation();
        assert_eq!(engine.sql(&format!("SELECT uqa_highlight('fox', 'fox', NULL, NULL, NULL, NULL, {analyzer}) AS h"), &[]).unwrap().rows[0]["h"], Value::Str("<b>fox</b>".into()));
    }
}
