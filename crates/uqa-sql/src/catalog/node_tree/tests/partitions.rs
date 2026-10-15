//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{deparse, parse, Routines, StringSettings};

#[test]
fn partition_nodes_render_without_relation_lookup_like_postgresql() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("partitions.json")).unwrap();
    for case in reference["cases"].as_array().unwrap() {
        let node = parse(case["node"].as_str().unwrap()).unwrap();
        deparse::validate_relation(&node, false).unwrap();
        for standard_strings in [true, false] {
            let names = StringSettings(&Routines, standard_strings);
            for pretty in [false, true] {
                assert_eq!(
                    deparse::expression(&node, &names, pretty).unwrap(),
                    case[if standard_strings { "sql" } else { "escaped" }]
                        .as_str()
                        .unwrap(),
                    "{}: standard_conforming_strings={standard_strings}, pretty={pretty}",
                    case["name"]
                );
            }
        }
    }
}
