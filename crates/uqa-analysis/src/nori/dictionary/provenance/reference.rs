//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Required source identities; declared URLs are never resolved by the loader.

use serde_json::Value;

use crate::nori::error::invalid;
use crate::nori::DictionaryResult;

use crate::morphology::manifest::{equal_with, hex, inventory_with, pinned_digest_with, text};
use crate::morphology::work::Work;

pub(super) async fn validate_with(json: &Value, work: &mut impl Work) -> DictionaryResult<()> {
    hex(&json["exporter_sha256"], 64)?;
    inventory_with(&json["files"], "path", super::FILES, work).await?;
    let reference = &json["reference"];
    text(&reference["lucene_version"])?;
    hex(&reference["lucene_commit"], 40)?;
    pinned_digest_with(&reference["docker_image"], work).await?;
    let runtime = &reference["runtime"];
    for key in ["java_version", "java_runtime_version", "java_vendor"] {
        text(&runtime[key])?;
    }
    if !equal_with(runtime, &json["model"]["runtime"], work).await? {
        return Err(invalid("provenance", "reference and model runtimes differ"));
    }
    inventory_with(
        &reference["jars"],
        "artifact",
        &[
            "lucene-core",
            "lucene-analysis-common",
            "lucene-analysis-nori",
        ],
        work,
    )
    .await?;
    for jar in reference["jars"].as_array().expect("inventory validated") {
        text(&jar["url"])?;
    }
    inventory_with(
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
        work,
    )
    .await?;
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
