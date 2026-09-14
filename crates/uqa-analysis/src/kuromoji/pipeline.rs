//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Japanese pipeline resources are frozen before descriptor publication.

use super::{
    DictionaryRequest, JapaneseTokenizer, KuromojiResources, KuromojiTokenizerConfig,
    ResolvedDictionary,
};
use crate::morphology::resources::Snapshot;
use crate::{AnalysisError, AnalysisResult, Analyzer, Tokenizer, UnicodeProfile};
use std::sync::Arc;

#[cfg(test)]
mod tests;

#[derive(Default)]
pub(crate) struct ResolvedKuromojiPipeline {
    pub tokenizer: Option<JapaneseTokenizer>,
    pub normalizer: Option<Arc<ResolvedDictionary>>,
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
