//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deliberately synthetic resources: no fixture claims upstream dictionary provenance.

use serde_json::{json, Value};

use crate::nori::dictionary::provenance::{canonical, FILES};
use crate::nori::dictionary::tables::{Characters, Matrix, CLASSES};
use crate::nori::frame::{self, Section};
use crate::nori::io::Writer;
use crate::nori::lexicon::Builder;
use crate::nori::morphology::{encode_words, Morpheme, Morphology, WordEntry, ABSENT};
use crate::nori::unicode::{Properties, UnicodeRange, UnicodeTable, CODE_POINTS};
use crate::nori::{DictionaryLimits, DictionaryResult, POSTag, POSType};

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

pub(in crate::nori) fn sections() -> Vec<Section> {
    let lexicon = section(1, 5, |output| {
        let mut builder = Builder::new();
        for text in ["", "가", "가나", "😀", "\u{e000}"] {
            builder.insert(text.encode_utf16().collect())?;
        }
        builder.finish(16)?.encode(output)?;
        output.u32(5)?;
        // Zigzag deltas encode source IDs [2, 0, 4, 1, 3], independently of lexical rank.
        for (delta, count) in [(4, 1), (3, 2), (8, 1), (5, 1), (4, 1)] {
            output.var_u32(delta)?;
            output.var_u32(count)?;
        }
        Ok(())
    });
    let mut words: Vec<_> = (0..20)
        .map(|index| WordEntry {
            original_id: if index < 6 {
                [0, 24, 3, 57, 81, 104][index]
            } else {
                (index - 6) as u32 * 7
            },
            left: 1,
            right: 2,
            cost: -123,
            pos_type: POSType::Morpheme,
            left_pos: POSTag::NNG,
            right_pos: POSTag::NNP,
            reading: ABSENT,
            morphemes: ABSENT,
            morpheme_count: 0,
        })
        .collect();
    words[1].pos_type = POSType::Compound;
    words[1].reading = 0;
    words[1].morphemes = 0;
    words[1].morpheme_count = 2;
    words[2].pos_type = POSType::Inflect;
    words[2].morphemes = 2;
    words[3].pos_type = POSType::Preanalysis;
    words[3].reading = 3;
    let words = section(2, 20, |output| encode_words(&words, 6, output));
    let morphology = Morphology {
        strings: ["", "가", "나", "한"].map(str::to_owned).into(),
        morphemes: vec![
            Morpheme {
                surface: 1,
                pos: POSTag::NNG,
            },
            Morpheme {
                surface: 2,
                pos: POSTag::NNP,
            },
        ],
    };
    let morphology = section(3, 2, |output| morphology.encode(output));
    let matrix = section(4, 6, |output| {
        Matrix {
            forward: 3,
            backward: 2,
            costs: vec![-10, 11, 12, 20, -21, 22],
        }
        .encode(output)
    });
    let characters = character_section();
    let unicode = UnicodeTable {
        ranges: [
            (0x41, 0, 2, 0),
            (0x5b, 1, 3, 32),
            (0xd800, 0, 2, 0),
            (0xe000, 19, 2, 0),
            (CODE_POINTS, 0, 2, 0),
        ]
        .map(|(end, category, script, lowercase_delta)| UnicodeRange {
            end,
            properties: Properties {
                category,
                flags: 0,
                script,
                lowercase_delta,
            },
        })
        .into(),
    };
    let unicode = section(6, u64::from(CODE_POINTS), |output| unicode.encode(output));
    let provenance = Section {
        kind: 7,
        records: 1,
        bytes: canonical(&provenance()).unwrap(),
    };
    vec![
        lexicon, words, morphology, matrix, characters, unicode, provenance,
    ]
}

pub(in crate::nori) fn bundle() -> Vec<u8> {
    frame::encode(&sections(), DictionaryLimits::default()).unwrap()
}

pub(in crate::nori) fn provenance() -> Value {
    let hash = "0".repeat(64);
    let jars: Vec<_> = ["lucene-core", "lucene-analysis-common", "lucene-analysis-nori"].iter().map(|artifact| json!({
        "artifact": artifact, "url": "https://example.invalid/synthetic.jar", "bytes": 1, "sha256": hash,
    })).collect();
    let resources: Vec<_> = ["CharacterDefinition.dat", "ConnectionCosts.dat", "TokenInfoDictionary$buffer.dat",
        "TokenInfoDictionary$fst.dat", "TokenInfoDictionary$posDict.dat", "TokenInfoDictionary$targetMap.dat",
        "UnknownDictionary$buffer.dat", "UnknownDictionary$posDict.dat", "UnknownDictionary$targetMap.dat"].iter().map(|name| json!({
            "path": format!("org/apache/lucene/analysis/ko/dict/{name}"), "bytes": 1, "sha256": hash,
        })).collect();
    let files: Vec<_> = FILES
        .iter()
        .map(|path| json!({ "path": path, "bytes": 1, "sha256": hash }))
        .collect();
    let tags: Vec<_> = (0..POSTag::NAMES.len())
        .map(|i| {
            let tag = POSTag::from_ordinal(i as u8).unwrap();
            json!({ "name": tag.name(), "code": tag.code() })
        })
        .collect();
    let runtime = json!({"java_version": "synthetic", "java_runtime_version": "synthetic", "java_vendor": "synthetic fixture"});
    json!({
        "format": "uqa-nori-neutral", "format_version": 1, "byte_order": "big", "exporter_sha256": hash,
        "reference": {
            "lucene_version": "synthetic fixture", "lucene_commit": "0".repeat(40),
            "docker_image": format!("synthetic-fixture@sha256:{hash}"), "runtime": runtime,
            "jars": jars, "dictionary_resources": resources,
            "dictionary_source": { "name": "synthetic fixture", "url": "https://example.invalid/synthetic",
                "sha256": hash, "license": "AGPL-3.0-or-later", "license_file": "LICENSE",
                "lucene_normalize_entries": false },
        },
        "files": files,
        "model": {
            "runtime": runtime, "pos_types": ["MORPHEME", "COMPOUND", "INFLECT", "PREANALYSIS"],
            "pos_tags": tags, "character_classes": CLASSES, "unicode_scripts": ["COMMON", "INHERITED", "UNKNOWN", "LATIN"],
            "surface_count": 5, "word_count": 6, "unknown_word_count": 14, "unknown_class_count": 14,
            "matrix_forward": 3, "matrix_backward": 2, "character_count": 65536, "unicode_count": CODE_POINTS,
        },
    })
}

fn character_section() -> Section {
    let mut flags = vec![2; 14];
    flags[5] = 3;
    let characters = Characters {
        flags,
        words: (6..20).map(|id| id..id + 1).collect(),
        values: (0..0x10000_i32)
            .map(|unit| {
                let class = match unit {
                    0x41 => 5,
                    0xd55c => 11,
                    0x6f22 => 12,
                    _ => 1,
                };
                let attributes = u8::from(class == 12 || class == 13)
                    | (u8::from(class == 11) << 1)
                    | (u8::from((unit - 0xac00) % 28 != 0) << 2);
                [class, attributes]
            })
            .collect(),
    };
    section(5, 65536, |output| characters.encode(output))
}
