//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Named [`Analyzer`] registry. Built-in entries (`whitespace`, `standard`,
//! `standard_cjk`, `keyword`) are immutable. Users register custom
//! analyzers under any other name; built-in names cannot be overwritten or
//! dropped.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use parking_lot::RwLock;

use crate::analyzer::{
    keyword_analyzer, standard_analyzer, standard_cjk_analyzer, whitespace_analyzer, Analyzer,
};

/// Built-in default analyzer name.
pub const DEFAULT_ANALYZER_NAME: &str = "standard";

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("cannot overwrite built-in analyzer: {0:?}")]
    OverwriteBuiltin(String),
    #[error("cannot drop built-in analyzer: {0:?}")]
    DropBuiltin(String),
    #[error("analyzer does not exist: {0:?}")]
    NotFound(String),
}

fn builtins() -> &'static BTreeMap<&'static str, Analyzer> {
    static B: OnceLock<BTreeMap<&'static str, Analyzer>> = OnceLock::new();
    B.get_or_init(|| {
        let mut m = BTreeMap::new();
        m.insert("whitespace", whitespace_analyzer());
        m.insert("standard", standard_analyzer("english"));
        m.insert("standard_cjk", standard_cjk_analyzer("english"));
        m.insert("keyword", keyword_analyzer());
        m
    })
}

fn custom() -> &'static RwLock<BTreeMap<String, Analyzer>> {
    static C: OnceLock<RwLock<BTreeMap<String, Analyzer>>> = OnceLock::new();
    C.get_or_init(|| RwLock::new(BTreeMap::new()))
}

pub fn register_analyzer(name: impl Into<String>, analyzer: Analyzer) -> Result<(), RegistryError> {
    let name = name.into();
    if builtins().contains_key(name.as_str()) {
        return Err(RegistryError::OverwriteBuiltin(name));
    }
    custom().write().insert(name, analyzer);
    Ok(())
}

pub fn get_analyzer(name: &str) -> Result<Analyzer, RegistryError> {
    if let Some(a) = custom().read().get(name) {
        return Ok(a.clone());
    }
    if let Some(a) = builtins().get(name) {
        return Ok(a.clone());
    }
    Err(RegistryError::NotFound(name.to_string()))
}

pub fn drop_analyzer(name: &str) -> Result<(), RegistryError> {
    if builtins().contains_key(name) {
        return Err(RegistryError::DropBuiltin(name.to_string()));
    }
    if custom().write().remove(name).is_none() {
        return Err(RegistryError::NotFound(name.to_string()));
    }
    Ok(())
}

pub fn list_analyzers() -> Vec<String> {
    let mut names: Vec<String> = builtins().keys().map(|s| (*s).to_string()).collect();
    names.extend(custom().read().keys().cloned());
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_names_resolvable() {
        for name in ["whitespace", "standard", "standard_cjk", "keyword"] {
            assert!(get_analyzer(name).is_ok(), "missing builtin: {name}");
        }
    }

    #[test]
    fn cannot_overwrite_builtin() {
        let err = register_analyzer("standard", whitespace_analyzer()).unwrap_err();
        assert!(matches!(err, RegistryError::OverwriteBuiltin(_)));
    }

    #[test]
    fn register_get_drop_custom() {
        register_analyzer("test_custom_alpha", whitespace_analyzer()).unwrap();
        assert!(get_analyzer("test_custom_alpha").is_ok());
        drop_analyzer("test_custom_alpha").unwrap();
        assert!(matches!(
            get_analyzer("test_custom_alpha"),
            Err(RegistryError::NotFound(_))
        ));
    }

    #[test]
    fn cannot_drop_builtin() {
        let err = drop_analyzer("standard").unwrap_err();
        assert!(matches!(err, RegistryError::DropBuiltin(_)));
    }
}
