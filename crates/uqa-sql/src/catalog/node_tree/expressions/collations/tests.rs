//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::node_tree::parse;
use std::collections::BTreeMap;

fn reset_result_defaults(field: &mut Field, types: &BTreeMap<String, i64>) {
    match field {
        Field::Node(node) => {
            let result = match node.kind.as_str() {
                "OPEXPR" | "NULLIFEXPR" | "DISTINCTEXPR" => Some(("opresulttype", "opcollid")),
                "FUNCEXPR" => Some(("funcresulttype", "funccollid")),
                "RELABELTYPE" | "COERCEVIAIO" | "COERCETODOMAIN" | "ARRAYCOERCEEXPR" => {
                    Some(("resulttype", "resultcollid"))
                }
                "ARRAYEXPR" => Some(("array_typeid", "array_collid")),
                "COALESCEEXPR" => Some(("coalescetype", "coalescecollid")),
                "MINMAXEXPR" => Some(("minmaxtype", "minmaxcollid")),
                "CASEEXPR" => Some(("casetype", "casecollid")),
                _ => None,
            };
            if let Some((ty, field)) = result {
                let declared = types[&node.integer(ty).unwrap().to_string()];
                *field_mut(node, field).unwrap() = declared.into();
            }
            for (name, field) in &mut node.fields {
                if name == "inputcollid" {
                    *field = 0.into();
                } else if name == "inputcollids" {
                    let Field::List(values) = field else {
                        panic!("collation list")
                    };
                    for value in &mut values[1..] {
                        *value = 0.into();
                    }
                } else {
                    reset_result_defaults(field, types);
                }
            }
        }
        Field::List(items) => {
            for item in items {
                reset_result_defaults(item, types);
            }
        }
        _ => {}
    }
}

#[test]
fn selected_collations_match_independent_postgresql_nodes() {
    let types = serde_json::from_str(include_str!("../../tests/collation_types.json")).unwrap();
    let mut observed = 0;
    for source in [
        include_str!("../../tests/implicit_collations.json"),
        include_str!("../../tests/explicit_collations.json"),
    ] {
        let cases: Vec<serde_json::Value> = serde_json::from_str(source).unwrap();
        for case in cases {
            let expected = parse(case["conbin"].as_str().unwrap()).unwrap();
            let mut actual = expected.clone();
            reset_result_defaults(&mut actual, &types);
            let Field::Node(node) = &mut actual else {
                panic!("expression node")
            };
            assign(node).unwrap();
            assert_eq!(actual, expected, "{}", case["check"]);
            observed += 1;
        }
    }
    assert_eq!(observed, 37);
}

#[test]
fn derivation_precedence_and_conflicts_match_postgresql() {
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("../../tests/collation_derivation.json")).unwrap();
    assert_eq!(cases.len(), 43);
    for case in cases {
        let selected =
            case["inputs"]
                .as_array()
                .unwrap()
                .iter()
                .try_fold(Derivation::None, |state, input| {
                    let oid = input["oid"].as_i64().unwrap();
                    let next = match input["derivation"].as_str().unwrap() {
                        "implicit" => Derivation::implicit(oid),
                        "explicit" => Derivation::Explicit(oid),
                        _ => panic!("unknown reference derivation"),
                    };
                    state.merge(next)
                });
        if case.get("error").is_some() {
            let error = selected.unwrap_err();
            assert_eq!(error.sqlstate(), case["error"]["sqlstate"].as_str());
            assert_eq!(
                error.to_string(),
                case["error"]["message"].as_str().unwrap()
            );
            assert_eq!(error.detail(), case["error"]["detail"].as_str());
            assert_eq!(error.hint(), case["error"]["hint"].as_str());
        } else {
            assert_eq!(
                selected.unwrap().oid(),
                case["oid"].as_i64().unwrap(),
                "{}",
                case["sql"]
            );
        }
    }
}

fn replace_collations(field: &mut Field, index: &mut usize, replacements: &BTreeMap<usize, i64>) {
    match field {
        Field::Node(node) => {
            if node.kind == "COLLATEEXPR" {
                if let Some(oid) = replacements.get(index) {
                    *field_mut(node, "collOid").unwrap() = (*oid).into();
                }
                *index += 1;
            }
            for (_, field) in &mut node.fields {
                replace_collations(field, index, replacements);
            }
        }
        Field::List(items) => {
            for item in items {
                replace_collations(item, index, replacements);
            }
        }
        _ => {}
    }
}

#[test]
fn row_comparison_visits_pairs_in_postgresql_diagnostic_order() {
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("../../tests/collation_errors.json")).unwrap();
    assert_eq!(cases.len(), 2);
    for case in cases {
        let mut input = parse(case["conbin"].as_str().unwrap()).unwrap();
        let replacements = case["collation_replacements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pair| {
                (
                    usize::try_from(pair[0].as_u64().unwrap()).unwrap(),
                    pair[1].as_i64().unwrap(),
                )
            })
            .collect();
        replace_collations(&mut input, &mut 0, &replacements);
        let Field::Node(node) = &mut input else {
            panic!("ROW comparison")
        };
        let error = assign(node).unwrap_err();
        assert_eq!(error.sqlstate(), case["error"]["sqlstate"].as_str());
        assert_eq!(
            error.to_string(),
            case["error"]["message"].as_str().unwrap(),
            "{}",
            case["check"]
        );
        assert_eq!(error.detail(), case["error"]["detail"].as_str());
        assert_eq!(error.hint(), case["error"]["hint"].as_str());
    }
}
