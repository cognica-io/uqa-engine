//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn stored_query_input_preserves_flattened_body_order_and_assignment_alternatives() {
    let mut expression = query(QUERIES[9]);
    let mut target = expression["ScalarSubquery"]["with"][0]["update"]["assignments"][0][0].clone();
    target["column"] = json!("first_target");
    for indirection in [json!(false), json!([{"Index":{"Unknown":null}}]), json!([])] {
        target["indirection"] = indirection;
        expression["ScalarSubquery"]["with"][0]["update"]["assignments"][0][0] = target.clone();
        assert!(compare(&expression.to_string()));
    }
    target["targets"] = json!(false);
    expression["ScalarSubquery"]["with"][0]["update"]["assignments"][0][0] = target;
    assert!(compare(&expression.to_string()));

    let minimal = query("SELECT 1")["ScalarSubquery"].to_string();
    let outer = query("WITH q AS (SELECT 1) SELECT 2");
    let needle = outer["ScalarSubquery"]["with"][0].to_string();
    for (body, accepted) in [
        (format!("\"query\":{minimal},\"insert\":false"), true),
        (format!("\"query\":{minimal},\"query\":false"), true),
        (format!("\"insert\":false,\"query\":{minimal}"), false),
        (format!("\"query\":false,\"query\":{minimal}"), false),
        (format!("\"unknown\":0,\"query\":{minimal}"), true),
        (format!("\"query\":{minimal},\"name\":\"duplicate\""), false),
    ] {
        let cte = format!("{{\"name\":\"q\",\"columns\":[],\"recursive\":false,{body}}}");
        assert_eq!(compare(&outer.to_string().replace(&needle, &cte)), accepted);
    }
}

#[test]
fn stored_query_input_preserves_optional_mutations_and_generic_numeric_content() {
    let mut expression = query(QUERIES[11]);
    for tag in [
        "DeleteNotMatchedBySource",
        "NothingMatched",
        "NothingNotMatched",
        "NothingNotMatchedBySource",
    ] {
        for body in [
            json!({}),
            json!({"condition":null}),
            json!({"condition":{"Literal":true}}),
            json!([]),
            json!(null),
            json!(-0.0),
        ] {
            expression["ScalarSubquery"]["with"][0]["merge"]["when_clauses"] = json!([{tag:body}]);
            compare(&expression.to_string());
        }
    }
    expression["ScalarSubquery"]["with"][0]["merge"]["when_clauses"] =
        json!([{"NothingMatched":null}]);
    for number in [
        "0",
        "-0",
        "0.0",
        "-0.0",
        "1e0",
        "1e99999",
        "18446744073709551615",
        "18446744073709551616",
        "-9223372036854775809",
    ] {
        compare(&expression.to_string().replace(
            "\"NothingMatched\":null",
            &format!("\"NothingMatched\":{number}"),
        ));
        compare(&format!("{{\"Param\":{number}}}"));
        compare(&format!(
            "{{\"WindowCall\":{{\"name\":\"x\",\"args\":[],\"spec\":{{\"partition_by\":[],\"order_by\":[],\"definition\":{number}}}}}}}"
        ));
    }
}

#[test]
fn stored_query_input_preserves_depth_unknown_fields_and_invalid_unicode() {
    let base = query("SELECT 1")["ScalarSubquery"].to_string();
    assert!(base.contains(r#""expr":{"Literal":1}"#));
    assert!(base.contains(r#""where":null"#));
    for depth in [0, 1, 16, 32, 40, 42, 64] {
        let mut text = base.clone();
        for _ in 0..depth {
            let from = format!(
                "\"from\":{{\"Subquery\":{{\"body\":{text},\"alias\":null,\"column_aliases\":[]}}}}"
            );
            text = base.replace("\"from\":null", &from);
        }
        compare(&format!("{{\"ScalarSubquery\":{text}}}"));
    }
    for depth in [1, 16, 24, 32, 40, 64] {
        for kind in ["ScalarSubquery", "Exists", "WindowCall"] {
            let mut text = r#"{"Literal":1}"#.to_owned();
            for _ in 0..depth {
                text = match kind {
                    "ScalarSubquery" => format!(
                        "{{\"ScalarSubquery\":{}}}",
                        base.replace(r#""expr":{"Literal":1}"#, &format!("\"expr\":{text}"))
                    ),
                    "Exists" => format!(
                        "{{\"Exists\":{{\"body\":{},\"negated\":false}}}}",
                        base.replace(r#""where":null"#, &format!("\"where\":{text}"))
                    ),
                    _ => format!(
                        "{{\"WindowCall\":{{\"name\":\"sum\",\"args\":[{text}],\"spec\":{{\"partition_by\":[],\"order_by\":[]}}}}}}"
                    ),
                };
            }
            compare(&text);
        }
    }
    for text in [
        r#"{"WindowCall":{"name":"x","args":[],"spec":{"partition_by":[],"order_by":[]},"unused":"\uD800"}}"#,
        r#"{"WindowCall":{"name":"x","args":[],"args":[],"spec":{"partition_by":[],"order_by":[]}}}"#,
        r#"{"WindowCall":{"name":"한글\uD834\uDD1E","args":[],"spec":{"partition_by":[],"order_by":[]},"unused":1,"unused":2}}"#,
        r#"{"WindowCall":{"name":"x","args":[],"spec":{"partition_by":[],"order_by":[],"raw_definition":false}}}"#,
    ] {
        compare(text);
    }
}
