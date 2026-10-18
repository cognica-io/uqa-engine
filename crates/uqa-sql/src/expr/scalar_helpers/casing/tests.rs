//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::CaseMapping;
use crate::catalog::collations::{BuiltinCollationCtype, DEFAULT_COLLATION_OID};
use std::fmt::Write as _;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!("tests/postgresql.json")).unwrap()
}

fn apply(mapping: CaseMapping, function: &str, text: &str) -> String {
    let control = ProductionControl::uncontrolled();
    let mapped = match function {
        "lower" => mapping.lowercase(text, &control),
        "upper" => mapping.uppercase(text, &control),
        "initcap" => mapping.titlecase(text, &control),
        "casefold" => mapping.fold(text, &control),
        _ => unreachable!("fixture function"),
    };
    mapped.unwrap().into_uncontrolled().unwrap()
}

#[test]
fn each_builtin_collation_selects_its_provider() {
    for (oid, ctype) in [
        (100, BuiltinCollationCtype::UnicodeFull),
        (811, BuiltinCollationCtype::UnicodeSimple),
        (950, BuiltinCollationCtype::Ascii),
        (951, BuiltinCollationCtype::Ascii),
        (962, BuiltinCollationCtype::Ascii),
        (963, BuiltinCollationCtype::IcuRoot),
        (6411, BuiltinCollationCtype::UnicodeFull),
    ] {
        assert_eq!(
            CaseMapping::select(oid, "lower() function").unwrap(),
            CaseMapping { ctype },
            "{oid}"
        );
    }
    for subject in [
        "lower() function",
        "upper() function",
        "initcap() function",
        "LIKE",
        "ILIKE",
    ] {
        let crate::SQLError::Diagnostic {
            sqlstate,
            message,
            detail,
            hint,
        } = CaseMapping::select(0, subject).unwrap_err()
        else {
            panic!("indeterminate collation diagnostic");
        };
        assert_eq!(sqlstate, "42P22");
        assert_eq!(
            message,
            format!("could not determine which collation to use for {subject}")
        );
        assert_eq!(detail, None);
        assert_eq!(
            hint.as_deref(),
            Some("Use the COLLATE clause to set the collation explicitly.")
        );
    }
    assert_eq!(
        CaseMapping::select(16_384, "LIKE").unwrap_err().to_string(),
        crate::SQLError::Internal("cache lookup failed for collation 16384".into()).to_string()
    );
}

/// Every single-character result of every provider agrees with `PostgreSQL` 18.4's exhaustive digest.
#[test]
fn single_character_mappings_match_postgresql_for_every_code_point() {
    let fixture = fixture();
    for entry in fixture["single_characters"].as_array().unwrap() {
        let collation = u32::try_from(entry["collation"].as_u64().unwrap()).unwrap();
        let function = entry["function"].as_str().unwrap();
        let mapping = CaseMapping::select(collation, "test").unwrap();
        let mut canonical = String::new();
        let mut changed = 0_u64;
        let mut input = String::new();
        for character in (1..=0x10_ffff).filter_map(char::from_u32) {
            input.clear();
            input.push(character);
            let output = apply(mapping, function, &input);
            if output == input {
                continue;
            }
            changed += 1;
            write!(canonical, "{:x}:", u32::from(character)).unwrap();
            for (index, mapped) in output.chars().enumerate() {
                if index > 0 {
                    canonical.push(' ');
                }
                write!(canonical, "{:x}", u32::from(mapped)).unwrap();
            }
            canonical.push(';');
        }
        assert_eq!(
            (changed, u64::from(crc32fast::hash(canonical.as_bytes()))),
            (
                entry["changed"].as_u64().unwrap(),
                entry["crc32"].as_u64().unwrap()
            ),
            "{function} under collation {collation}"
        );
    }
}

/// Word boundaries, word-start adjustment and `Final_Sigma` context agree with `PostgreSQL` 18.4 under each provider.
#[test]
fn contextual_mappings_match_postgresql() {
    let fixture = fixture();
    for entry in fixture["strings"].as_array().unwrap() {
        let input = entry["input"].as_str().unwrap();
        for (name, oid) in [
            ("unicode", 963),
            ("pg_unicode_fast", 6411),
            ("pg_c_utf8", 811),
            ("C", 950),
        ] {
            let mapping = CaseMapping::select(oid, "test").unwrap();
            for function in ["initcap", "lower"] {
                assert_eq!(
                    apply(mapping, function, input),
                    entry[name][function].as_str().unwrap(),
                    "{function}({input:?} COLLATE {name})"
                );
            }
        }
    }
    let default = CaseMapping::select(DEFAULT_COLLATION_OID as u32, "test").unwrap();
    let fast = CaseMapping::select(6411, "test").unwrap();
    for entry in fixture["strings"].as_array().unwrap() {
        let input = entry["input"].as_str().unwrap();
        for function in ["initcap", "lower", "upper", "casefold"] {
            assert_eq!(
                apply(default, function, input),
                apply(fast, function, input),
                "the database default is PG_UNICODE_FAST"
            );
        }
    }
}

#[test]
fn mappings_admit_their_entire_output_and_observe_both_cancellations() {
    for oid in [950, 811, 6411, 963] {
        let mapping = CaseMapping::select(oid, "test").unwrap();
        for function in ["lower", "upper", "initcap", "casefold"] {
            let memory = MemoryBudget::new(64 * 1024);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&memory, &original, &invoking);
            let input = "Stra\u{df}e \u{3a3}\u{3a3} o'neil \u{1c6}x".repeat(16);
            let result = match function {
                "lower" => mapping.lowercase(&input, &control),
                "upper" => mapping.uppercase(&input, &control),
                "initcap" => mapping.titlecase(&input, &control),
                _ => mapping.fold(&input, &control),
            }
            .unwrap();
            assert_eq!(memory.used(), result.capacity(), "{function} {oid}");
            drop(result);
            assert_eq!(memory.used(), 0);
            let limited = MemoryBudget::new(8);
            let control = ProductionControl::new(&limited, &original, &invoking);
            let error = mapping.lowercase(&input, &control).unwrap_err();
            assert_eq!(crate::SQLError::from(error).sqlstate(), Some("53200"));
            assert_eq!(limited.used(), 0);
            invoking.cancel();
            let control = ProductionControl::new(&memory, &original, &invoking);
            let error = mapping.titlecase(&input, &control).unwrap_err();
            assert_eq!(crate::SQLError::from(error).sqlstate(), Some("57014"));
            assert_eq!(memory.used(), 0);
        }
    }
}

/// ASCII runs, whole non-ASCII runs and bounded chunks map exactly as each character does on its own, and a capital sigma still reads its context across a chunk boundary.
#[test]
fn runs_and_chunks_map_like_single_characters() {
    let mixed = "Stra\u{df}e \u{c9}cole i\u{307}x \u{1c6}A \u{1e9e}z \u{130}K\u{212a} 12-ab_CD \u{fb01}\u{434}\u{416} "
        .repeat(200);
    for oid in [950, 811, 6411, 963] {
        let mapping = CaseMapping::select(oid, "test").unwrap();
        for function in ["lower", "upper", "casefold"] {
            let expected = mixed
                .chars()
                .map(|character| apply(mapping, function, &character.to_string()))
                .collect::<String>();
            assert_eq!(
                apply(mapping, function, &mixed),
                expected,
                "{function} {oid}"
            );
        }
    }
    let ascii = "A".repeat(4095);
    for (oid, text, lowered) in [
        (
            6411,
            format!("{ascii}\u{3a3} "),
            format!("{}\u{3c2} ", "a".repeat(4095)),
        ),
        (
            6411,
            format!("{ascii}\u{3a3}a"),
            format!("{}\u{3c3}a", "a".repeat(4095)),
        ),
        (
            963,
            format!("{ascii}\u{3a3} "),
            format!("{}\u{3c2} ", "a".repeat(4095)),
        ),
        (963, format!("'\u{3a3}"), "'\u{3c3}".to_owned()),
        (6411, format!("'\u{3a3}"), "'\u{3c2}".to_owned()),
    ] {
        let mapping = CaseMapping::select(oid, "test").unwrap();
        assert_eq!(apply(mapping, "lower", &text), lowered, "{oid}");
    }
}
