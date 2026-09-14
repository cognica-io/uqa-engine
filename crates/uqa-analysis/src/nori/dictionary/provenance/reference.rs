//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Required source identities; declared URLs are never resolved by the loader.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::nori::error::invalid;
use crate::nori::DictionaryResult;

fn text(value: &Value) -> DictionaryResult<&str> {
    value
        .as_str()
        .filter(|text| !text.is_empty())
        .ok_or_else(|| invalid("provenance", "missing nonempty identity string"))
}

fn hex(value: &Value, length: usize) -> DictionaryResult<()> {
    let value = text(value)?;
    if value.len() != length
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid(
            "provenance",
            "invalid hexadecimal digest or commit",
        ));
    }
    Ok(())
}

fn inventory(value: &Value, key: &str, expected: &[&str]) -> DictionaryResult<()> {
    let entries = value
        .as_array()
        .ok_or_else(|| invalid("provenance", "missing inventory"))?;
    let mut names = BTreeSet::new();
    for entry in entries {
        let name = text(&entry[key])?;
        if !names.insert(name) || entry["bytes"].as_u64().is_none_or(|bytes| bytes == 0) {
            return Err(invalid(
                "provenance",
                "duplicate identity or invalid byte count",
            ));
        }
        hex(&entry["sha256"], 64)?;
    }
    if names != expected.iter().copied().collect() {
        return Err(invalid(
            "provenance",
            "inventory differs from expected resources",
        ));
    }
    Ok(())
}

pub(super) fn validate(json: &Value) -> DictionaryResult<()> {
    hex(&json["exporter_sha256"], 64)?;
    inventory(&json["files"], "path", super::FILES)?;
    let reference = &json["reference"];
    text(&reference["lucene_version"])?;
    hex(&reference["lucene_commit"], 40)?;
    let image = text(&reference["docker_image"])?;
    let (_, digest) = image
        .split_once("@sha256:")
        .filter(|(name, _)| !name.is_empty())
        .ok_or_else(|| invalid("provenance", "Docker image is not digest pinned"))?;
    hex(&Value::String(digest.to_owned()), 64)?;
    let runtime = &reference["runtime"];
    for key in ["java_version", "java_runtime_version", "java_vendor"] {
        text(&runtime[key])?;
    }
    if runtime != &json["model"]["runtime"] {
        return Err(invalid("provenance", "reference and model runtimes differ"));
    }
    inventory(
        &reference["jars"],
        "artifact",
        &[
            "lucene-core",
            "lucene-analysis-common",
            "lucene-analysis-nori",
        ],
    )?;
    for jar in reference["jars"].as_array().expect("inventory validated") {
        text(&jar["url"])?;
    }
    inventory(
        &reference["dictionary_resources"],
        "path",
        &[
            "org/apache/lucene/analysis/ko/dict/CharacterDefinition.dat",
            "org/apache/lucene/analysis/ko/dict/ConnectionCosts.dat",
            "org/apache/lucene/analysis/ko/dict/TokenInfoDictionary$buffer.dat",
            "org/apache/lucene/analysis/ko/dict/TokenInfoDictionary$fst.dat",
            "org/apache/lucene/analysis/ko/dict/TokenInfoDictionary$posDict.dat",
            "org/apache/lucene/analysis/ko/dict/TokenInfoDictionary$targetMap.dat",
            "org/apache/lucene/analysis/ko/dict/UnknownDictionary$buffer.dat",
            "org/apache/lucene/analysis/ko/dict/UnknownDictionary$posDict.dat",
            "org/apache/lucene/analysis/ko/dict/UnknownDictionary$targetMap.dat",
        ],
    )?;
    let source = &reference["dictionary_source"];
    for key in ["name", "url", "license", "license_file"] {
        text(&source[key])?;
    }
    hex(&source["sha256"], 64)?;
    if !source["lucene_normalize_entries"].is_boolean() {
        return Err(invalid(
            "provenance",
            "missing dictionary normalization setting",
        ));
    }
    Ok(())
}
