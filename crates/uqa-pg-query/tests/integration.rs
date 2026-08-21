//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Regression tests for the pinned PostgreSQL 18 parser fixes.

#[test]
fn preserves_quoted_plpgsql_type_name_identifiers() {
    let result = pg_query::parse_plpgsql(
        r#"
        CREATE FUNCTION quoted_type_names() RETURNS void AS $$
        DECLARE
            column_value "app.dot"."typed.dot"."id.dot"%TYPE;
            row_value "app.dot"."typed.dot"%ROWTYPE;
        BEGIN
            RETURN;
        END;
        $$ LANGUAGE plpgsql;
        "#,
    )
    .unwrap();

    let datums = result[0]["PLpgSQL_function"]["datums"].as_array().unwrap();
    let type_metadata = |refname: &str| {
        &datums
            .iter()
            .find(|datum| datum["PLpgSQL_var"]["refname"] == refname)
            .unwrap()["PLpgSQL_var"]["datatype"]["PLpgSQL_type"]
    };

    assert_eq!(
        type_metadata("column_value")["typname_identifiers"],
        serde_json::json!(["app.dot", "typed.dot", "id.dot"])
    );
    assert_eq!(
        type_metadata("row_value")["typname_identifiers"],
        serde_json::json!(["app.dot", "typed.dot"])
    );
}
