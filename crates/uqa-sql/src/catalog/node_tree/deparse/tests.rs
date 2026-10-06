//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::node_tree::parse;

struct Names {
    qualified: bool,
}

impl ExpressionNames for Names {
    fn column(&self, _: i64) -> Result<String, SQLError> {
        Err(invalid("unexpected column in built-in RETURN body"))
    }

    fn routine(&self, oid: i64) -> Result<Vec<String>, SQLError> {
        let name = match oid {
            720 | 1374 => "octet_length",
            2021 => "date_part",
            2626 => "pg_sleep",
            2649 => "clock_timestamp",
            _ => return Err(invalid(format!("unexpected routine {oid}"))),
        };
        Ok(if self.qualified {
            vec!["pg_catalog".into(), name.into()]
        } else {
            vec![name.into()]
        })
    }

    fn type_name(&self, oid: i64, modifier: i64) -> Result<String, SQLError> {
        assert_eq!(modifier, -1);
        match oid {
            701 => Ok("double precision".into()),
            1114 => Ok("timestamp without time zone".into()),
            _ => Err(invalid(format!("unexpected type {oid}"))),
        }
    }
}

fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/builtin_sqlbody_oracle.expected.json"
    )))
    .unwrap()
}

fn rows<'a>(fixture: &'a serde_json::Value, id: &str) -> &'a [serde_json::Value] {
    fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == id)
        .unwrap()["results"][0]["rows"]
        .as_array()
        .unwrap()
}

#[test]
fn builtin_return_bodies_match_postgresql_with_catalog_selected_names() {
    let fixture = fixture();
    let bodies = rows(&fixture, "raw_bodies");
    assert_eq!(bodies.len(), 5);
    for (case, qualified) in [
        ("builtin_bodies", false),
        ("shadowed_bodies", true),
        ("reopen_bodies", false),
    ] {
        let expected = rows(&fixture, case);
        assert_eq!(expected.len(), 5);
        for body in bodies {
            let node = parse(body[1].as_str().unwrap()).unwrap();
            assert_eq!(parse(&node.to_string()).unwrap(), node);
            let expected = expected.iter().find(|row| row[0] == body[0]).unwrap();
            assert_eq!(
                return_body(&node, &Names { qualified }).unwrap(),
                expected[2].as_str().unwrap(),
                "{case}: {}",
                body[0],
            );
        }
    }
}

#[test]
fn external_parameters_keep_positional_spelling_and_cast_prettiness() {
    let names = Names { qualified: false };
    let parameter: Field =
        Node::new("PARAM", [("paramkind", 0.into()), ("paramid", 2.into())]).into();
    for pretty in [false, true] {
        assert_eq!(expression(&parameter, &names, pretty).unwrap(), "$2");
    }
    let cast = Node::new(
        "RELABELTYPE",
        [
            ("arg", parameter),
            ("resulttype", 1114.into()),
            ("resulttypmod", (-1).into()),
        ],
    )
    .into();
    assert_eq!(
        expression(&cast, &names, false).unwrap(),
        "($2)::timestamp without time zone"
    );
    assert_eq!(
        expression(&cast, &names, true).unwrap(),
        "$2::timestamp without time zone"
    );
    for (kind, number) in [(1, 1), (2, 1), (3, 1), (0, 0), (0, -1)] {
        let parameter = Node::new(
            "PARAM",
            [("paramkind", kind.into()), ("paramid", number.into())],
        )
        .into();
        assert_eq!(
            expression(&parameter, &names, false)
                .unwrap_err()
                .sqlstate(),
            Some("XX000")
        );
    }
}

fn replace(node: &mut Node, name: &str, value: Field) {
    node.fields
        .iter_mut()
        .find(|(field, _)| field == name)
        .unwrap()
        .1 = value;
}

#[test]
fn scalar_return_queries_reject_other_commands_targets_and_clauses() {
    let fixture = fixture();
    let value = parse(rows(&fixture, "raw_bodies")[0][1].as_str().unwrap()).unwrap();
    let Field::Node(query) = value else {
        panic!("QUERY");
    };
    let names = Names { qualified: false };
    for (field, value) in [
        ("commandType", 2.into()),
        ("isReturn", false.into()),
        ("resultRelation", 1.into()),
        ("cteList", Field::List(vec![Field::Null])),
        ("rtable", Field::List(vec![Field::Null])),
        ("limitCount", 1.into()),
        ("targetList", Field::Null),
    ] {
        let mut invalid_query = query.clone();
        replace(&mut invalid_query, field, value);
        assert_eq!(
            return_body(&invalid_query.into(), &names)
                .unwrap_err()
                .sqlstate(),
            Some("XX000"),
            "{field}"
        );
    }
    let mut filtered = query.clone();
    let Field::Node(mut from) = query.field("jointree").unwrap().clone() else {
        panic!("FROMEXPR");
    };
    replace(&mut from, "quals", 1.into());
    replace(&mut filtered, "jointree", from.into());
    assert!(return_body(&filtered.into(), &names).is_err());
    let [Field::Node(target)] = list(&query, "targetList").unwrap() else {
        panic!("TARGETENTRY");
    };
    let mut junk = target.clone();
    replace(&mut junk, "resjunk", true.into());
    let mut invalid_query = query.clone();
    replace(
        &mut invalid_query,
        "targetList",
        Field::List(vec![junk.into()]),
    );
    assert!(return_body(&invalid_query.into(), &names).is_err());
    let mut invalid_query = query.clone();
    replace(
        &mut invalid_query,
        "targetList",
        Field::List(vec![target.clone().into(), target.clone().into()]),
    );
    assert!(return_body(&invalid_query.into(), &names).is_err());
}
