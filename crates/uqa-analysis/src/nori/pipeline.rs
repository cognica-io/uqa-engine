//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve immutable Korean stage inputs before entering the compiled-analyzer cache.

use std::{collections::BTreeMap, sync::Arc};

use super::{
    DictionaryRequest, KoreanFilter, KoreanTokenizer, NoriResources, NoriTokenizerConfig,
    ResolvedDictionary, ResourceHash, DEFAULT_STOP_TAGS,
};
use crate::{AnalysisError, AnalysisResult, Analyzer, TokenFilter, Tokenizer};

mod stages;
pub(crate) use stages::PreparedNoriFilter;

#[derive(Default)]
pub(crate) struct ResolvedNoriPipeline {
    pub tokenizer: Option<KoreanTokenizer>,
    pub normalizer: Option<Arc<ResolvedDictionary>>,
    profiles: BTreeMap<String, Arc<ResolvedDictionary>>,
}

impl ResolvedNoriPipeline {
    pub fn resolve(config: &mut Analyzer, resources: &NoriResources) -> AnalysisResult<Self> {
        let mut resolved = Self::default();
        let mut snapshot = ResourceSnapshot {
            resources,
            entries: Vec::new(),
        };
        if let Tokenizer::Nori(tokenizer) = &mut config.tokenizer {
            let dictionary = snapshot.load(request(&tokenizer.dictionary)?)?;
            resolved.tokenizer = Some(prepare_tokenizer(tokenizer, &dictionary, resources)?);
            tokenizer.dictionary = exact_name(dictionary.sha256());
            resolved.normalizer = Some(dictionary);
        }
        for filter in &mut config.token_filters {
            if let TokenFilter::UnicodeSimpleLowercase(config) = filter {
                let profile = snapshot.load(profile_request(&config.unicode_profile)?)?;
                config.unicode_profile = exact_name(profile.sha256());
                resolved
                    .profiles
                    .insert(config.unicode_profile.clone(), profile);
            }
        }
        canonicalize(config);
        Ok(resolved)
    }

    pub fn filter(&self, filter: &TokenFilter) -> AnalysisResult<Option<PreparedNoriFilter>> {
        let Some(korean) = korean_filter(filter) else {
            return Ok(None);
        };
        let profile = if let TokenFilter::UnicodeSimpleLowercase(config) = filter {
            Some(self.profiles.get(&config.unicode_profile).cloned().ok_or(
                AnalysisError::Descriptor("missing resolved Unicode profile"),
            )?)
        } else {
            None
        };
        Ok(Some(PreparedNoriFilter::new(&korean, profile)))
    }
}

pub(crate) fn request(name: &str) -> AnalysisResult<DictionaryRequest> {
    match name.strip_prefix("sha256:") {
        Some(hash) => Ok(DictionaryRequest::Sha256(hash.parse()?)),
        None => Ok(DictionaryRequest::Name(name.into())),
    }
}

fn exact_name(hash: ResourceHash) -> String {
    format!("sha256:{hash}")
}

pub(crate) fn load_profile(
    name: &str,
    resources: &NoriResources,
) -> AnalysisResult<Arc<ResolvedDictionary>> {
    Ok(resources.load(&profile_request(name)?)?)
}

fn profile_request(name: &str) -> AnalysisResult<DictionaryRequest> {
    Ok(if name == "jdk21" {
        DictionaryRequest::Sha256(uqa_nori_data::BUNDLE_SHA256.parse()?)
    } else {
        request(name)?
    })
}

struct ResourceSnapshot<'a> {
    resources: &'a NoriResources,
    entries: Vec<(DictionaryRequest, Arc<ResolvedDictionary>)>,
}

impl ResourceSnapshot<'_> {
    fn load(&mut self, request: DictionaryRequest) -> AnalysisResult<Arc<ResolvedDictionary>> {
        // An alias resolves once within a pipeline, and an already verified artifact can satisfy its exact hash.
        if let Some((_, dictionary)) = self.entries.iter().find(|(prior, dictionary)| {
            prior == &request || matches!(&request, DictionaryRequest::Sha256(hash) if *hash == dictionary.sha256())
        }) { return Ok(dictionary.clone()); }
        let dictionary = self.resources.load(&request)?;
        self.entries.push((request, dictionary.clone()));
        Ok(dictionary)
    }
}

pub(crate) fn prepare_tokenizer(
    config: &NoriTokenizerConfig,
    dictionary: &ResolvedDictionary,
    resources: &NoriResources,
) -> AnalysisResult<KoreanTokenizer> {
    let user = config
        .user_dictionary
        .as_deref()
        .map(|source| resources.compile_user(source, dictionary.model()))
        .transpose()?;
    KoreanTokenizer::new(
        dictionary.model().clone(),
        user.as_ref().and_then(|user| user.dictionary().cloned()),
        config.options(),
    )
}

pub(crate) fn korean_filter(filter: &TokenFilter) -> Option<KoreanFilter> {
    Some(match filter {
        TokenFilter::NoriPartOfSpeech(config) => KoreanFilter::PartOfSpeech {
            stop_tags: config.stop_tags.clone(),
        },
        TokenFilter::NoriReadingForm(_) => KoreanFilter::ReadingForm,
        TokenFilter::UnicodeSimpleLowercase(_) => KoreanFilter::SimpleLowercase,
        TokenFilter::NoriNumber(_) => KoreanFilter::Number,
        _ => return None,
    })
}

pub(crate) fn canonicalize(config: &mut Analyzer) {
    for filter in &mut config.token_filters {
        if let TokenFilter::NoriPartOfSpeech(config) = filter {
            let tags = config
                .stop_tags
                .get_or_insert_with(|| DEFAULT_STOP_TAGS.to_vec());
            tags.sort_by_key(|tag| tag.ordinal());
            tags.dedup();
        }
    }
}

pub(crate) fn check_resolved(config: &Analyzer) -> AnalysisResult<()> {
    fn exact(name: &str) -> AnalysisResult<()> {
        let DictionaryRequest::Sha256(hash) = request(name)? else {
            return Err(AnalysisError::Descriptor(
                "resolved Korean resources require exact artifact hashes",
            ));
        };
        if exact_name(hash) != name {
            return Err(AnalysisError::Descriptor(
                "noncanonical Korean resource hash",
            ));
        }
        Ok(())
    }
    if let Tokenizer::Nori(config) = &config.tokenizer {
        exact(&config.dictionary)?;
    }
    for filter in &config.token_filters {
        if let TokenFilter::UnicodeSimpleLowercase(config) = filter {
            exact(&config.unicode_profile)?;
        }
    }
    Ok(())
}

pub(crate) fn normalize_budgeted(
    text: &str,
    profile: &ResolvedDictionary,
    budget: &uqa_core::memory::MemoryBudget,
    poll: &mut impl FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<uqa_core::memory::Budgeted<String>> {
    super::filters::normalize_text_budgeted(
        text,
        profile.model(),
        super::NoriLimits::default(),
        budget,
        poll,
    )
}
