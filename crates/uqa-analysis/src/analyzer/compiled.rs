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
    tokenizer::PreparedTokenizer, AnalysisError, AnalysisResult, AnalyzedText, AnalyzerDescriptor,
    AnalyzerResources, FilteredText,
};

/// A frozen pipeline with compiled expressions, fixed stop sets, and resolved synonym maps.
#[derive(Debug)]
pub struct CompiledAnalyzer {
    pub(super) image_source: super::image_source::AnalyzerImageCache,
    descriptor: Arc<AnalyzerDescriptor>,
    configuration: Analyzer,
    char_filters: Vec<PreparedCharFilter<'static>>,
    tokenizer: PreparedTokenizer,
    token_filters: Vec<PreparedTokenFilter<'static>>,
    normalization: crate::normalization::PreparedNormalization,
    #[cfg(feature = "nori")]
    pub(super) nori_dictionaries: Vec<Arc<crate::nori::ResolvedDictionary>>,
    #[cfg(feature = "kuromoji")]
    pub(super) kuromoji_dictionaries: Vec<Arc<crate::kuromoji::ResolvedDictionary>>,
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
        crate::work::inline(Self::prepare_with(
            descriptor,
            #[cfg(feature = "nori")]
            nori,
            #[cfg(feature = "kuromoji")]
            kuromoji,
            &mut (),
        ))
    }

    pub(crate) async fn prepare_with(
        descriptor: Arc<AnalyzerDescriptor>,
        #[cfg(feature = "nori")] nori: crate::nori::pipeline::ResolvedNoriPipeline,
        #[cfg(feature = "kuromoji")] kuromoji: crate::kuromoji::pipeline::ResolvedKuromojiPipeline,
        work: &mut impl crate::work::Work<AnalysisError>,
    ) -> AnalysisResult<Self> {
        work.checkpoint().await?;
        let config = descriptor.configuration_with(work).await?;
        let unicode_profile = descriptor.unicode_profile()?;
        let mut char_filters = Vec::with_capacity(config.char_filters.len());
        for (index, filter) in config.char_filters.iter().enumerate() {
            work.records(index).await?;
            char_filters.push(
                filter
                    .prepare_with(work)
                    .await?
                    .into_owned_with(work)
                    .await?,
            );
        }
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
        let mut token_filters = Vec::with_capacity(config.token_filters.len());
        for (index, filter) in config.token_filters.iter().enumerate() {
            work.records(index).await?;
            #[cfg(feature = "nori")]
            if let Some(filter) = nori.filter(filter)? {
                token_filters.push(PreparedTokenFilter::Nori(filter));
                continue;
            }
            #[cfg(feature = "kuromoji")]
            if let Some(filter) = kuromoji.filter(index, filter)? {
                token_filters.push(PreparedTokenFilter::Kuromoji(filter));
                continue;
            }
            token_filters.push(
                filter
                    .prepare_with_profile_and_work(unicode_profile, work)
                    .await?
                    .into_owned_with(work)
                    .await?,
            );
        }
        let normalization = crate::normalization::PreparedNormalization::new(
            config.normalization.as_ref(),
            #[cfg(feature = "nori")]
            nori.normalizer,
            #[cfg(feature = "kuromoji")]
            kuromoji.normalizer,
        )?;
        Ok(Self {
            image_source: super::image_source::AnalyzerImageCache::default(),
            descriptor,
            configuration: config,
            char_filters,
            tokenizer,
            token_filters,
            normalization,
            #[cfg(feature = "nori")]
            nori_dictionaries: nori.dictionaries,
            #[cfg(feature = "kuromoji")]
            kuromoji_dictionaries: kuromoji.dictionaries,
        })
    }

    pub fn descriptor(&self) -> &Arc<AnalyzerDescriptor> {
        &self.descriptor
    }

    /// Borrow the resolved diagnostic configuration retained with this immutable revision.
    ///
    /// This shares preparation's configuration without decoding the descriptor or resolving external resources again.
    pub fn configuration(&self) -> &Analyzer {
        &self.configuration
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

#[cfg(test)]
mod tests;
