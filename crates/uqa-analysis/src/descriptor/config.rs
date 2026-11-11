//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve fixed filter data and require explicit canonical pipeline defaults on restore.

use std::{collections::BTreeSet, io::Read};

use serde_json::Value;

use super::{
    canonical, invalid,
    limits::{check_limit, encode},
    AnalyzerLimits,
};
use crate::{AnalysisResult, Analyzer, SynonymFileError, TokenFilter};

pub(crate) fn check_config(config: &Analyzer, limits: AnalyzerLimits) -> AnalysisResult<()> {
    crate::work::inline(check_shape(config, limits, &mut ()))?;
    encode(config, limits.max_descriptor_bytes, false)?;
    Ok(())
}

async fn check_shape(
    config: &Analyzer,
    limits: AnalyzerLimits,
    work: &mut impl crate::work::Work<crate::AnalysisError>,
) -> AnalysisResult<()> {
    work.checkpoint().await?;
    #[cfg(all(feature = "kuromoji", not(feature = "nori")))]
    for (index, filter) in config.token_filters.iter().enumerate() {
        work.records(index).await?;
        if let TokenFilter::UnicodeSimpleLowercase(config) = filter {
            config.unicode_profile.validate_features()?;
        }
    }
    let count = config
        .char_filters
        .len()
        .checked_add(config.token_filters.len())
        .and_then(|count| count.checked_add(1))
        .and_then(|count| {
            count.checked_add(
                config
                    .normalization
                    .as_ref()
                    .map_or(0, crate::NormalizationConfig::stage_count),
            )
        })
        .ok_or_else(|| invalid("stage count overflow"))?;
    check_limit("analyzer stages", count, limits.max_stages)?;
    Ok(())
}

pub(super) fn snapshot(config: &Analyzer, limits: AnalyzerLimits) -> AnalysisResult<Value> {
    let mut resolved = config.clone();
    #[cfg(feature = "nori")]
    crate::nori::pipeline::canonicalize(&mut resolved);
    #[cfg(feature = "kuromoji")]
    crate::kuromoji::pipeline::canonicalize(&mut resolved);
    for filter in &mut resolved.token_filters {
        match filter {
            #[cfg(feature = "kuromoji")]
            TokenFilter::KuromojiPartOfSpeech(_)
            | TokenFilter::KuromojiStop(_)
            | TokenFilter::KuromojiCompletion(_) => {}
            #[cfg(feature = "nori")]
            TokenFilter::NoriPartOfSpeech(_)
            | TokenFilter::NoriReadingForm(_)
            | TokenFilter::NoriNumber(_) => {}
            #[cfg(any(feature = "nori", feature = "kuromoji"))]
            TokenFilter::UnicodeSimpleLowercase(_) => {}
            TokenFilter::Stop {
                language,
                custom_words,
            } => {
                let mut words: BTreeSet<String> = crate::token_filter::builtin_stop_words(language)
                    .iter()
                    .map(|word| (*word).into())
                    .collect();
                words.extend(std::mem::take(custom_words));
                language.clear();
                *custom_words = words.into_iter().collect();
            }
            TokenFilter::Synonym {
                synonyms,
                synonyms_path,
            } => {
                if let Some(path) = synonyms_path.take() {
                    let file = std::fs::File::open(&path)
                        .map_err(|source| synonym_error(&path, source))?;
                    let mut bytes = Vec::new();
                    file.take((limits.max_descriptor_bytes as u64).saturating_add(1))
                        .read_to_end(&mut bytes)
                        .map_err(|source| synonym_error(&path, source))?;
                    check_limit(
                        "synonym source bytes",
                        bytes.len(),
                        limits.max_descriptor_bytes,
                    )?;
                    let body = std::str::from_utf8(&bytes).map_err(|source| {
                        synonym_error(
                            &path,
                            std::io::Error::new(std::io::ErrorKind::InvalidData, source),
                        )
                    })?;
                    *synonyms = crate::token_filter::parse_synonym_body_bounded(
                        body,
                        limits.max_descriptor_bytes,
                    )?;
                }
            }
            _ => {
                filter.validate()?;
            }
        }
    }
    config_value(&resolved)
}

fn synonym_error(path: &std::path::Path, source: std::io::Error) -> SynonymFileError {
    if source.kind() == std::io::ErrorKind::NotFound {
        SynonymFileError::NotFound(path.into())
    } else {
        SynonymFileError::Io {
            path: path.into(),
            source,
        }
    }
}

pub(super) async fn restore(
    value: &Value,
    limits: AnalyzerLimits,
    work: &mut impl crate::work::Work<crate::AnalysisError>,
) -> AnalysisResult<Analyzer> {
    let config = crate::configuration::decode(value, work).await?;
    check_shape(&config, limits, work).await?;
    crate::configuration::check_encoded_size(&config, limits.max_descriptor_bytes, work).await?;
    #[cfg(feature = "nori")]
    crate::nori::pipeline::check_resolved_with(&config, work).await?;
    #[cfg(feature = "kuromoji")]
    crate::kuromoji::pipeline::check_resolved_with(&config, work).await?;
    for (index, filter) in config.token_filters.iter().enumerate() {
        work.records(index).await?;
        if matches!(
            filter,
            TokenFilter::Synonym {
                synonyms_path: Some(_),
                ..
            }
        ) {
            return Err(invalid("resolved synonyms must not contain a file path"));
        }
    }
    let canonical = canonical_fields(&config, work).await?;
    if !canonical || !crate::configuration::matches_snapshot(&config, value, work).await? {
        return Err(invalid(
            "pipeline contains unknown properties, unresolved values, or implicit defaults",
        ));
    }
    Ok(config)
}

async fn canonical_fields(
    config: &Analyzer,
    work: &mut impl crate::work::Work<crate::AnalysisError>,
) -> AnalysisResult<bool> {
    let mut canonical = true;
    for (index, filter) in config.token_filters.iter().enumerate() {
        work.records(index).await?;
        match filter {
            TokenFilter::Stop {
                language,
                custom_words,
            } => {
                canonical &= language.is_empty() && sorted_words(custom_words, work).await?;
            }
            TokenFilter::Synonym { .. } => (),
            #[cfg(feature = "nori")]
            TokenFilter::NoriPartOfSpeech(config) => {
                if let Some(tags) = &config.stop_tags {
                    for (index, pair) in tags.windows(2).enumerate() {
                        work.records(index).await?;
                        canonical &= pair[0].ordinal() < pair[1].ordinal();
                    }
                } else {
                    canonical = false;
                }
            }
            #[cfg(feature = "kuromoji")]
            TokenFilter::KuromojiPartOfSpeech(config) => {
                if let Some(words) = &config.stop_tags {
                    canonical &= sorted_words(words, work).await?;
                }
            }
            #[cfg(feature = "kuromoji")]
            TokenFilter::KuromojiStop(config) => {
                if let Some(words) = &config.words {
                    canonical &= sorted_words(words, work).await?;
                }
            }
            #[cfg(feature = "kuromoji")]
            TokenFilter::KuromojiCompletion(_) => (),
            #[cfg(feature = "nori")]
            TokenFilter::NoriReadingForm(_) | TokenFilter::NoriNumber(_) => (),
            #[cfg(any(feature = "nori", feature = "kuromoji"))]
            TokenFilter::UnicodeSimpleLowercase(_) => (),
            _ => filter.validate()?,
        }
    }
    Ok(canonical)
}

async fn sorted_words(
    words: &[String],
    work: &mut impl crate::work::Work<crate::AnalysisError>,
) -> AnalysisResult<bool> {
    for (index, pair) in words.windows(2).enumerate() {
        work.records(index).await?;
        if crate::json::bytes_compare(pair[0].as_bytes(), pair[1].as_bytes(), work).await?
            != std::cmp::Ordering::Less
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn config_value(config: &Analyzer) -> AnalysisResult<Value> {
    let mut value = serde_json::to_value(config)?;
    let filters = value["token_filters"]
        .as_array_mut()
        .expect("serialized token filters");
    for (value, filter) in filters.iter_mut().zip(&config.token_filters) {
        if let TokenFilter::Synonym { synonyms, .. } = filter {
            let object = value.as_object_mut().expect("serialized filter");
            object.insert("synonyms".into(), serde_json::to_value(synonyms)?);
            object.insert("synonyms_path".into(), Value::Null);
        }
    }
    Ok(canonical(&value))
}

#[cfg(test)]
mod tests;
