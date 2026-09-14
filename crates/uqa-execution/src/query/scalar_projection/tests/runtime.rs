//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::query::runtime::{QueryMemorySettings, QueryRuntimeView};
use parking_lot::{Mutex, RwLock};
use std::sync::atomic::{AtomicUsize, Ordering};
use uqa_core::CancellationToken;

struct Settings {
    bytes: AtomicUsize,
    reads: AtomicUsize,
}
impl Settings {
    fn new(bytes: usize) -> Self {
        Self {
            bytes: AtomicUsize::new(bytes),
            reads: AtomicUsize::new(0),
        }
    }
}
impl QueryMemorySettings for Settings {
    fn work_mem_bytes(&self) -> Result<usize, SQLError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let bytes = self.bytes.load(Ordering::SeqCst);
        if bytes == usize::MAX {
            return Err(SQLError::Internal("invalid test setting".into()));
        }
        Ok(bytes)
    }
}

fn with_runtime(
    settings: &Settings,
    cancellation: &CancellationToken,
    run: impl FnOnce(QueryRuntimeView<'_>),
) {
    let scalar_functions = RwLock::default();
    let table_functions = RwLock::default();
    let aggregate_functions = RwLock::default();
    let notices = Mutex::default();
    run(QueryRuntimeView {
        settings,
        cancellation,
        scalar_functions: &scalar_functions,
        table_functions: &table_functions,
        aggregate_functions: &aggregate_functions,
        notices: &notices,
    });
}

fn execute(
    runtime: QueryRuntimeView<'_>,
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
    run_uqa_highlight_with_runtime(
        &ResultRow::new(),
        &args,
        &mut |expression| {
            let ScalarExpr::Literal(value) = expression else {
                unreachable!()
            };
            Ok(value.clone())
        },
        resources,
        Some(runtime),
    )
}

#[test]
fn named_and_word_highlighting_read_live_limits_and_preserve_sqlstate() {
    let settings = Settings::new(32);
    let cancellation = CancellationToken::new();
    let resources = Revisions {
        compiled: uqa_analysis::keyword_analyzer().compile().unwrap(),
        reads: Cell::new(0),
    };
    with_runtime(&settings, &cancellation, |runtime| {
        for analyzer in [Value::Null, Value::Str("whole".into())] {
            let call = || {
                execute(
                    runtime,
                    Value::Str("fox".into()),
                    Value::Str("fox".into()),
                    analyzer.clone(),
                    Some(&resources),
                )
            };
            settings.bytes.store(32, Ordering::SeqCst);
            let error = call().unwrap_err();
            assert_eq!(error.sqlstate(), Some("53200"));
            assert!(error.to_string().contains("highlight analysis failed"));
            settings.bytes.store(1 << 20, Ordering::SeqCst);
            assert_eq!(call().unwrap(), Value::Str("<b>fox</b>".into()));
        }
    });
    assert_eq!(settings.reads.load(Ordering::SeqCst), 4);
    assert_eq!(resources.reads.get(), 2);
}

#[test]
fn argument_short_circuits_precede_runtime_settings_and_resource_reads() {
    let settings = Settings::new(usize::MAX);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let resources = Revisions {
        compiled: uqa_analysis::keyword_analyzer().compile().unwrap(),
        reads: Cell::new(0),
    };
    with_runtime(&settings, &cancellation, |runtime| {
        assert_eq!(
            execute(
                runtime,
                Value::Null,
                Value::Str("x".into()),
                Value::Int(42),
                Some(&resources)
            )
            .unwrap(),
            Value::Null
        );
        assert_eq!(
            execute(
                runtime,
                Value::Str("original".into()),
                Value::Null,
                Value::Str("missing".into()),
                Some(&resources)
            )
            .unwrap(),
            Value::Str("original".into())
        );
        assert_eq!(settings.reads.load(Ordering::SeqCst), 0);
        let error = execute(
            runtime,
            Value::Str("x".into()),
            Value::Str("x".into()),
            Value::Null,
            Some(&resources),
        )
        .unwrap_err();
        assert!(error.to_string().contains("invalid test setting"));
        settings.bytes.store(1 << 20, Ordering::SeqCst);
        let error = execute(
            runtime,
            Value::Str("x".into()),
            Value::Str("x".into()),
            Value::Str("missing".into()),
            Some(&resources),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
    });
    assert_eq!(resources.reads.get(), 0);
}

#[test]
fn cancellation_after_resource_lookup_reaches_the_analysis_callback() {
    struct CancelOnRead<'a> {
        cancellation: &'a CancellationToken,
        compiled: Arc<CompiledAnalyzer>,
    }
    impl AnalyzerRevisions for CancelOnRead<'_> {
        fn analyzer_revision(&self, _: &str) -> Result<Arc<CompiledAnalyzer>, String> {
            self.cancellation.cancel();
            Ok(Arc::clone(&self.compiled))
        }
    }
    let cancellation = CancellationToken::new();
    let resources = CancelOnRead {
        cancellation: &cancellation,
        compiled: uqa_analysis::keyword_analyzer().compile().unwrap(),
    };
    with_runtime(&Settings::new(1 << 20), &cancellation, |runtime| {
        let error = execute(
            runtime,
            Value::Str("x".into()),
            Value::Str("x".into()),
            Value::Str("whole".into()),
            Some(&resources),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            SQLError::Cancelled(uqa_core::QueryCancelled)
        ));
        assert_eq!(error.sqlstate(), Some("57014"));
    });
}

#[test]
fn boolean_candidate_scan_borrows_unicode_words_and_releases_failed_storage() {
    let query = "AND\tfox\u{2003}Or NOT 한🙂 ΑND ＡＮＤ andrés and OR not";
    let budget = MemoryBudget::new(4096);
    let terms = query_candidates(query, &budget, &mut || Ok(())).unwrap();
    assert_eq!(&*terms, &["fox", "한🙂", "ΑND", "ＡＮＤ", "andrés"]);
    for term in &*terms {
        assert!(query.as_bytes().as_ptr_range().contains(&term.as_ptr()));
    }
    assert_eq!(budget.used(), terms.capacity() * size_of::<&str>());
    drop(terms);
    assert_eq!(budget.used(), 0);
    let query = format!("fox {} bar", "한".repeat(4096));
    let mut calls = 0;
    drop(
        query_candidates(&query, &budget, &mut || {
            calls += 1;
            Ok(())
        })
        .unwrap(),
    );
    assert!(calls >= 5);
    for stop in 1..=calls {
        let other = budget.reserve(7).unwrap();
        let mut count = 0;
        assert!(matches!(
            query_candidates(&query, &budget, &mut || {
                count += 1;
                if count == stop {
                    Err(AnalysisError::Cancelled)
                } else {
                    Ok(())
                }
            }),
            Err(AnalysisError::Cancelled)
        ));
        assert_eq!(budget.used(), 7);
        drop(other);
    }
    let budget = MemoryBudget::new(7);
    let other = budget.reserve(7).unwrap();
    assert!(matches!(
        query_candidates("fox", &budget, &mut || Ok(())),
        Err(AnalysisError::Memory(_))
    ));
    assert_eq!(budget.used(), 7);
    drop(other);
}
