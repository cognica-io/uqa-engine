//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Japanese pipeline resources are frozen before descriptor publication.

use super::{
    DictionaryRequest, JapaneseFilter, JapaneseTokenizer, KuromojiResources,
    KuromojiTokenizerConfig, ResolvedDictionary,
};
use crate::morphology::resources::Snapshot;
use crate::{AnalysisError, AnalysisResult, Analyzer, TokenFilter, Tokenizer, UnicodeProfile};
use std::sync::Arc;

mod stages;
pub(crate) use stages::PreparedKuromojiFilter;

#[cfg(test)]
mod tests;

#[derive(Default)]
pub(crate) struct ResolvedKuromojiPipeline {
    pub tokenizer: Option<JapaneseTokenizer>,
    pub normalizer: Option<Arc<ResolvedDictionary>>,
    filters: Vec<Option<Arc<PreparedKuromojiFilter>>>,
}

impl ResolvedKuromojiPipeline {
    pub(crate) fn resolve(
        config: &mut Analyzer,
        resources: &KuromojiResources,
    ) -> AnalysisResult<Self> {
        let mut resolved = Self::default();
        let mut snapshot = Snapshot::default();
        if let Tokenizer::Kuromoji(tokenizer) = &mut config.tokenizer {
            let dictionary = load(&mut snapshot, request(&tokenizer.dictionary)?, resources)?;
            let prepared = prepare_tokenizer(tokenizer, &dictionary, resources)?;
            tokenizer.dictionary = format!("sha256:{}", dictionary.sha256());
            tokenizer.n_best_cost = prepared.n_best_cost();
            tokenizer.n_best_examples = None;
            resolved.tokenizer = Some(prepared);
        }
        for filter in &mut config.token_filters {
            let prepared = if let Some((stage, dictionary)) = japanese_filter(filter) {
                let profile = dictionary
                    .map(|name| load(&mut snapshot, request(name)?, resources))
                    .transpose()?;
                if let TokenFilter::UnicodeSimpleLowercase(config) = filter {
                    *config
                        .unicode_profile
                        .kuromoji_dictionary_mut()
                        .expect("Japanese profile") = format!(
                        "sha256:{}",
                        profile
                            .as_ref()
                            .expect("resolved Japanese profile")
                            .sha256()
                    );
                }
                Some(Arc::new(PreparedKuromojiFilter::new(&stage, profile)?))
            } else {
                None
            };
            resolved.filters.push(prepared);
        }
        if let Some(UnicodeProfile::Kuromoji { dictionary }) = config
            .normalization
            .as_mut()
            .and_then(|value| value.profile_mut())
        {
            let profile = load(&mut snapshot, request(dictionary)?, resources)?;
            *dictionary = format!("sha256:{}", profile.sha256());
            resolved.normalizer = Some(profile);
        }
        Ok(resolved)
    }

    pub(crate) fn filter(
        &self,
        index: usize,
        filter: &TokenFilter,
    ) -> AnalysisResult<Option<Arc<PreparedKuromojiFilter>>> {
        if japanese_filter(filter).is_none() {
            return Ok(None);
        }
        self.filters
            .get(index)
            .and_then(Clone::clone)
            .map(Some)
            .ok_or(AnalysisError::Descriptor(
                "missing resolved Japanese filter",
            ))
    }
}

pub(crate) fn japanese_filter(filter: &TokenFilter) -> Option<(JapaneseFilter, Option<&str>)> {
    match filter {
        TokenFilter::KuromojiBaseForm(_) => Some((JapaneseFilter::BaseForm, None)),
        TokenFilter::KuromojiStem(config) => Some((
            JapaneseFilter::KatakanaStem {
                minimum_length: config.minimum_length,
            },
            None,
        )),
        TokenFilter::KuromojiHiraganaUppercase(_) => {
            Some((JapaneseFilter::HiraganaUppercase, None))
        }
        TokenFilter::KuromojiKatakanaUppercase(_) => {
            Some((JapaneseFilter::KatakanaUppercase, None))
        }
        TokenFilter::KuromojiReadingForm(config) => Some((
            JapaneseFilter::ReadingForm {
                use_romaji: config.use_romaji,
            },
            None,
        )),
        TokenFilter::KuromojiNumber(_) => Some((JapaneseFilter::Number, None)),
        TokenFilter::UnicodeSimpleLowercase(config) => config
            .unicode_profile
            .kuromoji_dictionary()
            .map(|dictionary| (JapaneseFilter::SimpleLowercase, Some(dictionary))),
        _ => None,
    }
}

pub(crate) fn prepare_filter(
    filter: &TokenFilter,
    resources: &KuromojiResources,
) -> AnalysisResult<Arc<PreparedKuromojiFilter>> {
    let (stage, dictionary) =
        japanese_filter(filter).ok_or(AnalysisError::Descriptor("expected a Japanese filter"))?;
    let profile = dictionary
        .map(|name| resources.load(&request(name)?).map_err(AnalysisError::from))
        .transpose()?;
    Ok(Arc::new(PreparedKuromojiFilter::new(&stage, profile)?))
}

pub(crate) fn request(name: &str) -> AnalysisResult<DictionaryRequest> {
    Ok(match name.strip_prefix("sha256:") {
        Some(hash) => DictionaryRequest::Sha256(hash.parse()?),
        None => DictionaryRequest::Name(name.into()),
    })
}

fn load(
    snapshot: &mut Snapshot<DictionaryRequest, ResolvedDictionary>,
    request: DictionaryRequest,
    resources: &KuromojiResources,
) -> AnalysisResult<Arc<ResolvedDictionary>> {
    Ok(snapshot.load(request,
        |request, dictionary| matches!(request, DictionaryRequest::Sha256(hash) if *hash == dictionary.sha256()),
        |request| resources.load(request),
    )?)
}

pub(crate) fn check_resolved(config: &Analyzer) -> AnalysisResult<()> {
    for filter in &config.token_filters {
        if let TokenFilter::UnicodeSimpleLowercase(config) = filter {
            if let Some(dictionary) = config.unicode_profile.kuromoji_dictionary() {
                exact(dictionary)?;
            }
        }
    }
    if let Tokenizer::Kuromoji(tokenizer) = &config.tokenizer {
        exact(&tokenizer.dictionary)?;
        if tokenizer.n_best_examples.is_some() {
            return Err(AnalysisError::Descriptor(
                "resolved Japanese tokenizers require an effective N-best cost without examples",
            ));
        }
    }
    if let Some(UnicodeProfile::Kuromoji { dictionary }) = config
        .normalization
        .as_ref()
        .and_then(|value| value.profile())
    {
        exact(dictionary)?;
    }
    Ok(())
}

fn exact(dictionary: &str) -> AnalysisResult<()> {
    let DictionaryRequest::Sha256(hash) = request(dictionary)? else {
        return Err(AnalysisError::Descriptor(
            "resolved Japanese resources require exact artifact hashes",
        ));
    };
    if format!("sha256:{hash}") != dictionary {
        return Err(AnalysisError::Descriptor(
            "noncanonical Japanese resource hash",
        ));
    }
    Ok(())
}

pub(crate) fn prepare_tokenizer(
    config: &KuromojiTokenizerConfig,
    dictionary: &ResolvedDictionary,
    resources: &KuromojiResources,
) -> AnalysisResult<JapaneseTokenizer> {
    let user = config
        .user_dictionary
        .as_deref()
        .map(|source| resources.compile_user(source, dictionary.model()))
        .transpose()?;
    let tokenizer = JapaneseTokenizer::new(
        dictionary.model().clone(),
        user.as_ref().and_then(|user| user.dictionary().cloned()),
        config.options(),
    )?;
    match config.n_best_examples.as_deref() {
        Some(examples) => tokenizer.with_n_best_examples(examples),
        None => Ok(tokenizer),
    }
}
