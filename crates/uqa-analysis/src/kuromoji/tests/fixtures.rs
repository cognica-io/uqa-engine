//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Synthetic bundles exercise validation without asserting upstream provenance.

use crate::kuromoji::analysis::{AnalysisData, CompletionMapping};
use crate::kuromoji::frame::{self, Section};
use crate::kuromoji::morphology::{encode_words, Morphology, WordEntry, ABSENT};
use crate::kuromoji::provenance::{canonical, FILES};
use crate::kuromoji::tables::{Characters, CLASSES};
use crate::kuromoji::{DictionaryLimits, DictionaryResult, SurfaceWords};
use crate::morphology::io::Writer;
use crate::morphology::lexicon::Builder;
use crate::morphology::matrix::Matrix;
use crate::morphology::unicode::{Properties, UnicodeRange, UnicodeTable, CODE_POINTS};
use serde_json::{json, Value};

fn section(
    kind: u32,
    records: u64,
    write: impl FnOnce(&mut Writer) -> DictionaryResult<()>,
) -> Section {
    let mut output = Writer::default();
    write(&mut output).unwrap();
    Section {
        kind,
        records,
        bytes: output.0,
    }
}

pub(super) fn sections() -> Vec<Section> {
    let morphology = Morphology {
        strings: [
            "名詞-一般",
            "レイワ",
            "動詞-自立",
            "食べる",
            "タベタ",
            "一段",
            "連用タ接続",
            "",
        ]
        .map(str::to_owned)
        .into(),
    };
    vec![
        lexical_section(),
        word_section(),
        section(3, 8, |output| morphology.encode(output)),
        section(4, 1, |output| {
            Matrix {
                forward: 1,
                backward: 1,
                costs: vec![0],
            }
            .encode(output)
            .map_err(Into::into)
        }),
        character_section(),
        unicode_section(),
        section(7, 1, |output| {
            AnalysisData {
                stop_words: vec!["は".into()],
                stop_tags: vec!["助詞".into()],
                completion: vec![CompletionMapping {
                    key: "レ".into(),
                    alternatives: vec!["re".into()],
                }],
            }
            .encode(output)
        }),
        Section {
            kind: 8,
            records: 1,
            bytes: canonical(&provenance()).unwrap(),
        },
    ]
}

fn lexical_section() -> Section {
    section(1, 2, |output| {
        let mut builder = Builder::new();
        builder.insert("令和".encode_utf16().collect())?;
        builder.insert("食べた".encode_utf16().collect())?;
        let lexicon = builder.finish(16)?;
        let surfaces = [
            SurfaceWords {
                source_id: 1,
                word_ids: 0..1,
            },
            SurfaceWords {
                source_id: 0,
                word_ids: 1..2,
            },
        ];
        crate::morphology::surfaces::encode(&lexicon, &surfaces, output).map_err(Into::into)
    })
}

fn word_section() -> Section {
    let mut words: Vec<_> = (0..14)
        .map(|index| WordEntry {
            original_id: if index < 2 {
                index * 7
            } else {
                (index - 2) * 7
            },
            left: 0,
            right: 0,
            cost: -123,
            attributes: [0, ABSENT, ABSENT, ABSENT, ABSENT, ABSENT],
        })
        .collect();
    words[0].attributes = [0, ABSENT, 1, 7, ABSENT, ABSENT];
    words[1].attributes = [2, 3, 4, 4, 5, 6];
    section(2, 14, |output| encode_words(&words, 2, output))
}

fn character_section() -> Section {
    section(5, 65536, |output| {
        Characters {
            flags: vec![2; 12],
            words: (2..14).map(|id| id..id + 1).collect(),
            values: vec![[1, 0]; 65536],
        }
        .encode(output)
    })
}

fn unicode_section() -> Section {
    let table = UnicodeTable {
        ranges: [(0xd800, 0), (0xe000, 19), (CODE_POINTS, 0)]
            .map(|(end, category)| UnicodeRange {
                end,
                properties: Properties {
                    category,
                    flags: 0,
                    script: 2,
                    lowercase_delta: 0,
                },
            })
            .into(),
    };
    section(6, u64::from(CODE_POINTS), |output| {
        table.encode(output).map_err(Into::into)
    })
}

pub(super) fn bundle() -> Vec<u8> {
    frame::encode(&sections(), DictionaryLimits::default()).unwrap()
}

pub(super) fn provenance() -> Value {
    let hash = "0".repeat(64);
    let jars: Vec<_> = ["lucene-core", "lucene-analysis-common", "lucene-analysis-kuromoji"].iter().map(|artifact| json!({
        "artifact": artifact, "url": "https://example.invalid/synthetic.jar", "bytes": 1, "sha256": hash,
    })).collect();
    let resources: Vec<_> = ["CharacterDefinition.dat", "ConnectionCosts.dat", "TokenInfoDictionary$buffer.dat",
        "TokenInfoDictionary$fst.dat", "TokenInfoDictionary$posDict.dat", "TokenInfoDictionary$targetMap.dat",
        "UnknownDictionary$buffer.dat", "UnknownDictionary$posDict.dat", "UnknownDictionary$targetMap.dat"].iter().map(|name| json!({
            "path": format!("org/apache/lucene/analysis/ja/dict/{name}"), "bytes": 1, "sha256": hash,
        })).collect();
    let files: Vec<_> = FILES
        .iter()
        .map(|path| json!({ "path": path, "bytes": 1, "sha256": hash }))
        .collect();
    let analysis_resources =
        ["stoptags.txt", "stopwords.txt", "completion/romaji_map.txt"].map(|name| {
            json!({
                "path": format!("org/apache/lucene/analysis/ja/{name}"), "bytes": 1, "sha256": hash,
            })
        });
    let runtime = json!({"java_version": "synthetic", "java_runtime_version": "synthetic", "java_vendor": "synthetic fixture"});
    json!({
        "format": "uqa-kuromoji-neutral", "format_version": 1, "byte_order": "big", "exporter_sha256": hash,
        "reference": {
            "lucene_version": "synthetic fixture", "lucene_commit": "0".repeat(40),
            "docker_image": format!("synthetic-fixture@sha256:{hash}"), "runtime": runtime,
            "jars": jars, "dictionary_resources": resources,
            "analysis_resources": analysis_resources,
            "dictionary_patch": { "url": "https://example.invalid/synthetic", "bytes": 1, "sha256": hash,
                "path": "Noun.proper.csv.patch", "target": "Noun.proper.csv", "git_blob": "0".repeat(40) },
            "generation_recipe": { "url": "https://example.invalid/synthetic", "bytes": 1, "sha256": hash },
            "dictionary_source": { "name": "synthetic fixture", "url": "https://example.invalid/synthetic",
                "sha256": hash, "license": "AGPL-3.0-or-later", "license_file": "LICENSE",
                "lucene_normalize_entries": false },
        },
        "files": files,
        "model": {
            "runtime": runtime, "morphology_fields": ["part_of_speech", "base_form", "reading", "pronunciation", "inflection_type", "inflection_form"], "character_classes": CLASSES, "unicode_scripts": ["COMMON", "INHERITED", "UNKNOWN", "LATIN"],
            "surface_count": 2, "word_count": 2, "unknown_word_count": 12, "unknown_class_count": 12,
            "stop_word_count": 1, "stop_tag_count": 1, "completion_mapping_count": 1,
            "matrix_forward": 1, "matrix_backward": 1, "character_count": 65536, "unicode_count": CODE_POINTS,
        },
    })
}
