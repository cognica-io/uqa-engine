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
use crate::morphology::resources::Snapshot;
use crate::{
    AnalysisError, AnalysisResult, Analyzer, TokenFilter, Tokenizer, UnicodeProfile,
    UnicodeProfileSource,
};

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
        let mut snapshot = Snapshot::default();
        if let Tokenizer::Nori(tokenizer) = &mut config.tokenizer {
            let dictionary = load(&mut snapshot, request(&tokenizer.dictionary)?, resources)?;
            resolved.tokenizer = Some(prepare_tokenizer(tokenizer, &dictionary, resources)?);
            tokenizer.dictionary = exact_name(dictionary.sha256());
            if config.normalization.is_none() {
                resolved.normalizer = Some(dictionary);
            }
        }
        for filter in &mut config.token_filters {
            if let TokenFilter::UnicodeSimpleLowercase(config) = filter {
                if config.unicode_profile.nori_dictionary().is_none() {
                    continue;
                }
                let profile = load(
                    &mut snapshot,
                    profile_request(&config.unicode_profile)?,
                    resources,
                )?;
                let name = exact_name(profile.sha256());
                config
                    .unicode_profile
                    .nori_dictionary_mut()
                    .expect("Nori profile")
                    .clone_from(&name);
                resolved.profiles.insert(name, profile);
            }
        }
        if let Some(UnicodeProfile::Nori { dictionary }) = config
            .normalization
            .as_mut()
            .and_then(|value| value.profile_mut())
        {
            let profile = load(&mut snapshot, request(dictionary)?, resources)?;
            *dictionary = exact_name(profile.sha256());
            resolved.normalizer = Some(profile);
        }
        canonicalize(config);
        Ok(resolved)
    }

    pub fn filter(&self, filter: &TokenFilter) -> AnalysisResult<Option<PreparedNoriFilter>> {
        let Some(korean) = korean_filter(filter) else {
            return Ok(None);
        };
        let profile = if let TokenFilter::UnicodeSimpleLowercase(config) = filter {
            Some(
                self.profiles
                    .get(
                        config
                            .unicode_profile
                            .nori_dictionary()
                            .expect("Nori profile"),
                    )
                    .cloned()
                    .ok_or(AnalysisError::Descriptor(
                        "missing resolved Unicode profile",
                    ))?,
            )
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
    profile: &UnicodeProfileSource,
    resources: &NoriResources,
) -> AnalysisResult<Arc<ResolvedDictionary>> {
    Ok(resources.load(&profile_request(profile)?)?)
}

fn profile_request(profile: &UnicodeProfileSource) -> AnalysisResult<DictionaryRequest> {
    if matches!(profile, UnicodeProfileSource::LegacyNori(name) if name == "jdk21") {
        Ok(DictionaryRequest::Sha256(
            uqa_nori_data::BUNDLE_SHA256.parse()?,
        ))
    } else {
        request(
            profile
                .nori_dictionary()
                .ok_or(AnalysisError::Descriptor("expected a Nori Unicode profile"))?,
        )
    }
}

fn load(
    snapshot: &mut Snapshot<DictionaryRequest, ResolvedDictionary>,
    request: DictionaryRequest,
    resources: &NoriResources,
) -> AnalysisResult<Arc<ResolvedDictionary>> {
    Ok(snapshot.load(request,
        |request, dictionary| matches!(request, DictionaryRequest::Sha256(hash) if *hash == dictionary.sha256()),
        |request| resources.load(request),
    )?)
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
        TokenFilter::UnicodeSimpleLowercase(config)
            if config.unicode_profile.nori_dictionary().is_some() =>
        {
            KoreanFilter::SimpleLowercase
        }
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
    if let Some(UnicodeProfile::Nori { dictionary }) = config
        .normalization
        .as_ref()
        .and_then(|value| value.profile())
    {
        exact(dictionary)?;
    }
    if let Tokenizer::Nori(config) = &config.tokenizer {
        exact(&config.dictionary)?;
    }
    for filter in &config.token_filters {
        if let TokenFilter::UnicodeSimpleLowercase(config) = filter {
            if let Some(dictionary) = config.unicode_profile.nori_dictionary() {
                exact(dictionary)?;
            }
        }
    }
    Ok(())
}
