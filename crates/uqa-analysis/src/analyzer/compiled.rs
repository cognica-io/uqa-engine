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
    tokenizer::PreparedTokenizer, AnalysisResult, AnalyzedText, AnalyzerDescriptor,
    AnalyzerResources, FilteredText,
};

/// A frozen pipeline with compiled expressions, fixed stop sets, and resolved synonym maps.
#[derive(Debug)]
pub struct CompiledAnalyzer {
    descriptor: Arc<AnalyzerDescriptor>,
    char_filters: Vec<PreparedCharFilter<'static>>,
    tokenizer: PreparedTokenizer,
    token_filters: Vec<PreparedTokenFilter<'static>>,
}

impl Analyzer {
    /// Resolve immutable inputs and reuse prepared stages with the default resource owner.
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
        AnalyzerResources::default().compile(self)
    }

    /// Resolve and compile this configuration with an explicit bounded resource owner.
    pub fn compile_with_resources(
        &self,
        resources: &AnalyzerResources,
    ) -> AnalysisResult<Arc<CompiledAnalyzer>> {
        resources.compile(self)
    }
}

impl CompiledAnalyzer {
    pub(crate) fn prepare(descriptor: Arc<AnalyzerDescriptor>) -> AnalysisResult<Self> {
        let config = descriptor.configuration()?;
        let char_filters = config
            .char_filters
            .iter()
            .map(|filter| filter.prepare().map(PreparedCharFilter::into_owned))
            .collect::<AnalysisResult<_>>()?;
        let tokenizer = config.tokenizer.prepare()?;
        let token_filters = config
            .token_filters
            .iter()
            .map(|filter| filter.prepare().map(PreparedTokenFilter::into_owned))
            .collect::<AnalysisResult<_>>()?;
        Ok(Self {
            descriptor,
            char_filters,
            tokenizer,
            token_filters,
        })
    }

    pub fn descriptor(&self) -> &Arc<AnalyzerDescriptor> {
        &self.descriptor
    }

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
