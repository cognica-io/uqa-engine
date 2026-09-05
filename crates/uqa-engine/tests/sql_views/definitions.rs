//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` view-definition transcripts and executable reconstruction.

use super::*;

const FIXTURE: &str = include_str!("../../../../tests/parity/pg18/view_definitions_stateful.sql");
const EXPECTED: &str =
    include_str!("../../../../tests/parity/pg18/view_definitions_stateful.expected.json");

#[test]
fn view_definitions_match_postgresql_transcript_and_execute() {
    let engine = Engine::new();
    let expected: serde_json::Value = serde_json::from_str(EXPECTED).unwrap();
    let mut differences = Vec::new();
    for (case, expected) in FIXTURE
        .split("-- @case ")
        .skip(1)
        .zip(expected["cases"].as_array().unwrap())
    {
        let (header, body) = case.split_once('\n').unwrap();
        let name = header.split_whitespace().next().unwrap();
        assert_eq!(name, expected["name"].as_str().unwrap());
        let sql = body
            .split("-- @end")
            .next()
            .unwrap()
            .replace("__UQA_STATEFUL_SCHEMA__", "view_definitions")
            .replace("__UQA_SCHEMA_PROBE__", "view_definitions_schema_probe");
        if name != "create_schema" {
            exec(&engine, "SET search_path = view_definitions, pg_catalog");
        }
        let result = engine.sql(&sql, &[]);
        if expected["kind"] == "error" {
            let state = result.as_ref().err().and_then(SQLError::sqlstate);
            if state != expected["sqlstate"].as_str() {
                differences.push(format!(
                    "{name}: expected {}, got {result:?}",
                    expected["sqlstate"]
                ));
            }
            continue;
        }
        let result = result.unwrap_or_else(|error| panic!("{name}: {sql}: {error}"));
        if expected["kind"] != "rows" {
            continue;
        }
        let rows = result
            .rows
            .iter()
            .map(|row| {
                result
                    .columns
                    .iter()
                    .zip(&result.column_types)
                    .map(|(column, ty)| transcript_cell(&row[column], ty.as_ref()))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        if serde_json::to_value(&rows).unwrap() != expected["rows"] {
            differences.push(format!(
                "{name}: expected {}, got {}",
                expected["rows"],
                serde_json::to_string(&rows).unwrap()
            ));
        }
        if let Some(view) = name.strip_prefix("definition_") {
            check_definition_execution(&engine, view, &result, &mut differences);
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

fn transcript_cell(value: &Value, ty: Option<&ColumnType>) -> Option<String> {
    Some(match value {
        Value::Null => return None,
        Value::Str(value) | Value::FixedChar(value) => value.clone(),
        Value::Int(value) => value.to_string(),
        Value::Bool(value) => if *value { "t" } else { "f" }.into(),
        value if matches!(ty, Some(ColumnType::OidVector)) => {
            uqa_sql::expr::vector_value_to_string(value).unwrap()
        }
        value => panic!("unexpected transcript value {value:?}"),
    })
}

fn check_definition_execution(
    engine: &Engine,
    view: &str,
    definitions: &SQLResult,
    differences: &mut Vec<String>,
) {
    let expected = exec(engine, &format!("SELECT * FROM {view}"));
    for column in &definitions.columns {
        let Value::Str(sql) = &definitions.rows[0][column] else {
            panic!("missing definition");
        };
        let Ok(actual) = engine.sql(sql, &[]) else {
            differences.push(format!("{view}/{column}: reconstructed SQL failed: {sql}"));
            continue;
        };
        let sorted = |result: &SQLResult| {
            let mut rows = result
                .rows
                .iter()
                .map(|row| format!("{row:?}"))
                .collect::<Vec<_>>();
            rows.sort();
            rows
        };
        if actual.columns != expected.columns
            || actual.column_types != expected.column_types
            || sorted(&actual) != sorted(&expected)
        {
            differences.push(format!("{view}/{column}: reconstructed query changed rows or declared types: {actual:?}; original: {expected:?}"));
        }
    }
}

fn definition(engine: &Engine, name: &str) -> String {
    let result = exec(
        engine,
        &format!("SELECT pg_get_viewdef('{name}') AS definition"),
    );
    let Value::Str(definition) = &result.rows[0]["definition"] else {
        panic!("missing view definition");
    };
    definition.clone()
}

#[test]
fn definitions_follow_names_search_path_transactions_and_reopen() {
    let directory = tempfile::TempDir::new().unwrap();
    let path = directory.path().join("definitions.uqa");
    let expected = " SELECT renamed_id AS exposed\n   FROM first.renamed_items;";
    {
        let engine = Engine::open(&path).unwrap();
        exec(
            &engine,
            "CREATE SCHEMA first; CREATE SCHEMA second;
            CREATE TABLE first.items(id int); CREATE TABLE second.items(id int);
            INSERT INTO first.items VALUES (7); INSERT INTO second.items VALUES (99);
            SET search_path=first,public; CREATE VIEW v(exposed) AS SELECT id FROM items;
            SET search_path=second,public",
        );
        assert_eq!(
            definition(&engine, "first.v"),
            " SELECT id AS exposed\n   FROM first.items;"
        );
        exec(
            &engine,
            "ALTER TABLE first.items RENAME TO renamed_items;
            ALTER TABLE first.renamed_items RENAME COLUMN id TO renamed_id;
            PREPARE view_def AS SELECT pg_get_viewdef('first.v') AS definition",
        );
        assert_eq!(definition(&engine, "first.v"), expected);
        exec(&engine, "BEGIN; SAVEPOINT before_replace;
            CREATE OR REPLACE VIEW first.v AS SELECT renamed_id + 10 AS exposed FROM first.renamed_items");
        let replacement = " SELECT (renamed_id + 10) AS exposed\n   FROM first.renamed_items;";
        assert_eq!(
            exec(&engine, "EXECUTE view_def").rows[0]["definition"],
            Value::Str(replacement.into())
        );
        exec(
            &engine,
            "ROLLBACK TO before_replace; ALTER VIEW first.v RENAME TO moved",
        );
        assert_eq!(definition(&engine, "first.moved"), expected);
        exec(&engine, "ROLLBACK");
        assert_eq!(
            exec(&engine, "EXECUTE view_def").rows[0]["definition"],
            Value::Str(expected.into())
        );
    }
    let engine = Engine::open(&path).unwrap();
    exec(&engine, "SET search_path=second,public");
    assert_eq!(definition(&engine, "first.v"), expected);
    assert_eq!(exec(&engine, expected).rows[0]["exposed"], Value::Int(7));
    let other = Engine::open(&path).unwrap();
    exec(
        &other,
        "ALTER TABLE first.renamed_items RENAME TO final_items",
    );
    assert_eq!(
        definition(&engine, "first.v"),
        " SELECT renamed_id AS exposed\n   FROM first.final_items;"
    );
}

#[test]
fn definition_catalog_visibility_and_schema_usage_follow_roles() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE ROLE definition_reader; CREATE SCHEMA definitions_private;
        CREATE TABLE definitions_private.source(id int);
        CREATE VIEW definitions_private.v AS SELECT id FROM definitions_private.source;
        GRANT USAGE ON SCHEMA definitions_private TO definition_reader;
        GRANT SELECT ON definitions_private.v TO definition_reader;
        SET ROLE definition_reader",
    );
    let result = exec(&engine, "SELECT view_definition FROM information_schema.views WHERE table_schema='definitions_private' AND table_name='v'");
    assert_eq!(result.rows[0]["view_definition"], Value::Null);
    assert_eq!(
        definition(&engine, "definitions_private.v"),
        " SELECT id\n   FROM definitions_private.source;"
    );
    let result = exec(
        &engine,
        "SELECT pg_get_viewdef(oid) AS definition FROM pg_class WHERE relname='v'",
    );
    assert!(matches!(result.rows[0]["definition"], Value::Str(_)));
    exec(&engine, "RESET ROLE; REVOKE USAGE ON SCHEMA definitions_private FROM definition_reader; SET ROLE definition_reader");
    let error = engine
        .sql("SELECT pg_get_viewdef('definitions_private.v')", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42501"));
    let result = exec(
        &engine,
        "SELECT pg_get_viewdef(oid) AS definition FROM pg_class WHERE relname='v'",
    );
    assert!(matches!(result.rows[0]["definition"], Value::Str(_)));
    exec(&engine, "RESET ROLE");
    let result = exec(&engine, "SELECT view_definition FROM information_schema.views WHERE table_schema='definitions_private' AND table_name='v'");
    assert!(matches!(result.rows[0]["view_definition"], Value::Str(_)));
}

#[test]
fn definitions_preserve_temp_foreign_and_quoted_relations() {
    let engine = Engine::new();
    exec(&engine, "CREATE SCHEMA \"Mixed.Schema\";
        CREATE TABLE \"Mixed.Schema\".\"Source.Table\"(\"Column.Name\" int);
        CREATE VIEW \"Mixed.Schema\".\"View.Name\"(\"Output.Name\") AS SELECT \"Column.Name\" FROM \"Mixed.Schema\".\"Source.Table\";
        CREATE TEMP TABLE temp_source(id int); CREATE TEMP VIEW temp_v AS SELECT id FROM temp_source;
        CREATE SERVER memory_server FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory');
        CREATE FOREIGN TABLE remote_source(id int) SERVER memory_server;
        CREATE VIEW remote_v AS SELECT id FROM remote_source");
    assert_eq!(
        definition(&engine, "\"Mixed.Schema\".\"View.Name\""),
        " SELECT \"Column.Name\" AS \"Output.Name\"\n   FROM \"Mixed.Schema\".\"Source.Table\";"
    );
    assert_eq!(
        definition(&engine, "pg_temp.temp_v"),
        " SELECT id\n   FROM temp_source;"
    );
    assert_eq!(
        definition(&engine, "remote_v"),
        " SELECT id\n   FROM remote_source;"
    );
    exec(
        &engine,
        "ALTER FOREIGN TABLE remote_source RENAME TO renamed_remote",
    );
    assert_eq!(
        definition(&engine, "remote_v"),
        " SELECT id\n   FROM renamed_remote;"
    );
}

#[test]
fn column_rename_keeps_nested_and_joined_view_bindings() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE rename_base(id int, label text);
        CREATE TABLE rename_other(id int, value int);
        INSERT INTO rename_base VALUES(1,'one'),(2,'two');
        INSERT INTO rename_other VALUES(1,10),(3,30);
        CREATE VIEW rename_join AS SELECT rename_base.id,rename_other.value FROM rename_base JOIN rename_other ON rename_base.id=rename_other.id;
        CREATE VIEW rename_using AS SELECT * FROM rename_base FULL JOIN rename_other USING(id);
        CREATE VIEW rename_using_alias AS SELECT j.id,j.label,j.value FROM (rename_base FULL JOIN rename_other USING(id)) j;
        CREATE VIEW rename_on_alias AS SELECT j.id,j.label,j.value FROM (rename_base LEFT JOIN rename_other ON rename_base.id=rename_other.id) j(id,label,other_id,value);
        CREATE VIEW rename_cte AS WITH chosen AS (SELECT id FROM rename_base) SELECT id FROM chosen;
        CREATE VIEW rename_sub AS SELECT q.id,(SELECT value FROM rename_other o WHERE o.id=q.id) AS value FROM (SELECT id FROM rename_base) q;
        CREATE VIEW rename_alias AS SELECT x.id FROM rename_base AS x(id,label)");
    let names = [
        "rename_join",
        "rename_using",
        "rename_using_alias",
        "rename_on_alias",
        "rename_cte",
        "rename_sub",
        "rename_alias",
    ];
    let before = names.map(|name| exec(&engine, &format!("SELECT * FROM {name} ORDER BY id")));
    let rule_oid = exec(
        &engine,
        "SELECT oid FROM pg_rewrite WHERE ev_class='rename_cte'::regclass",
    )
    .rows[0]["oid"]
        .clone();
    exec(
        &engine,
        "ALTER TABLE rename_base RENAME COLUMN id TO renamed_id;
        ALTER VIEW rename_cte RENAME TO renamed_cte",
    );
    for (index, name) in names.iter().enumerate() {
        let name = if *name == "rename_cte" {
            "renamed_cte"
        } else {
            name
        };
        let after = exec(&engine, &format!("SELECT * FROM {name} ORDER BY id"));
        assert_eq!(after.rows, before[index].rows, "{name}");
        assert_eq!(after.column_types, before[index].column_types, "{name}");
        let sql = definition(&engine, name);
        let result = engine
            .sql(&sql, &[])
            .unwrap_or_else(|error| panic!("{name}: {sql}: {error}"));
        assert_eq!(result.columns, before[index].columns, "{name}");
    }
    assert_eq!(
        exec(
            &engine,
            "SELECT oid FROM pg_rewrite WHERE ev_class='renamed_cte'::regclass"
        )
        .rows[0]["oid"],
        rule_oid
    );
}

#[test]
fn column_renames_preserve_all_view_fixture_rows_and_types() {
    let engine = Engine::new();
    for case in FIXTURE.split("-- @case ").skip(1) {
        let (header, body) = case.split_once('\n').unwrap();
        let name = header.split_whitespace().next().unwrap();
        if !name.starts_with("create_") && !name.starts_with("populate_") {
            continue;
        }
        let sql = body
            .split("-- @end")
            .next()
            .unwrap()
            .replace("__UQA_STATEFUL_SCHEMA__", "rename_definitions");
        if name != "create_schema" {
            exec(&engine, "SET search_path=rename_definitions,pg_catalog");
        }
        exec(&engine, &sql);
    }
    let views = exec(
        &engine,
        "SELECT viewname FROM pg_views WHERE schemaname='rename_definitions' ORDER BY viewname",
    );
    let before = views
        .rows
        .iter()
        .map(|row| {
            let Value::Str(name) = &row["viewname"] else {
                panic!("view name");
            };
            (
                name.clone(),
                exec(&engine, &format!("SELECT * FROM {name}")),
            )
        })
        .collect::<Vec<_>>();
    exec(&engine, "ALTER TABLE t RENAME COLUMN a TO renamed_a");
    for (name, before) in before {
        let after = exec(&engine, &format!("SELECT * FROM {name}"));
        assert_eq!(before.columns, after.columns, "{name}");
        assert_eq!(before.column_types, after.column_types, "{name}");
        let rows = |rows: &[uqa_sql::ResultRow]| {
            let mut rows = rows
                .iter()
                .map(|row| format!("{row:?}"))
                .collect::<Vec<_>>();
            rows.sort();
            rows
        };
        assert_eq!(rows(&before.rows), rows(&after.rows), "{name}");
        let sql = definition(&engine, &name);
        let rebuilt = engine
            .sql(&sql, &[])
            .unwrap_or_else(|error| panic!("{name}: {sql}: {error}"));
        assert_eq!(rows(&before.rows), rows(&rebuilt.rows), "{name}: {sql}");
    }
}

#[test]
fn string_agg_delimiters_survive_bounded_spill_execution() {
    let engine = Engine::new();
    exec(&engine, "SET work_mem='1B'");
    let expected: serde_json::Value = serde_json::from_str(EXPECTED).unwrap();
    for (case, expected) in FIXTURE
        .split("-- @case ")
        .skip(1)
        .zip(expected["cases"].as_array().unwrap())
    {
        let (header, body) = case.split_once('\n').unwrap();
        if ![
            "dynamic_string_delimiters",
            "distinct_string_delimiters",
            "string_bytea",
        ]
        .iter()
        .any(|name| header.starts_with(name))
        {
            continue;
        }
        let result = exec(&engine, body.split("-- @end").next().unwrap());
        let actual = transcript_cell(&result.rows[0]["result"], result.column_types[0].as_ref());
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            expected["rows"][0][0],
            "{header}"
        );
    }
}

#[test]
fn quoted_cte_names_bind_in_prepared_cursors() {
    let engine = Engine::new();
    exec(
        &engine,
        r#"PREPARE quoted_query AS WITH "Q.CTE"(n) AS (VALUES(1),(2)) SELECT n FROM "Q.CTE" ORDER BY n"#,
    );
    let expected = exec(&engine, "EXECUTE quoted_query");
    assert_eq!(expected.rows.len(), 2);
    assert_eq!(expected.rows[0]["n"], Value::Int(1));
    assert_eq!(expected.rows[1]["n"], Value::Int(2));
    exec(
        &engine,
        r#"BEGIN; DECLARE quoted_cursor CURSOR FOR WITH "Q.CTE"(n) AS (VALUES(1),(2)) SELECT n FROM "Q.CTE" ORDER BY n"#,
    );
    assert_eq!(
        exec(&engine, "FETCH ALL FROM quoted_cursor").rows,
        expected.rows
    );
    exec(&engine, "COMMIT");
}
