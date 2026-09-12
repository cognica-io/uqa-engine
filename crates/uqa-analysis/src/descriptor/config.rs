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

pub(super) fn check_config(config: &Analyzer, limits: AnalyzerLimits) -> AnalysisResult<()> {
    let count = config
        .char_filters
        .len()
        .checked_add(config.token_filters.len())
        .and_then(|count| count.checked_add(1))
        .ok_or_else(|| invalid("stage count overflow"))?;
    check_limit("analyzer stages", count, limits.max_stages)?;
    encode(config, limits.max_descriptor_bytes, false)?;
    Ok(())
}

pub(super) fn snapshot(config: &Analyzer, limits: AnalyzerLimits) -> AnalysisResult<Value> {
    let mut resolved = config.clone();
    for filter in &mut resolved.token_filters {
        match filter {
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

pub(super) fn restore(value: &Value, limits: AnalyzerLimits) -> AnalysisResult<Analyzer> {
    let config: Analyzer = serde_json::from_value(value.clone())?;
    check_config(&config, limits)?;
    for filter in &config.token_filters {
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
    if canonical(value) != snapshot(&config, limits)? {
        return Err(invalid(
            "pipeline contains unknown properties, unresolved values, or implicit defaults",
        ));
    }
    Ok(config)
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
