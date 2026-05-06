//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Hierarchical-path property tests (Master Plan Section 2.3,
//! Definitions 5.3.1-5.3.5 from Paper 1).
//!
//! Pins:
//! - empty path returns `None` (degenerate),
//! - leading `Index` segment returns `None` (a Document is keyed,
//!   not indexed, at the top),
//! - single-key path equals direct `Document::get`,
//! - `parse_path` is a homomorphism: every `.`-delimited segment in
//!   the input string becomes one segment in the output,
//! - **composition law**: walking `p1 ++ p2` against a Document
//!   equals walking `p1` to a subtree, wrapping that subtree under a
//!   synthetic key, and walking `[Key(synthetic), ...p2]` against
//!   the wrapper. This is the hierarchical analogue of
//!   `eval(h, p1 ++ p2) == eval(eval(h, p1), p2)`.

use std::collections::BTreeMap;

use proptest::prelude::*;
use uqa_core::{PathExpr, PathSegment, Value};
use uqa_operators::hierarchical::{eval_path, parse_path};
use uqa_storage::document_store::Document;

/// Build a small two-level nested Document, returning the doc plus
/// the keys we put at each level.
fn nested_doc(outer_key: &str, inner_key: &str, leaf: Value) -> Document {
    let mut inner = BTreeMap::new();
    inner.insert(inner_key.to_string(), leaf);
    let mut doc = Document::new();
    doc.insert(outer_key.into(), Value::Map(inner));
    doc
}

/// Wrap an arbitrary Value under a single key so it can be walked
/// from a Document root.
fn wrap_under(key: &str, value: Value) -> Document {
    let mut d = Document::new();
    d.insert(key.into(), value);
    d
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 96,
        ..ProptestConfig::default()
    })]

    /// Single-key path equals direct lookup.
    #[test]
    fn single_key_equals_direct_lookup(
        key in "[a-z]{1,4}",
        value in -1000i64..=1000,
    ) {
        let mut doc = Document::new();
        doc.insert(key.clone(), Value::Int(value));
        let path = vec![PathSegment::Key(key.clone())];
        let observed = eval_path(&doc, &path);
        prop_assert_eq!(observed, Some(Value::Int(value)));
    }

    /// `parse_path` produces one segment per `.`-delimited piece.
    #[test]
    fn parse_path_segment_count(s in "[a-z]{1,4}(\\.[a-z]{1,4}){0,4}") {
        let parsed = parse_path(&s);
        let expected = s.split('.').count();
        prop_assert_eq!(parsed.len(), expected);
    }

    /// Numeric segments parse to `Index`, alphabetic to `Key`.
    #[test]
    fn parse_path_classifies_segments(
        a in "[a-z]{1,4}",
        n in 0usize..=20,
    ) {
        let s = format!("{a}.{n}");
        let parsed = parse_path(&s);
        prop_assert_eq!(parsed.len(), 2);
        prop_assert!(matches!(parsed[0], PathSegment::Key(ref k) if k == &a));
        prop_assert!(matches!(parsed[1], PathSegment::Index(i) if i == n));
    }

    /// Composition law: walking `p1 ++ p2` from the Document root
    /// equals walking `p1` to get a subtree, wrapping it under a
    /// fresh key, and walking `[Key(fresh), ...p2]` from the wrapper.
    #[test]
    fn composition_law_two_level(
        outer in "[a-z]{1,4}",
        inner in "[a-z]{1,4}",
        leaf in -1000i64..=1000,
    ) {
        prop_assume!(outer != inner);
        let doc = nested_doc(&outer, &inner, Value::Int(leaf));
        let p1: PathExpr = vec![PathSegment::Key(outer.clone())];
        let p2: PathExpr = vec![PathSegment::Key(inner.clone())];

        let mut concat = p1.clone();
        concat.extend(p2.clone());
        let direct = eval_path(&doc, &concat);

        // Inner walk: eval p1 to get the subtree, wrap under "x",
        // then walk [Key("x"), ...p2].
        let subtree = eval_path(&doc, &p1).expect("p1 resolves");
        let wrapper = wrap_under("x", subtree);
        let mut stepwise_path = vec![PathSegment::Key("x".into())];
        stepwise_path.extend(p2);
        let stepwise = eval_path(&wrapper, &stepwise_path);

        prop_assert_eq!(direct, stepwise);
    }
}

/// Empty path is undefined: `path.first()?` returns None on the
/// empty slice.
#[test]
fn empty_path_returns_none() {
    let mut doc = Document::new();
    doc.insert("k".into(), Value::Int(1));
    assert_eq!(eval_path(&doc, &[]), None);
}

/// Leading `Index` segment against a Document returns None — a
/// Document is keyed, not indexed.
#[test]
fn leading_index_returns_none() {
    let mut doc = Document::new();
    doc.insert("k".into(), Value::Int(1));
    let path = vec![PathSegment::Index(0)];
    assert_eq!(eval_path(&doc, &path), None);
}

/// Missing key returns None.
#[test]
fn missing_key_returns_none() {
    let mut doc = Document::new();
    doc.insert("a".into(), Value::Int(1));
    let path = vec![PathSegment::Key("b".into())];
    assert_eq!(eval_path(&doc, &path), None);
}

/// Walking through a List with an Index segment picks that element.
#[test]
fn list_index_descent() {
    let mut doc = Document::new();
    doc.insert(
        "xs".into(),
        Value::List(vec![Value::Int(10), Value::Int(20), Value::Int(30)]),
    );
    let path = vec![PathSegment::Key("xs".into()), PathSegment::Index(1)];
    assert_eq!(eval_path(&doc, &path), Some(Value::Int(20)));
}
