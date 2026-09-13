//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::HashSet;

use uqa_analysis::{AnalysisError, TokenTerm};

#[test]
fn every_utf16_unit_round_trips_without_replacement_or_identity_collisions() {
    let mut distinct = HashSet::new();
    for unit in 0..=u16::MAX {
        let term = TokenTerm::from_utf16(vec![unit]);
        assert_eq!(term.utf16().as_ref(), [unit]);
        assert_eq!(term.character_count(), 1);
        assert_eq!(term.utf16_len(), 1);
        let json = serde_json::to_string(&term).unwrap();
        let restored: TokenTerm = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, term);
        if (0xd800..=0xdfff).contains(&unit) {
            assert!(term.as_str().is_none());
            assert!(
                matches!(term.clone().into_string(), Err(AnalysisError::UnpairedTokenSurrogate { unit: actual }) if actual == unit)
            );
        } else {
            assert_eq!(term.as_str().unwrap(), term.clone().into_string().unwrap());
        }
        assert!(distinct.insert(term));
    }
    assert_eq!(distinct.len(), 65536);
}

#[test]
fn paired_units_share_string_identity_and_raw_diagnostics_are_strict() {
    for high in 0xd800..=0xdbff {
        for low in [0xdc00, 0xdc01, 0xdfff] {
            let units = vec![high, low];
            let term = TokenTerm::from_utf16(units.clone());
            let text = String::from_utf16(&units).unwrap();
            assert_eq!(term, TokenTerm::from(text.clone()));
            assert_eq!(term.as_str(), Some(text.as_str()));
            assert_eq!(term.character_count(), 1);
            assert_eq!(term.utf16_len(), 2);
            assert_eq!(term.into_utf16(), units);
        }
    }
    let raw = TokenTerm::from_utf16(vec![0xdc00, 0xd800]);
    assert_eq!(
        serde_json::to_value(&raw).unwrap(),
        serde_json::json!({"utf16": [56320, 55296]})
    );
    assert_ne!(raw, TokenTerm::from("��"));
    assert_eq!(
        serde_json::from_str::<TokenTerm>(r#"{"utf16":[65]}"#).unwrap(),
        "A"
    );
    for invalid in [
        r#"{"utf16":[65536]}"#,
        r#"{"utf16":[-1]}"#,
        r#"{"utf16":[1],"extra":2}"#,
        "[]",
        "null",
    ] {
        assert!(serde_json::from_str::<TokenTerm>(invalid).is_err());
    }
}
