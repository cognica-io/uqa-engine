//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Required source identities; declared URLs are never resolved by the loader.

use serde_json::Value;

use crate::kuromoji::error::invalid;
use crate::kuromoji::DictionaryResult;

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
            "lucene-analysis-kuromoji",
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
            "org/apache/lucene/analysis/ja/dict/CharacterDefinition.dat",
            "org/apache/lucene/analysis/ja/dict/ConnectionCosts.dat",
            "org/apache/lucene/analysis/ja/dict/TokenInfoDictionary$buffer.dat",
            "org/apache/lucene/analysis/ja/dict/TokenInfoDictionary$fst.dat",
            "org/apache/lucene/analysis/ja/dict/TokenInfoDictionary$posDict.dat",
            "org/apache/lucene/analysis/ja/dict/TokenInfoDictionary$targetMap.dat",
            "org/apache/lucene/analysis/ja/dict/UnknownDictionary$buffer.dat",
            "org/apache/lucene/analysis/ja/dict/UnknownDictionary$posDict.dat",
            "org/apache/lucene/analysis/ja/dict/UnknownDictionary$targetMap.dat",
        ],
        work,
    )
    .await?;
    inventory_with(
        &reference["analysis_resources"],
        "path",
        &[
            "org/apache/lucene/analysis/ja/stopwords.txt",
            "org/apache/lucene/analysis/ja/stoptags.txt",
            "org/apache/lucene/analysis/ja/completion/romaji_map.txt",
        ],
        work,
    )
    .await?;
    for name in ["dictionary_patch", "generation_recipe"] {
        let input = &reference[name];
        text(&input["url"])?;
        hex(&input["sha256"], 64)?;
        if input["bytes"].as_u64().is_none_or(|bytes| bytes == 0) {
            return Err(invalid("provenance", "missing generation input size"));
        }
    }
    let patch = &reference["dictionary_patch"];
    if patch["path"] != "Noun.proper.csv.patch" || patch["target"] != "Noun.proper.csv" {
        return Err(invalid("provenance", "unsupported dictionary patch target"));
    }
    hex(&patch["git_blob"], 40)?;
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
