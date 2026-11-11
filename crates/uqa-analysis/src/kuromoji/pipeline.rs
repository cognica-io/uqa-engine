//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Japanese pipeline resources are frozen before descriptor publication.

pub(crate) use super::resources::preparation::Source;
use super::{
    DictionaryRequest, JapaneseTokenizer, KuromojiResources, KuromojiTokenizerConfig,
    ResolvedDictionary,
};
use crate::morphology::resources::Snapshot;
use crate::{AnalysisError, AnalysisResult, Analyzer, TokenFilter, Tokenizer, UnicodeProfile};
use std::sync::Arc;

mod configuration;
pub(crate) use configuration::{canonicalize, is_japanese_filter};

mod stages;
pub(crate) use stages::PreparedKuromojiFilter;

#[cfg(test)]
mod tests;

#[derive(Default)]
pub(crate) struct ResolvedKuromojiPipeline {
    pub tokenizer: Option<JapaneseTokenizer>,
    pub normalizer: Option<Arc<ResolvedDictionary>>,
    pub dictionaries: Vec<Arc<ResolvedDictionary>>,
    filters: Vec<Option<Arc<PreparedKuromojiFilter>>>,
}

impl ResolvedKuromojiPipeline {
    pub(crate) fn resolve(
        config: &mut Analyzer,
        resources: &KuromojiResources,
        limits: crate::AnalyzerLimits,
    ) -> AnalysisResult<Self> {
        crate::work::inline(Self::resolve_with(config, resources, limits, &mut ()))
    }

    pub(crate) async fn resolve_with(
        config: &mut Analyzer,
        resources: &impl Source,
        limits: crate::AnalyzerLimits,
        work: &mut impl crate::work::ResourceWork,
    ) -> AnalysisResult<Self> {
        let mut resolved = Self::default();
        let mut snapshot = Snapshot::default();
        if let Tokenizer::Kuromoji(tokenizer) = &mut config.tokenizer {
            let dictionary = load(
                &mut snapshot,
                request(&tokenizer.dictionary)?,
                resources,
                work,
            )
            .await?;
            let prepared = prepare_tokenizer_with(tokenizer, &dictionary, resources, work).await?;
            tokenizer.dictionary = format!("sha256:{}", dictionary.sha256());
            tokenizer.n_best_cost = prepared.n_best_cost();
            tokenizer.n_best_examples = None;
            resolved.tokenizer = Some(prepared);
            resolved.dictionaries.push(dictionary);
        }
        for index in 0..config.token_filters.len() {
            crate::work::Work::<crate::AnalysisError>::records(work, index).await?;
            let filter = &mut config.token_filters[index];
            if !is_japanese_filter(filter) {
                resolved.filters.push(None);
                continue;
            }
            let profile = match configuration::dictionary_name(filter)? {
                Some(name) => Some(load(&mut snapshot, request(name)?, resources, work).await?),
                None => None,
            };
            let stage = configuration::native_filter_with(filter, work).await?;
            let prepared =
                Arc::new(PreparedKuromojiFilter::new_with(&stage, profile.clone(), work).await?);
            let expanded = configuration::freeze_with(filter, profile.as_deref(), work).await?;
            if configuration::dictionary_name(filter)?.is_some() {
                resolved
                    .dictionaries
                    .push(profile.ok_or(AnalysisError::Descriptor(
                        "missing resolved Japanese resource",
                    ))?);
            }
            if expanded {
                crate::descriptor::check_configuration_with(config, limits, work).await?;
            }
            resolved.filters.push(Some(prepared));
        }
        if let Some(UnicodeProfile::Kuromoji { dictionary }) = config
            .normalization
            .as_mut()
            .and_then(|value| value.profile_mut())
        {
            let profile = load(&mut snapshot, request(dictionary)?, resources, work).await?;
            resolved.dictionaries.push(profile.clone());
            *dictionary = format!("sha256:{}", profile.sha256());
            resolved.normalizer = Some(profile);
        }
        crate::work::ordering::unique::<_, _, crate::AnalysisError>(
            &mut resolved.dictionaries,
            |item| item.sha256(),
            work,
        )
        .await?;
        Ok(resolved)
    }

    pub(crate) fn filter(
        &self,
        index: usize,
        filter: &TokenFilter,
    ) -> AnalysisResult<Option<Arc<PreparedKuromojiFilter>>> {
        if !is_japanese_filter(filter) {
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

pub(crate) fn prepare_filter(
    filter: &TokenFilter,
    resources: &KuromojiResources,
) -> AnalysisResult<Arc<PreparedKuromojiFilter>> {
    let profile = configuration::dictionary_name(filter)?
        .map(|name| resources.load(&request(name)?).map_err(AnalysisError::from))
        .transpose()?;
    let stage = configuration::native_filter(filter)?;
    Ok(Arc::new(PreparedKuromojiFilter::new(&stage, profile)?))
}

pub(crate) fn request(name: &str) -> AnalysisResult<DictionaryRequest> {
    Ok(match name.strip_prefix("sha256:") {
        Some(hash) => DictionaryRequest::Sha256(hash.parse()?),
        None => DictionaryRequest::Name(name.into()),
    })
}

async fn load(
    snapshot: &mut Snapshot<DictionaryRequest, ResolvedDictionary>,
    request: DictionaryRequest,
    resources: &impl Source,
    work: &mut impl crate::morphology::work::Work,
) -> AnalysisResult<Arc<ResolvedDictionary>> {
    Ok(snapshot.load(request,
        |request, dictionary| matches!(request, DictionaryRequest::Sha256(hash) if *hash == dictionary.sha256()),
        resources, work,
    ).await?)
}

pub(crate) fn check_resolved(config: &Analyzer) -> AnalysisResult<()> {
    crate::work::inline(check_resolved_with(config, &mut ()))
}

pub(crate) async fn check_resolved_with(
    config: &Analyzer,
    work: &mut impl crate::work::Work<AnalysisError>,
) -> AnalysisResult<()> {
    for (index, filter) in config.token_filters.iter().enumerate() {
        work.records(index).await?;
        configuration::check_resolved(filter)?;
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
    let Some(hash) = dictionary.strip_prefix("sha256:") else {
        return Err(AnalysisError::Descriptor(
            "resolved Japanese resources require exact artifact hashes",
        ));
    };
    let hash = hash.parse::<super::ResourceHash>()?;
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
    crate::work::inline(prepare_tokenizer_with(
        config,
        dictionary,
        resources,
        &mut (),
    ))
}

async fn prepare_tokenizer_with(
    config: &KuromojiTokenizerConfig,
    dictionary: &ResolvedDictionary,
    resources: &impl Source,
    work: &mut impl crate::morphology::work::Work,
) -> AnalysisResult<JapaneseTokenizer> {
    let user = match config.user_dictionary.as_deref() {
        Some(source) => {
            resources
                .compile_user(source, dictionary.model(), work)
                .await?
        }
        None => None,
    };
    let tokenizer = JapaneseTokenizer::new(dictionary.model().clone(), user, config.options())?;
    match config.n_best_examples.as_deref() {
        Some(examples) => tokenizer.with_n_best_examples(examples),
        None => Ok(tokenizer),
    }
}
