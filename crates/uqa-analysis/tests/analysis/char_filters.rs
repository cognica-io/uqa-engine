//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

// =====================================================================
// Char Filters
// =====================================================================

#[test]
fn html_strip_strip_tags() {
    let f = CharFilter::HTMLStrip;
    let result = f.filter("<p>Hello <b>world</b></p>").unwrap();
    assert!(result.contains("Hello"));
    assert!(result.contains("world"));
    assert!(!result.contains('<'));
}

#[test]
fn html_strip_no_tags() {
    let f = CharFilter::HTMLStrip;
    assert_eq!(f.filter("plain text").unwrap(), "plain text");
}

#[test]
fn html_strip_entities() {
    let f = CharFilter::HTMLStrip;
    assert_eq!(f.filter("a &amp; b").unwrap(), "a & b");
}

#[test]
fn html_strip_roundtrip() {
    let f = CharFilter::HTMLStrip;
    let json = serde_json::to_string(&f).unwrap();
    let back: CharFilter = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, CharFilter::HTMLStrip));
}

#[test]
fn mapping_char_filter_mapping() {
    let mut m = BTreeMap::new();
    m.insert("&".to_string(), "and".to_string());
    m.insert("@".to_string(), "at".to_string());
    let f = CharFilter::Mapping { mapping: m };
    assert_eq!(f.filter("you & me @ home").unwrap(), "you and me at home");
}

#[test]
fn mapping_char_filter_roundtrip() {
    let mut m = BTreeMap::new();
    m.insert("x".into(), "y".into());
    let f = CharFilter::Mapping { mapping: m };
    let json = serde_json::to_string(&f).unwrap();
    let back: CharFilter = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, CharFilter::Mapping { .. }));
}

#[test]
fn pattern_replace_replace() {
    let f = CharFilter::PatternReplace {
        pattern: r"\d+".into(),
        replacement: "#".into(),
    };
    assert_eq!(f.filter("abc123def456").unwrap(), "abc#def#");
}

#[test]
fn pattern_replace_roundtrip() {
    let f = CharFilter::PatternReplace {
        pattern: r"\s+".into(),
        replacement: " ".into(),
    };
    let json = serde_json::to_string(&f).unwrap();
    let back: CharFilter = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, CharFilter::PatternReplace { .. }));
}
