//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::query::runtime::QueryMemorySettings;
use parking_lot::{Mutex, RwLock};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_analysis::CompiledAnalyzer;
use uqa_core::CancellationToken;

struct Settings(AtomicUsize);

impl QueryMemorySettings for Settings {
    fn work_mem_bytes(&self) -> Result<usize, SQLError> {
        Ok(self.0.load(Ordering::Relaxed))
    }
}

struct Revisions {
    analyzer: Arc<CompiledAnalyzer>,
    cancel_on_resolve: Option<CancellationToken>,
}

impl AnalyzerRevisions for Revisions {
    fn analyzer_revision(&self, _: &str) -> Result<Arc<CompiledAnalyzer>, String> {
        if let Some(token) = &self.cancel_on_resolve {
            token.cancel();
        }
        Ok(self.analyzer.clone())
    }
}

#[test]
fn diagnostic_execution_reads_live_limits_and_preserves_cancellation() {
    let settings = Settings(AtomicUsize::new(32));
    let cancellation = CancellationToken::new();
    let scalar_functions = RwLock::default();
    let table_functions = RwLock::default();
    let aggregate_functions = RwLock::default();
    let notices = Mutex::default();
    let runtime = QueryRuntimeView {
        settings: &settings,
        cancellation: &cancellation,
        scalar_functions: &scalar_functions,
        table_functions: &table_functions,
        aggregate_functions: &aggregate_functions,
        notices: &notices,
    };
    let mut revisions = Revisions {
        analyzer: uqa_analysis::keyword_analyzer().compile().unwrap(),
        cancel_on_resolve: None,
    };
    let error = analyze_text(runtime, &revisions, "keyword", "UQA").unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    settings.0.store(1024 * 1024, Ordering::Relaxed);
    assert!(matches!(
        analyze_text(runtime, &revisions, "keyword", "UQA").unwrap(),
        Value::JsonB(_)
    ));
    revisions.cancel_on_resolve = Some(cancellation.clone());
    let error = analyze_text(runtime, &revisions, "keyword", "UQA").unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
}
