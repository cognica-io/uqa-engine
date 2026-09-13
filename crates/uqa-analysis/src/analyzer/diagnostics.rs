//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Serialize complete analysis diagnostics while retaining analysis and output reservations.

use std::io::{self, Write};

use serde::Serialize;
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget};

use crate::{AnalysisError, AnalysisResult, AnalyzedText, AnalyzerFingerprint, CompiledAnalyzer};

#[derive(Serialize)]
struct Diagnostic<'a> {
    #[serde(flatten)]
    analysis: &'a AnalyzedText,
    analyzer_fingerprint: &'a AnalyzerFingerprint,
}

impl CompiledAnalyzer {
    /// Analyze and encode the complete token graph, source end state, and immutable revision.
    ///
    /// Analysis and JSON output share the supplied allowance. The token stream remains reserved until encoding finishes, and cancellation is checked while writing. The returned string retains its output reservation.
    pub fn analyze_diagnostic_budgeted(
        &self,
        text: &str,
        budget: &MemoryBudget,
        mut poll: impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<String>> {
        let analysis = self.analyze_tokens_budgeted(text, budget, &mut poll)?;
        let diagnostic = Diagnostic {
            analysis: &analysis,
            analyzer_fingerprint: &self.descriptor().fingerprint(),
        };
        let mut writer = DiagnosticWriter {
            bytes: BudgetedVec::new(budget),
            poll: &mut poll,
            failure: None,
        };
        let result = serde_json::to_writer(&mut writer, &diagnostic);
        if let Some(error) = writer.failure {
            return Err(error);
        }
        result?;
        (writer.poll)()?;
        drop(analysis);
        let (bytes, memory) = writer.bytes.into_parts();
        let json = String::from_utf8(bytes).expect("JSON serialization emits valid UTF-8");
        Ok(Budgeted::new(json, memory))
    }
}

struct DiagnosticWriter<'a> {
    bytes: BudgetedVec<u8>,
    poll: &'a mut dyn FnMut() -> AnalysisResult<()>,
    failure: Option<AnalysisError>,
}

impl Write for DiagnosticWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let result = (self.poll)().and_then(|()| {
            self.bytes.extend_from_slice(bytes)?;
            Ok(())
        });
        if let Err(error) = result {
            self.failure = Some(error);
            return Err(io::Error::other("analysis diagnostic encoding interrupted"));
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
