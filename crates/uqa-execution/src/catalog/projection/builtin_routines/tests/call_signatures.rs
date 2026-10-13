//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The declared polymorphic and wildcard signatures used by retained call nodes.

use super::{boolean_text, PG18_BUILTIN_ROUTINE_GROUPS};

#[test]
fn catalog_call_signatures_match_postgresql() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("call_signatures.json")).unwrap();
    let array = |items: Option<Vec<String>>| items.map(|items| format!("{{{}}}", items.join(",")));
    for row in fixture["rows"].as_array().unwrap() {
        let oid = row[0].as_str().unwrap().parse::<i64>().unwrap();
        let routine = PG18_BUILTIN_ROUTINE_GROUPS
            .iter()
            .flat_map(|group| group.iter())
            .find(|routine| routine.oid == oid)
            .unwrap();
        let actual = vec![
            Some(routine.oid.to_string()),
            Some(routine.name.into()),
            Some(routine.return_type.to_string()),
            Some(
                routine
                    .argument_types
                    .iter()
                    .map(i64::to_string)
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            array(
                routine
                    .all_argument_types()
                    .map(|types| types.iter().map(i64::to_string).collect()),
            ),
            array(
                routine
                    .argument_modes()
                    .map(|modes| modes.iter().map(|mode| (*mode).to_owned()).collect()),
            ),
            Some(routine.variadic_type().to_string()),
            Some(boolean_text(routine.strict).into()),
            Some(routine.volatility.into()),
            Some(routine.parallel.into()),
            Some(boolean_text(routine.leakproof).into()),
            Some(routine.source.into()),
        ];
        assert_eq!(serde_json::to_value(actual).unwrap(), *row, "OID {oid}");
    }
}
