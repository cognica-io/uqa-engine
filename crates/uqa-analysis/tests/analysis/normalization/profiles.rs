//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use sha2::{Digest, Sha256};
use uqa_analysis::UnicodeProfile;

pub(super) fn plans() -> Vec<NormalizationConfig> {
    let profiles = [
        #[cfg(feature = "nori")]
        UnicodeProfile::Nori {
            dictionary: uqa_analysis::nori::DEFAULT_NORI_DICTIONARY.into(),
        },
        #[cfg(feature = "kuromoji")]
        UnicodeProfile::Kuromoji {
            dictionary: uqa_analysis::kuromoji::DEFAULT_KUROMOJI_DICTIONARY.into(),
        },
    ];
    profiles
        .into_iter()
        .flat_map(|profile| {
            [
                NormalizationConfig::UnicodeSimpleLowercase {
                    profile: profile.clone(),
                },
                NormalizationConfig::CJKWidthSimpleLowercase { profile },
            ]
        })
        .collect()
}

#[test]
fn explicit_profile_identity_and_order_restore_without_changing_analysis() {
    let mut fingerprints = Vec::new();
    for plan in plans() {
        let width = matches!(plan, NormalizationConfig::CJKWidthSimpleLowercase { .. });
        let config = keyword().with_normalization(plan);
        let original = serde_json::to_value(&config).unwrap();
        let compiled = config.compile().unwrap();
        assert_eq!(serde_json::to_value(&config).unwrap(), original);
        let wire: Value = serde_json::from_str(compiled.descriptor().canonical_json()).unwrap();
        let profile = &wire["descriptor"]["pipeline"]["normalization"]["profile"];
        let expected_hash = match profile["provider"].as_str().unwrap() {
            #[cfg(feature = "nori")]
            "nori" => uqa_nori_data::BUNDLE_SHA256,
            #[cfg(feature = "kuromoji")]
            "kuromoji" => uqa_kuromoji_data::BUNDLE_SHA256,
            provider => panic!("unexpected provider {provider}"),
        };
        assert_eq!(profile["dictionary"], format!("sha256:{expected_hash}"));
        assert_eq!(
            wire["descriptor"]["runtime_profiles"]["rust_unicode"],
            Value::Null
        );
        assert_eq!(
            wire["descriptor"]["runtime_profiles"]["normalization_unicode"],
            if width {
                json!(unicode_normalization::UNICODE_VERSION)
            } else {
                Value::Null
            }
        );
        let restored = AnalyzerResources::new(AnalyzerLimits::default())
            .restore_json(compiled.descriptor().canonical_json())
            .unwrap();
        assert_eq!(
            restored.normalize("ＵＱＡ ｶﾞ İ ΟΣ 𐐀").unwrap(),
            if width {
                "uqa ガ i οσ 𐐨"
            } else {
                "ｕｑａ ｶﾞ i οσ 𐐨"
            }
        );
        assert_eq!(
            restored.analyze("ＵＱＡ ｶﾞ İ ΟΣ 𐐀").unwrap(),
            ["ＵＱＡ ｶﾞ İ ΟΣ 𐐀"]
        );
        fingerprints.push(compiled.descriptor().fingerprint());
    }
    let count = fingerprints.len();
    fingerprints.sort();
    fingerprints.dedup();
    assert_eq!(fingerprints.len(), count);
}

fn rehash(wire: &mut Value) -> String {
    let mut hash = Sha256::new();
    hash.update(b"UQA analyzer descriptor\0");
    hash.update(serde_json::to_vec(&wire["descriptor"]).unwrap());
    wire["fingerprint"] = json!(format!("{:x}", hash.finalize()));
    wire.to_string()
}

#[test]
fn restoration_requires_canonical_exact_profiles_even_with_a_valid_fingerprint() {
    for plan in plans() {
        let compiled = keyword().with_normalization(plan).compile().unwrap();
        let original: Value = serde_json::from_str(compiled.descriptor().canonical_json()).unwrap();
        let exact = original["descriptor"]["pipeline"]["normalization"]["profile"]["dictionary"]
            .as_str()
            .unwrap();
        for (pointer, replacement) in [
            (
                "/descriptor/pipeline/normalization/profile/dictionary",
                json!("lucene-10.5.1"),
            ),
            (
                "/descriptor/pipeline/normalization/profile/dictionary",
                json!(exact.to_uppercase().replace("SHA256:", "sha256:")),
            ),
            (
                "/descriptor/pipeline/normalization/profile/dictionary",
                json!(format!("sha256:{}", "0".repeat(64))),
            ),
            (
                "/descriptor/pipeline/normalization/profile/dictionary",
                Value::Null,
            ),
        ] {
            let mut wire = original.clone();
            *wire.pointer_mut(pointer).unwrap() = replacement;
            let resources = AnalyzerResources::new(AnalyzerLimits::default());
            assert!(resources.restore_json(&rehash(&mut wire)).is_err());
            assert_eq!(resources.cache_stats().analyzers, 0);
        }
        let mut wire = original;
        // A correct rehash of the unmodified descriptor proves failures reach profile validation.
        assert!(AnalyzerResources::new(AnalyzerLimits::default())
            .restore_json(&rehash(&mut wire))
            .is_ok());
    }
}

#[cfg(feature = "nori")]
#[test]
fn explicit_unavailable_and_width_plans_override_legacy_korean_inference() {
    let config = uqa_analysis::nori::nori_analyzer();
    let legacy = config.compile().unwrap();
    assert!(!legacy
        .descriptor()
        .canonical_json()
        .contains("\"normalization\":"));
    assert_eq!(legacy.normalize("ＵＱＡ ｶﾞ İ").unwrap(), "ｕｑａ ｶﾞ i");
    for plan in [
        NormalizationConfig::Unavailable,
        NormalizationConfig::CJKWidth,
    ] {
        let width = plan == NormalizationConfig::CJKWidth;
        let changed = config.clone().with_normalization(plan).compile().unwrap();
        assert_eq!(
            changed.analyze_tokens("한국어 UQA").unwrap(),
            legacy.analyze_tokens("한국어 UQA").unwrap()
        );
        if width {
            assert_eq!(changed.normalize("ＵＱＡ ｶﾞ İ").unwrap(), "UQA ガ İ");
        } else {
            assert!(matches!(
                changed.normalize("ＵＱＡ"),
                Err(AnalysisError::NormalizationUnavailable)
            ));
        }
    }
}

#[cfg(feature = "kuromoji")]
#[test]
fn common_normalization_plans_match_all_pinned_japanese_normalization_observations() {
    let ordinary = keyword()
        .with_normalization(NormalizationConfig::CJKWidthSimpleLowercase {
            profile: UnicodeProfile::Kuromoji {
                dictionary: uqa_analysis::kuromoji::DEFAULT_KUROMOJI_DICTIONARY.into(),
            },
        })
        .compile()
        .unwrap();
    let completion = keyword()
        .with_normalization(NormalizationConfig::CJKWidth)
        .compile()
        .unwrap();
    for (cases, expected, compiled, kind, count) in [
        (
            include_str!("../../../../../tests/parity/kuromoji/filter_cases.json"),
            include_str!("../../../../../tests/parity/kuromoji/filter_expected.jsonl"),
            ordinary,
            "normalize",
            14,
        ),
        (
            include_str!("../../../../../tests/parity/kuromoji/completion_cases.json"),
            include_str!("../../../../../tests/parity/kuromoji/completion_expected.jsonl"),
            completion,
            "completion_normalize",
            12,
        ),
    ] {
        let cases: Vec<Value> = serde_json::from_str(cases).unwrap();
        let mut verified = 0;
        for (case, expected) in cases.iter().zip(expected.lines()) {
            if case["kind"] != kind {
                continue;
            }
            let expected: Value = serde_json::from_str(expected).unwrap();
            assert_eq!(case["id"], expected["id"]);
            let output = compiled.normalize(case["input"].as_str().unwrap()).unwrap();
            assert_eq!(
                json!(output.encode_utf16().collect::<Vec<_>>()),
                expected["normalized_utf16"],
                "{}",
                case["id"]
            );
            verified += 1;
        }
        assert_eq!(verified, count);
    }
}
