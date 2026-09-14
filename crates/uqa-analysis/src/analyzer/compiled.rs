//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable prepared pipelines keep execution state local to each analyzed input.

use std::sync::Arc;
use uqa_core::memory::{Budgeted, MemoryBudget};

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
    normalization: crate::normalization::PreparedNormalization,
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
    pub(crate) fn prepare(
        descriptor: Arc<AnalyzerDescriptor>,
        #[cfg(feature = "nori")] nori: crate::nori::pipeline::ResolvedNoriPipeline,
        #[cfg(feature = "kuromoji")] kuromoji: crate::kuromoji::pipeline::ResolvedKuromojiPipeline,
    ) -> AnalysisResult<Self> {
        let config = descriptor.configuration()?;
        let char_filters = config
            .char_filters
            .iter()
            .map(|filter| filter.prepare().map(PreparedCharFilter::into_owned))
            .collect::<AnalysisResult<_>>()?;
        let tokenizer = match &config.tokenizer {
            #[cfg(feature = "nori")]
            crate::Tokenizer::Nori(_) => PreparedTokenizer::Nori(nori.tokenizer.clone().ok_or(
                crate::AnalysisError::Descriptor("missing resolved Korean tokenizer"),
            )?),
            #[cfg(feature = "kuromoji")]
            crate::Tokenizer::Kuromoji(_) => {
                PreparedTokenizer::Kuromoji(kuromoji.tokenizer.clone().ok_or(
                    crate::AnalysisError::Descriptor("missing resolved Japanese tokenizer"),
                )?)
            }
            _ => config.tokenizer.prepare()?,
        };
        let token_filters = config
            .token_filters
            .iter()
            .map(|filter| {
                #[cfg(feature = "nori")]
                if let Some(filter) = nori.filter(filter)? {
                    return Ok(PreparedTokenFilter::Nori(filter));
                }
                filter.prepare().map(PreparedTokenFilter::into_owned)
            })
            .collect::<AnalysisResult<_>>()?;
        Ok(Self {
            descriptor,
            char_filters,
            tokenizer,
            token_filters,
            normalization: crate::normalization::PreparedNormalization::new(
                config.normalization.as_ref(),
                #[cfg(feature = "nori")]
                nori.normalizer,
                #[cfg(feature = "kuromoji")]
                kuromoji.normalizer,
            )?,
        })
    }

    pub fn descriptor(&self) -> &Arc<AnalyzerDescriptor> {
        &self.descriptor
    }

    /// Analyze with independent output/source state and no expression compilation or file resolution.
    pub fn analyze_tokens(&self, text: &str) -> AnalysisResult<AnalyzedText> {
        Ok(self
            .analyze_tokens_budgeted(text, &MemoryBudget::new(usize::MAX), || Ok(()))?
            .into_parts()
            .0)
    }

    /// Execute all prepared stages with one allowance for runtime buffers and retained output.
    ///
    /// Immutable compiled resources and borrowed input have separate owners. Character maps, tokenization, common/Korean filters and source projections share this allowance. Errors return no partial output. Prepared regex automata retain analysis-owned search workspaces and poll during traversal and capture resolution.
    ///
    /// ```
    /// use uqa_analysis::standard_analyzer;
    /// use uqa_core::memory::MemoryBudget;
    /// let compiled = standard_analyzer("english").compile()?;
    /// let budget = MemoryBudget::new(64 * 1024);
    /// let result = compiled.analyze_tokens_budgeted("The cats and", &budget, || Ok(()))?;
    /// assert_eq!(result.tokens()[0].term(), "cat");
    /// assert_eq!(result.final_position_increment(), 1);
    /// drop(result);
    /// assert_eq!(budget.used(), 0);
    /// # Ok::<(), uqa_analysis::AnalysisError>(())
    /// ```
    pub fn analyze_tokens_budgeted(
        &self,
        text: &str,
        budget: &MemoryBudget,
        mut poll: impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<AnalyzedText>> {
        poll()?;
        let mut filtered = FilteredText::new(text);
        for filter in &self.char_filters {
            filtered = filter.filter_mapped_budgeted(filtered, budget, &mut poll)?;
        }
        let mut tokens = self
            .tokenizer
            .tokenize_mapped_for_filters_budgeted(&filtered, budget, &mut poll)?;
        drop(filtered);
        for filter in &self.token_filters {
            tokens = filter.filter_analyzed_budgeted(tokens, &mut poll)?;
        }
        #[cfg(feature = "kuromoji")]
        if matches!(self.tokenizer, PreparedTokenizer::Kuromoji(_)) {
            tokens.validate_japanese_attributes(&mut poll)?;
        }
        poll()?;
        Ok(tokens)
    }

    pub fn analyze(&self, text: &str) -> AnalysisResult<Vec<String>> {
        self.analyze_tokens(text)?.into_terms()
    }

    /// Normalize complete input with its retained plan, independently of analysis stages.
    pub fn normalize(&self, text: &str) -> AnalysisResult<String> {
        Ok(self
            .normalize_budgeted(text, &MemoryBudget::new(usize::MAX), || Ok(()))?
            .into_parts()
            .0)
    }

    /// Normalize complete text with one retained output allowance and cancellation callback.
    pub fn normalize_budgeted(
        &self,
        text: &str,
        budget: &MemoryBudget,
        mut poll: impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<String>> {
        self.normalization
            .normalize_budgeted(text, budget, &mut poll)
    }
}
