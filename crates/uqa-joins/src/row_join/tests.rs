//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::Value;

fn row<const N: usize>(pairs: [(&str, Value); N]) -> ResultRow {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

#[test]
fn hash_inner_basic() {
    let l = vec![
        row([("id", Value::Int(1)), ("a", Value::Str("a1".into()))]),
        row([("id", Value::Int(2)), ("a", Value::Str("a2".into()))]),
    ];
    let r = vec![
        row([("uid", Value::Int(2)), ("b", Value::Str("b2".into()))]),
        row([("uid", Value::Int(3)), ("b", Value::Str("b3".into()))]),
    ];
    let out = hash_inner_join(
        &l,
        &r,
        |row| row.get("id").map(JoinKey::new),
        |row| row.get("uid").map(JoinKey::new),
    );
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["id"], Value::Int(2));
    assert_eq!(out[0]["b"], Value::Str("b2".into()));
}

#[test]
fn enum_keys_match_by_type_and_label_key() {
    let label = |type_oid, key: &[u8]| {
        Value::Enum(uqa_core::EnumValue::new(
            type_oid,
            uqa_core::EnumLabelKey::from_bytes(key.to_vec()).unwrap(),
        ))
    };
    let l = vec![
        row([
            ("m", label(7, &[64])),
            ("side", Value::Str("left-a".into())),
        ]),
        row([
            ("m", label(7, &[128])),
            ("side", Value::Str("left-b".into())),
        ]),
    ];
    let r = vec![
        row([("n", label(7, &[64])), ("other", Value::Int(1))]),
        row([("n", label(8, &[64])), ("other", Value::Int(2))]),
        row([("n", Value::Bytes(vec![64])), ("other", Value::Int(3))]),
    ];
    let out = hash_inner_join(
        &l,
        &r,
        |row| row.get("m").map(JoinKey::new),
        |row| row.get("n").map(JoinKey::new),
    );
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["side"], Value::Str("left-a".into()));
    assert_eq!(out[0]["other"], Value::Int(1));
}

#[test]
fn left_outer_pads_unmatched() {
    let l = vec![
        row([("id", Value::Int(1)), ("name", Value::Str("a".into()))]),
        row([("id", Value::Int(2)), ("name", Value::Str("b".into()))]),
    ];
    let r = vec![row([("uid", Value::Int(2)), ("v", Value::Int(99))])];
    let out = left_outer_join(
        &l,
        &r,
        |row| row.get("id").map(JoinKey::new),
        |row| row.get("uid").map(JoinKey::new),
    );
    assert_eq!(out.len(), 2);
    let unmatched = out.iter().find(|r| r["id"] == Value::Int(1)).unwrap();
    assert_eq!(unmatched["v"], Value::Null);
}

#[test]
fn full_outer_matches_unmatched_on_both_sides() {
    let l = vec![row([("id", Value::Int(1))])];
    let r = vec![row([("uid", Value::Int(2))])];
    let out = full_outer_join(
        &l,
        &r,
        |row| row.get("id").map(JoinKey::new),
        |row| row.get("uid").map(JoinKey::new),
    );
    assert_eq!(out.len(), 2);
}

#[test]
fn semi_anti_partition_input() {
    let l = vec![
        row([("id", Value::Int(1))]),
        row([("id", Value::Int(2))]),
        row([("id", Value::Int(3))]),
    ];
    let r = vec![row([("uid", Value::Int(2))]), row([("uid", Value::Int(3))])];
    let semi = semi_join(
        &l,
        &r,
        |row| row.get("id").map(JoinKey::new),
        |row| row.get("uid").map(JoinKey::new),
    );
    let anti = anti_join(
        &l,
        &r,
        |row| row.get("id").map(JoinKey::new),
        |row| row.get("uid").map(JoinKey::new),
    );
    assert_eq!(semi.len(), 2);
    assert_eq!(anti.len(), 1);
    assert_eq!(anti[0]["id"], Value::Int(1));
}

#[test]
fn cross_size_is_product() {
    let l = vec![row([("id", Value::Int(1))]), row([("id", Value::Int(2))])];
    let r = vec![
        row([("uid", Value::Int(10))]),
        row([("uid", Value::Int(20))]),
        row([("uid", Value::Int(30))]),
    ];
    assert_eq!(cross_join(&l, &r).len(), 6);
}

#[test]
fn sort_merge_matches_hash_join() {
    let l = vec![
        row([("k", Value::Int(1)), ("a", Value::Int(10))]),
        row([("k", Value::Int(2)), ("a", Value::Int(20))]),
        row([("k", Value::Int(2)), ("a", Value::Int(21))]),
    ];
    let r = vec![
        row([("k", Value::Int(2)), ("b", Value::Int(200))]),
        row([("k", Value::Int(3)), ("b", Value::Int(300))]),
    ];
    let out = sort_merge_inner_join(&l, &r, "k", "k");
    assert_eq!(out.len(), 2);
}

#[test]
fn sort_merge_never_matches_null_or_missing_keys() {
    let left = vec![
        row([("k", Value::Null), ("left", Value::Str("null".into()))]),
        row([
            ("other", Value::Int(1)),
            ("left", Value::Str("missing".into())),
        ]),
        row([("k", Value::Int(1)), ("left", Value::Str("match".into()))]),
    ];
    let right = vec![
        row([("k", Value::Null), ("right", Value::Str("null".into()))]),
        row([
            ("other", Value::Int(1)),
            ("right", Value::Str("missing".into())),
        ]),
        row([
            ("k", Value::Float(1.0)),
            ("right", Value::Str("match".into())),
        ]),
    ];
    let out = sort_merge_inner_join(&left, &right, "k", "k");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["left"], Value::Str("match".into()));
    assert_eq!(out[0]["right"], Value::Str("match".into()));
}

#[test]
fn hash_join_key_matches_value_cross_numeric_equality() {
    let decimal_one = DecimalValue::parse("1.00").unwrap();
    let equivalent = [
        Value::Bool(true),
        Value::Int(1),
        Value::Float(1.0),
        Value::Decimal(decimal_one),
    ];
    let mut index = HashMap::new();
    index.insert(JoinKey::new(&equivalent[0]), "found");
    for value in &equivalent {
        assert_eq!(equivalent[0].cmp(value), std::cmp::Ordering::Equal);
        assert_eq!(
            JoinKey::new(&equivalent[0]).cmp(&JoinKey::new(value)),
            std::cmp::Ordering::Equal
        );
        assert_eq!(index.get(&JoinKey::new(value)), Some(&"found"));
    }

    let left = vec![row([("k", Value::Int(1))])];
    let right = vec![row([("k", Value::Float(1.0))])];
    assert_eq!(
        hash_inner_join(
            &left,
            &right,
            |row| row.get("k").map(JoinKey::new),
            |row| row.get("k").map(JoinKey::new),
        )
        .len(),
        1
    );
}

#[test]
fn hash_join_keys_preserve_exact_numeric_equivalence_in_nested_keys() {
    for value in [
        0.1,
        -0.1,
        9_223_372_036_854_774_784_i64 as f64,
        f64::from_bits(1),
        f64::MAX,
        -0.0,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
    ] {
        let float = Value::Float(value);
        let decimal = Value::Decimal(DecimalValue::from_f64_exact(value));
        let left = vec![row([("k", float.clone())])];
        let right = vec![row([("k", decimal.clone())])];
        let joined = hash_inner_join(
            &left,
            &right,
            |row| row.get("k").map(JoinKey::new),
            |row| row.get("k").map(JoinKey::new),
        );
        assert_eq!(joined, sort_merge_inner_join(&left, &right, "k", "k"));
        assert_eq!(joined.len(), 1);
        for (left, right) in [
            (JoinKey::new(&float), JoinKey::new(&decimal)),
            (
                JoinKey::composite(&[&float]),
                JoinKey::composite(&[&decimal]),
            ),
            (
                JoinKey::new(&Value::Array(
                    uqa_core::ArrayValue::try_new(vec![float.clone()]).unwrap(),
                )),
                JoinKey::new(&Value::Array(
                    uqa_core::ArrayValue::try_new(vec![decimal.clone()]).unwrap(),
                )),
            ),
        ] {
            assert_eq!(left, right);
            let mut index = HashMap::new();
            index.insert(left, true);
            assert_eq!(index.get(&right), Some(&true));
        }
    }
}

#[test]
fn index_inner_uses_prebuilt_hash() {
    let l = vec![row([("id", Value::Int(7))])];
    let mut idx: HashMap<JoinKey, Vec<ResultRow>> = HashMap::new();
    idx.insert(
        JoinKey::new(&Value::Int(7)),
        vec![row([
            ("uid", Value::Int(7)),
            ("name", Value::Str("x".into())),
        ])],
    );
    let out = index_inner_join(&l, &idx, |row| row.get("id").map(JoinKey::new));
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["name"], Value::Str("x".into()));
}

#[test]
fn jsonb_join_hashes_match_postgresql_semantic_equality() {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../uqa-core/src/types/tests/pg18_jsonb.json"
    )))
    .unwrap();
    let values: Vec<_> = oracle["values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|text| Value::JsonB(text.as_str().unwrap().into()))
        .collect();
    let mut expected = std::collections::BTreeSet::new();
    for pair in oracle["comparisons"].as_array().unwrap() {
        let left = pair[0].as_u64().unwrap() as usize;
        let right = pair[1].as_u64().unwrap() as usize;
        let equal = pair[2].as_bool().unwrap();
        let mut index = HashMap::new();
        index.insert(JoinKey::new(&values[left]), true);
        assert_eq!(
            index.contains_key(&JoinKey::new(&values[right])),
            equal,
            "{}, {}",
            oracle["values"][left],
            oracle["values"][right]
        );
        if equal {
            expected.insert((left as i64, right as i64));
        }
    }
    let rows = |id, key| {
        values
            .iter()
            .enumerate()
            .map(|(i, value)| row([(id, Value::Int(i as i64)), (key, value.clone())]))
            .collect::<Vec<_>>()
    };
    let left = rows("left_id", "left_key");
    let right = rows("right_id", "right_key");
    let joined = hash_inner_join(
        &left,
        &right,
        |row| row.get("left_key").map(JoinKey::new),
        |row| row.get("right_key").map(JoinKey::new),
    );
    let actual: std::collections::BTreeSet<_> = joined
        .iter()
        .map(|row| {
            let Value::Int(left) = row["left_id"] else {
                panic!("left identity");
            };
            let Value::Int(right) = row["right_id"] else {
                panic!("right identity");
            };
            (left, right)
        })
        .collect();
    assert_eq!(joined.len(), expected.len());
    assert_eq!(actual, expected);
}
