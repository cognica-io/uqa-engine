//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeSet;

use super::PG18_BUILTIN_ROUTINE_GROUPS;

#[test]
fn clock_and_case_metadata_matches_postgresql() {
    let reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/builtin_routine_identities_oracle.expected.json"
    )))
    .unwrap();
    let catalog = reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == "catalog")
        .unwrap();
    let expected: Vec<Vec<String>> =
        serde_json::from_value(catalog["results"][0]["rows"].clone()).unwrap();
    assert_eq!(expected.len(), 7);
    let oids: BTreeSet<i64> = expected.iter().map(|row| row[0].parse().unwrap()).collect();
    let mut routines: Vec<_> = PG18_BUILTIN_ROUTINE_GROUPS
        .iter()
        .flat_map(|group| group.iter())
        .filter(|routine| oids.contains(&routine.oid))
        .collect();
    routines.sort_by_key(|routine| routine.oid);
    let actual: Vec<Vec<String>> = routines
        .into_iter()
        .map(|routine| {
            vec![
                routine.oid.to_string(),
                routine.name.into(),
                routine.kind.into(),
                boolean_text(routine.strict).into(),
                routine.volatility.into(),
                routine.parallel.into(),
                boolean_text(routine.leakproof).into(),
                routine.return_type.to_string(),
                routine
                    .argument_types
                    .iter()
                    .map(i64::to_string)
                    .collect::<Vec<_>>()
                    .join(" "),
                routine.source.into(),
            ]
        })
        .collect();
    assert_eq!(actual, expected);
}

fn boolean_text(value: bool) -> &'static str {
    if value {
        "t"
    } else {
        "f"
    }
}

#[test]
fn builtin_routine_oids_and_signatures_are_unique() {
    let mut oids = BTreeSet::new();
    let mut signatures = BTreeSet::new();
    for routine in PG18_BUILTIN_ROUTINE_GROUPS
        .iter()
        .flat_map(|group| group.iter())
    {
        assert!(
            oids.insert(routine.oid),
            "duplicate OID {} for {}",
            routine.oid,
            routine.name
        );
        assert!(
            signatures.insert((routine.name, routine.argument_types)),
            "duplicate signature {}({:?})",
            routine.name,
            routine.argument_types
        );
    }
}
