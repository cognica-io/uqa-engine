//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::Cell;
use uqa_sql::ResultRow;

struct Revisions {
    compiled: Arc<CompiledAnalyzer>,
    reads: Cell<usize>,
}

impl AnalyzerRevisions for Revisions {
    fn analyzer_revision(&self, name: &str) -> Result<Arc<CompiledAnalyzer>, String> {
        self.reads.set(self.reads.get() + 1);
        if name == "whole" {
            Ok(Arc::clone(&self.compiled))
        } else {
            Err(format!("missing analyzer {name}"))
        }
    }
}

fn run(
    text: Value,
    query: Value,
    analyzer: Value,
    resources: Option<&dyn AnalyzerRevisions>,
) -> Result<Value, SQLError> {
    let args = [
        text,
        query,
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
        analyzer,
    ]
    .map(ScalarExpr::Literal);
    run_uqa_highlight(
        &ResultRow::new(),
        &args,
        &mut |expression| {
            let ScalarExpr::Literal(value) = expression else {
                unreachable!()
            };
            Ok(value.clone())
        },
        resources,
    )
}

#[test]
fn named_revision_analyzes_the_whole_query_once_without_boolean_word_filtering() {
    let resources = Revisions {
        compiled: uqa_analysis::keyword_analyzer().compile().unwrap(),
        reads: Cell::new(0),
    };
    assert_eq!(
        run(
            Value::Str("new AND york".into()),
            Value::Str("new AND york".into()),
            Value::Str("whole".into()),
            Some(&resources)
        )
        .unwrap(),
        Value::Str("<b>new AND york</b>".into())
    );
    assert_eq!(resources.reads.get(), 1);
}

#[test]
fn word_highlighting_and_null_short_circuits_do_not_require_named_resources() {
    assert_eq!(
        run(
            Value::Str("c++".into()),
            Value::Str("c++".into()),
            Value::Null,
            None
        )
        .unwrap(),
        Value::Str("<b>c</b>++".into())
    );
    assert_eq!(
        run(Value::Null, Value::Str("x".into()), Value::Int(42), None).unwrap(),
        Value::Null
    );
    assert_eq!(
        run(
            Value::Str("c++".into()),
            Value::Null,
            Value::Str("missing".into()),
            None
        )
        .unwrap(),
        Value::Str("c++".into())
    );
    let error = run(
        Value::Str("c++".into()),
        Value::Str("c++".into()),
        Value::Str("whole".into()),
        None,
    )
    .unwrap_err();
    assert!(
        matches!(error, SQLError::Unsupported(message) if message.contains("requires named analyzer resources"))
    );
}

#[test]
fn named_resource_failures_are_reported_before_any_fallback_analysis() {
    let resources = Revisions {
        compiled: uqa_analysis::keyword_analyzer().compile().unwrap(),
        reads: Cell::new(0),
    };
    let error = run(
        Value::Str("x".into()),
        Value::Str("x".into()),
        Value::Str("absent".into()),
        Some(&resources),
    )
    .unwrap_err();
    assert!(
        matches!(error, SQLError::Unsupported(message) if message == "missing analyzer absent")
    );
    assert_eq!(resources.reads.get(), 1);
}
