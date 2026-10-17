//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::PG18_BUILTIN_ROUTINE_GROUPS;

#[test]
fn range_subtype_distance_metadata_matches_postgresql() {
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("range_subtype_distance.json")).unwrap();
    let expected = expected["routines"].as_array().unwrap();
    assert_eq!(expected.len(), 6);
    for row in expected {
        let oid = row["oid"].as_str().unwrap().parse::<i64>().unwrap();
        let routine = PG18_BUILTIN_ROUTINE_GROUPS
            .iter()
            .flat_map(|group| group.iter())
            .find(|routine| routine.oid == oid)
            .unwrap();
        assert_eq!(
            serde_json::json!({
                "oid": routine.oid.to_string(),
                "proname": routine.name,
                "prosrc": routine.source,
                "prorettype": routine.return_type.to_string(),
                "proargtypes": routine.argument_types.iter().map(i64::to_string).collect::<Vec<_>>().join(" "),
                "proisstrict": routine.strict,
                "provolatile": routine.volatility,
                "proparallel": routine.parallel,
                "proleakproof": routine.leakproof,
                "prokind": routine.kind,
                "proretset": routine.returns_set(),
                "prolang": routine.language().to_string(),
            }),
            *row
        );
    }
}
