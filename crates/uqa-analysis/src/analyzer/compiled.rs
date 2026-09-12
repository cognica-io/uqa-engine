//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable prepared pipelines keep execution state local to each analyzed input.

use std::sync::Arc;

use super::Analyzer;
use crate::{
    char_filter::PreparedCharFilter, token_filter::PreparedTokenFilter,
    tokenizer::PreparedTokenizer, AnalysisResult, AnalyzedText, FilteredText,
};

/// A frozen pipeline with compiled expressions, fixed stop sets, and resolved synonym maps.
#[derive(Debug)]
pub struct CompiledAnalyzer {
    char_filters: Vec<PreparedCharFilter<'static>>,
    tokenizer: PreparedTokenizer,
    token_filters: Vec<PreparedTokenFilter<'static>>,
}

impl Analyzer {
    /// Compile independently owned, reusable stages from this configuration.
    ///
    /// Each file-backed synonym stage resolves its map during compilation. The existing uncompiled analysis methods continue to reload it on every call.
    ///
    /// ```
    /// use uqa_analysis::standard_analyzer;
    /// let compiled = standard_analyzer("english").compile()?;
    /// let result = compiled.analyze_tokens("The cats and")?;
    /// assert_eq!(result.tokens()[0].term(), "cat");
    /// assert_eq!(result.tokens()[0].offsets().unwrap().utf8, 4..8);
    /// assert_eq!(result.tokens()[0].position_increment(), 2);
    /// assert_eq!(result.final_position_increment(), 1);
    /// assert_eq!(compiled.analyze("Dogs")?, ["dog"]);
    /// # Ok::<(), uqa_analysis::AnalysisError>(())
    /// ```
    pub fn compile(&self) -> AnalysisResult<Arc<CompiledAnalyzer>> {
        let char_filters = self
            .char_filters
            .iter()
            .map(|filter| filter.prepare().map(PreparedCharFilter::into_owned))
            .collect::<AnalysisResult<_>>()?;
        let tokenizer = self.tokenizer.prepare()?;
        let token_filters = self
            .token_filters
            .iter()
            .map(|filter| filter.prepare().map(PreparedTokenFilter::into_owned))
            .collect::<AnalysisResult<_>>()?;
        Ok(Arc::new(CompiledAnalyzer {
            char_filters,
            tokenizer,
            token_filters,
        }))
    }
}

impl CompiledAnalyzer {
    /// Analyze with independent output/source state and no expression compilation or file resolution.
    pub fn analyze_tokens(&self, text: &str) -> AnalysisResult<AnalyzedText> {
        let mut filtered = FilteredText::new(text);
        for filter in &self.char_filters {
            filtered = filter.filter_mapped(filtered)?;
        }
        let mut tokens = self.tokenizer.tokenize_mapped(&filtered)?;
        for filter in &self.token_filters {
            tokens = filter.filter_analyzed(tokens)?;
        }
        Ok(tokens)
    }

    pub fn analyze(&self, text: &str) -> AnalysisResult<Vec<String>> {
        self.analyze_tokens(text)?.into_terms()
    }
}
