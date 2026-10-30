//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn expanded_rule_fields_keep_postgresql_event_collations() {
    let catalog = catalog();
    let mut cases: Vec<Reference> = serde_json::from_str(include_str!("expanded.json")).unwrap();
    assert_eq!(
        cases.iter().map(|case| case.variables.len()).sum::<usize>(),
        64
    );
    let shadowed: Vec<Reference> = serde_json::from_str(include_str!("shadowed.json")).unwrap();
    assert_eq!(
        shadowed
            .iter()
            .map(|case| case.variables.len())
            .sum::<usize>(),
        16
    );
    cases.extend(shadowed);
    for case in cases {
        for empty in [false, true] {
            for restore in [false, true] {
                check_schema(
                    &catalog,
                    &case,
                    native_query(&catalog, &case, empty, restore),
                );
            }
        }
    }
}
