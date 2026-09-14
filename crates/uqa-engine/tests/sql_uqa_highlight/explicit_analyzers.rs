//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_engine::SQLParam;

fn snippet(engine: &Engine, text: &str, query: &str, analyzer: &str) -> Value {
    engine
        .sql(
            "SELECT uqa_highlight($1, $2, NULL, NULL, NULL, NULL, $3) AS h",
            &[text, query, analyzer].map(|value| SQLParam::Scalar(Value::Str(value.into()))),
        )
        .unwrap()
        .rows[0]["h"]
        .clone()
}

#[test]
fn explicit_analyzers_use_complete_source_and_query_text() {
    let engine = Engine::new();
    assert_eq!(
        snippet(&engine, "c++", "c++", "keyword"),
        Value::Str("<b>c++</b>".into())
    );
    assert_eq!(
        snippet(&engine, "new AND york", "new AND york", "keyword"),
        Value::Str("<b>new AND york</b>".into())
    );
    assert_eq!(
        snippet(&engine, "new york", "new", "keyword"),
        Value::Str("new york".into())
    );
    let legacy = engine.sql("SELECT uqa_highlight('c++', 'c++') AS h, uqa_highlight('c++', 'c++', NULL, NULL, NULL, NULL, NULL) AS nullable", &[]).unwrap();
    assert_eq!(legacy.rows[0]["h"], Value::Str("<b>c</b>++".into()));
    assert_eq!(legacy.rows[0]["h"], legacy.rows[0]["nullable"]);
}

#[test]
fn analyzer_arguments_preserve_null_short_circuits_and_report_errors() {
    let engine = Engine::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    engine
        .register_scalar_function("snippet_analyzer", move |_args: &[Value]| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(Value::Str("keyword".into()))
        })
        .unwrap();
    let row = &engine.sql("SELECT uqa_highlight(NULL, 'x', NULL, NULL, NULL, NULL, snippet_analyzer()) AS nil, uqa_highlight('c++', NULL, NULL, NULL, NULL, NULL, snippet_analyzer()) AS original", &[]).unwrap().rows[0];
    assert_eq!(row["nil"], Value::Null);
    assert_eq!(row["original"], Value::Str("c++".into()));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let row = &engine
        .sql(
            "SELECT uqa_highlight('c++', 'c++', NULL, NULL, NULL, NULL, snippet_analyzer()) AS h",
            &[],
        )
        .unwrap()
        .rows[0];
    assert_eq!(row["h"], Value::Str("<b>c++</b>".into()));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    for (expression, message) in [
        (
            "uqa_highlight('x', 'x', NULL, NULL, NULL, NULL, 42)",
            "analyzer must be string",
        ),
        (
            "uqa_highlight('x', 'x', NULL, NULL, NULL, NULL, 'missing_snippet_analyzer')",
            "is not registered",
        ),
        (
            "uqa_highlight('x', 'x', NULL, NULL, NULL, NULL, ' ')",
            "cannot be empty",
        ),
        (
            "uqa_highlight('x', 'x', NULL, NULL, NULL, NULL, 'keyword', 'extra')",
            "2..=7",
        ),
    ] {
        let error = engine
            .sql(&format!("SELECT {expression}"), &[])
            .unwrap_err();
        assert!(error.to_string().contains(message), "{expression}: {error}");
    }
}

const SOURCE_MAPPING: &str = r#"{"tokenizer":{"type":"whitespace"},"char_filters":[{"type":"pattern_replace","pattern":"New York","replacement":"NY"}],"token_filters":[]}"#;
const WORDS: &str = r#"{"tokenizer":{"type":"whitespace"},"token_filters":[]}"#;

fn revision_lifecycle(engine: &Engine) {
    engine
        .register_named_analyzer("snippet_revision", SOURCE_MAPPING)
        .unwrap();
    engine.sql("PREPARE snippet AS SELECT uqa_highlight('visit New York today', 'NY', NULL, NULL, NULL, NULL, 'snippet_revision') AS h", &[]).unwrap();
    let expected = Value::Str("visit <b>New York</b> today".into());
    assert_eq!(
        engine.sql("EXECUTE snippet", &[]).unwrap().rows[0]["h"],
        expected
    );
    engine.sql("BEGIN; SAVEPOINT prior_analyzer", &[]).unwrap();
    engine
        .register_named_analyzer("snippet_revision", WORDS)
        .unwrap();
    assert_eq!(
        engine.sql("EXECUTE snippet", &[]).unwrap().rows[0]["h"],
        Value::Str("visit New York today".into())
    );
    engine.sql("ROLLBACK TO prior_analyzer", &[]).unwrap();
    assert_eq!(
        engine.sql("EXECUTE snippet", &[]).unwrap().rows[0]["h"],
        expected
    );
    engine.sql("COMMIT", &[]).unwrap();
}

#[test]
fn named_highlight_revisions_follow_savepoints_preparation_and_session_epochs() {
    revision_lifecycle(&Engine::new());
    let directory = tempfile::tempdir().unwrap();
    for provider in ["sqlite", "redb"] {
        let path = directory.path().join(provider);
        let open = || match provider {
            "sqlite" => Engine::open(&path).unwrap(),
            "redb" => Engine::from_persistent_provider(Arc::new(
                uqa_storage_redb::RedbStorage::open(&path).unwrap(),
            ))
            .unwrap(),
            _ => unreachable!(),
        };
        let engine = open();
        revision_lifecycle(&engine);
        let observer = engine.new_session().unwrap();
        assert_eq!(
            snippet(&observer, "New York", "NY", "snippet_revision"),
            Value::Str("<b>New York</b>".into())
        );
        engine
            .register_named_analyzer("snippet_revision", WORDS)
            .unwrap();
        assert_eq!(
            snippet(&observer, "New York", "NY", "snippet_revision"),
            Value::Str("New York".into())
        );
        engine
            .register_named_analyzer("snippet_revision", SOURCE_MAPPING)
            .unwrap();
        drop(observer);
        drop(engine);
        assert_eq!(
            snippet(&open(), "New York", "NY", "snippet_revision"),
            Value::Str("<b>New York</b>".into())
        );
    }
}

#[test]
fn nori_highlights_source_spans_through_durable_named_resources() {
    let config = r#"{"tokenizer":{"type":"nori_tokenizer","decompound_mode":"mixed","user_dictionary":"서울역 서울 역\n🙂a 가 나\nc++"},"char_filters":[{"type":"html_strip"}],"token_filters":[{"type":"nori_readingform"}]}"#;
    if let Err(error) = serde_json::from_str::<uqa_analysis::Analyzer>(config) {
        assert!(error
            .to_string()
            .contains("unknown variant `nori_tokenizer`"));
        assert!(Engine::new()
            .register_named_analyzer("korean_snippet", config)
            .is_err());
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    for provider in ["memory", "sqlite", "redb"] {
        let path = directory.path().join(provider);
        let open = || match provider {
            "memory" => Engine::new(),
            "sqlite" => Engine::open(&path).unwrap(),
            "redb" => Engine::from_persistent_provider(Arc::new(
                uqa_storage_redb::RedbStorage::open(&path).unwrap(),
            ))
            .unwrap(),
            _ => unreachable!(),
        };
        let engine = open();
        engine
            .register_named_analyzer("korean_snippet", config)
            .unwrap();
        let verify = |engine: &Engine| {
            for (source, query, expected) in [
                ("서울역 도착", "역", "서울<b>역</b> 도착"),
                ("서울역 도착", "서울역", "<b>서울역</b> 도착"),
                ("<i>韓國</i> 경제", "한국", "<i><b>韓國</b></i> 경제"),
                ("<i>🙂a</i> �a", "🙂a", "<i><b>🙂a</b></i> �a"),
                ("c++ 개발", "c++", "<b>c++</b> 개발"),
            ] {
                assert_eq!(
                    snippet(engine, source, query, "korean_snippet"),
                    Value::Str(expected.into()),
                    "{provider}: {source}"
                );
            }
        };
        verify(&engine);
        if provider != "memory" {
            drop(engine);
            verify(&open());
        }
    }
}
