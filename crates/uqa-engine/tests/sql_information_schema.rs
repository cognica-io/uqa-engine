//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `information_schema.tables` / `information_schema.columns` and
//! `pg_catalog.pg_tables` virtual views.

use uqa_core::Value;
use uqa_engine::Engine;

#[test]
fn information_schema_tables_lists_user_tables() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE accounts (id INTEGER PRIMARY KEY, balance INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE TABLE owners (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    let r = eng
        .sql(
            "SELECT table_name FROM information_schema.tables ORDER BY table_name",
            &[],
        )
        .unwrap();
    let names: Vec<String> = r
        .rows
        .iter()
        .filter_map(|row| match row.get("table_name") {
            Some(uqa_core::Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert!(names.contains(&"accounts".to_string()));
    assert!(names.contains(&"owners".to_string()));
}

#[test]
fn information_schema_columns_lists_each_column() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE accounts (id INTEGER PRIMARY KEY, balance INTEGER, owner TEXT)",
        &[],
    )
    .unwrap();
    let r = eng
        .sql(
            "SELECT column_name, ordinal_position FROM information_schema.columns \
             WHERE table_name = 'accounts'",
            &[],
        )
        .unwrap();
    assert_eq!(r.rows.len(), 3);
}

#[test]
fn fixed_character_catalog_metadata_preserves_declared_length() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE labels (code CHAR(4))", &[]).unwrap();

    let info = eng
        .sql(
            "SELECT data_type, character_maximum_length, character_octet_length, udt_name \
             FROM information_schema.columns \
             WHERE table_name = 'labels' AND column_name = 'code'",
            &[],
        )
        .unwrap();
    assert_eq!(info.rows[0]["data_type"], Value::Str("character".into()));
    assert_eq!(info.rows[0]["character_maximum_length"], Value::Int(4));
    assert_eq!(info.rows[0]["character_octet_length"], Value::Int(16));
    assert_eq!(info.rows[0]["udt_name"], Value::Str("bpchar".into()));

    let attribute = eng
        .sql(
            "SELECT atttypid, atttypmod FROM pg_catalog.pg_attribute WHERE attname = 'code'",
            &[],
        )
        .unwrap();
    assert_eq!(attribute.rows[0]["atttypid"], Value::Int(1042));
    assert_eq!(attribute.rows[0]["atttypmod"], Value::Int(8));
}

#[test]
fn pg_tables_lists_user_tables() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE accounts (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    let r = eng
        .sql("SELECT tablename FROM pg_catalog.pg_tables", &[])
        .unwrap();
    let names: Vec<String> = r
        .rows
        .iter()
        .filter_map(|row| match row.get("tablename") {
            Some(uqa_core::Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert!(names.contains(&"accounts".to_string()));
}

#[test]
fn information_schema_lists_schemas_views_sequences_and_routines() {
    let eng = Engine::new();
    eng.sql("CREATE SCHEMA app", &[]).unwrap();
    eng.sql(
        "CREATE TABLE app.accounts (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE VIEW app.account_names AS SELECT name FROM app.accounts",
        &[],
    )
    .unwrap();
    eng.sql("CREATE SEQUENCE app.account_ids", &[]).unwrap();

    let schemas = eng
        .sql(
            "SELECT schema_name FROM information_schema.schemata ORDER BY schema_name",
            &[],
        )
        .unwrap();
    let schema_names: Vec<String> = schemas
        .rows
        .iter()
        .filter_map(|row| match row.get("schema_name") {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert!(schema_names.contains(&"app".to_string()));
    assert!(schema_names.contains(&"pg_catalog".to_string()));
    assert!(schema_names.contains(&"information_schema".to_string()));

    let views = eng
        .sql(
            "SELECT table_schema, table_name FROM information_schema.views \
             WHERE table_schema = 'app'",
            &[],
        )
        .unwrap();
    assert_eq!(views.rows.len(), 1);
    assert_eq!(
        views.rows[0]["table_name"],
        Value::Str("account_names".into())
    );

    let sequences = eng
        .sql(
            "SELECT sequence_schema, sequence_name FROM information_schema.sequences \
             WHERE sequence_schema = 'app'",
            &[],
        )
        .unwrap();
    assert_eq!(sequences.rows.len(), 1);
    assert_eq!(
        sequences.rows[0]["sequence_name"],
        Value::Str("account_ids".into())
    );

    let routines = eng
        .sql(
            "SELECT routine_name FROM information_schema.routines \
             WHERE routine_name = 'text_match'",
            &[],
        )
        .unwrap();
    assert_eq!(routines.rows.len(), 1);
}

#[test]
fn pg_catalog_exposes_namespace_class_and_attribute_rows() {
    let eng = Engine::new();
    eng.sql("CREATE SCHEMA app", &[]).unwrap();
    eng.sql(
        "CREATE TABLE app.accounts (id INTEGER PRIMARY KEY, balance INTEGER, owner TEXT NOT NULL)",
        &[],
    )
    .unwrap();

    let rels = eng
        .sql(
            "SELECT c.relname, c.relkind \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON c.relnamespace = n.oid \
             WHERE n.nspname = 'app' AND c.relname = 'accounts'",
            &[],
        )
        .unwrap();
    assert_eq!(rels.rows.len(), 1);
    assert_eq!(rels.rows[0]["relkind"], Value::Str("r".into()));

    let attrs = eng
        .sql(
            "SELECT a.attname, a.attnum, a.attnotnull \
             FROM pg_catalog.pg_attribute a \
             JOIN pg_catalog.pg_class c ON a.attrelid = c.oid \
             JOIN pg_catalog.pg_namespace n ON c.relnamespace = n.oid \
             WHERE n.nspname = 'app' AND c.relname = 'accounts' \
             ORDER BY a.attnum",
            &[],
        )
        .unwrap();
    let attr_names: Vec<String> = attrs
        .rows
        .iter()
        .filter_map(|row| match row.get("attname") {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(attr_names, vec!["id", "balance", "owner"]);
    assert_eq!(attrs.rows[0]["attnotnull"], Value::Bool(true));
    assert_eq!(attrs.rows[2]["attnotnull"], Value::Bool(true));
}

#[test]
fn postgresql_18_constraint_catalog_reports_names_and_enforcement() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE parent (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    eng.sql(
        "CREATE TABLE child (\
             id INTEGER PRIMARY KEY, \
             label TEXT CONSTRAINT label_nn NOT NULL, \
             score INTEGER CONSTRAINT score_positive CHECK (score > 0) NOT ENFORCED, \
             parent_id INTEGER CONSTRAINT parent_ref REFERENCES parent(id) NOT ENFORCED\
         )",
        &[],
    )
    .unwrap();

    let constraints = eng
        .sql(
            "SELECT conname, contype, conenforced, convalidated \
             FROM pg_catalog.pg_constraint \
             WHERE conname IN ('label_nn', 'score_positive', 'parent_ref') \
             ORDER BY conname",
            &[],
        )
        .unwrap();
    assert_eq!(constraints.rows.len(), 3);
    assert_eq!(
        constraints.rows[0]["conname"],
        Value::Str("label_nn".into())
    );
    assert_eq!(constraints.rows[0]["contype"], Value::Str("n".into()));
    assert_eq!(constraints.rows[0]["conenforced"], Value::Bool(true));
    assert_eq!(constraints.rows[0]["convalidated"], Value::Bool(true));
    assert_eq!(
        constraints.rows[1]["conname"],
        Value::Str("parent_ref".into())
    );
    assert_eq!(constraints.rows[1]["contype"], Value::Str("f".into()));
    assert_eq!(constraints.rows[1]["conenforced"], Value::Bool(false));
    assert_eq!(constraints.rows[1]["convalidated"], Value::Bool(false));
    assert_eq!(
        constraints.rows[2]["conname"],
        Value::Str("score_positive".into())
    );
    assert_eq!(constraints.rows[2]["contype"], Value::Str("c".into()));
    assert_eq!(constraints.rows[2]["conenforced"], Value::Bool(false));
    assert_eq!(constraints.rows[2]["convalidated"], Value::Bool(false));

    let info = eng
        .sql(
            "SELECT constraint_name, constraint_type, enforced \
             FROM information_schema.table_constraints \
             WHERE table_name = 'child' \
               AND constraint_name IN ('label_nn', 'score_positive', 'parent_ref') \
             ORDER BY constraint_name",
            &[],
        )
        .unwrap();
    assert_eq!(info.rows[0]["constraint_type"], Value::Str("CHECK".into()));
    assert_eq!(info.rows[0]["enforced"], Value::Str("YES".into()));
    assert_eq!(
        info.rows[1]["constraint_type"],
        Value::Str("FOREIGN KEY".into())
    );
    assert_eq!(info.rows[1]["enforced"], Value::Str("NO".into()));
    assert_eq!(info.rows[2]["constraint_type"], Value::Str("CHECK".into()));
    assert_eq!(info.rows[2]["enforced"], Value::Str("NO".into()));
}

#[test]
fn postgresql_18_constraint_catalog_preserves_table_and_composite_structure() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE parent (\
             p INTEGER PRIMARY KEY, \
             q INTEGER UNIQUE, \
             r INTEGER, \
             UNIQUE (r, q)\
         )",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE TABLE child (\
             id INTEGER PRIMARY KEY, \
             u INTEGER UNIQUE, \
             nn INTEGER NOT NULL, \
             ck INTEGER CHECK (ck > 0), \
             fk INTEGER REFERENCES parent(p) ON UPDATE CASCADE ON DELETE SET NULL, \
             a INTEGER, \
             b INTEGER, \
             CHECK (a < b), \
             UNIQUE NULLS NOT DISTINCT (a, b), \
             FOREIGN KEY (a, b) REFERENCES parent(q, r) MATCH FULL ON UPDATE RESTRICT ON DELETE CASCADE\
         )",
        &[],
    )
    .unwrap();

    let constraints = eng
        .sql(
            "SELECT conname, contype, conkey, confrelid, confkey, \
                    confupdtype, confdeltype, confmatchtype, connoinherit \
             FROM pg_catalog.pg_constraint \
             WHERE conrelid = (SELECT oid FROM pg_catalog.pg_class WHERE relname = 'child') \
             ORDER BY conname",
            &[],
        )
        .unwrap();
    let names: Vec<&str> = constraints
        .rows
        .iter()
        .map(|row| match &row["conname"] {
            Value::Str(name) => name.as_str(),
            value => panic!("expected constraint name, got {value:?}"),
        })
        .collect();
    assert_eq!(
        names,
        vec![
            "child_a_b_fkey",
            "child_a_b_key",
            "child_check",
            "child_ck_check",
            "child_fk_fkey",
            "child_id_not_null",
            "child_nn_not_null",
            "child_pkey",
            "child_u_key",
        ]
    );
    let row = |name: &str| {
        constraints
            .rows
            .iter()
            .find(|row| row["conname"] == Value::Str(name.into()))
            .unwrap_or_else(|| panic!("missing constraint `{name}`"))
    };
    assert_eq!(
        row("child_check")["conkey"],
        Value::List(vec![Value::Int(6), Value::Int(7)])
    );
    assert_eq!(
        row("child_a_b_key")["conkey"],
        Value::List(vec![Value::Int(6), Value::Int(7)])
    );
    assert_eq!(
        row("child_a_b_fkey")["confkey"],
        Value::List(vec![Value::Int(2), Value::Int(3)])
    );
    assert_ne!(row("child_a_b_fkey")["confrelid"], Value::Int(0));
    assert_eq!(row("child_a_b_fkey")["confupdtype"], Value::Str("r".into()));
    assert_eq!(row("child_a_b_fkey")["confdeltype"], Value::Str("c".into()));
    assert_eq!(
        row("child_a_b_fkey")["confmatchtype"],
        Value::Str("f".into())
    );
    assert_eq!(row("child_check")["connoinherit"], Value::Bool(false));
    assert_eq!(row("child_pkey")["connoinherit"], Value::Bool(true));

    let table_constraints = eng
        .sql(
            "SELECT constraint_name, constraint_type, nulls_distinct \
             FROM information_schema.table_constraints \
             WHERE table_name = 'child' \
             ORDER BY constraint_name",
            &[],
        )
        .unwrap();
    let composite_unique = table_constraints
        .rows
        .iter()
        .find(|row| row["constraint_name"] == Value::Str("child_a_b_key".into()))
        .expect("composite UNIQUE constraint");
    assert_eq!(
        composite_unique["constraint_type"],
        Value::Str("UNIQUE".into())
    );
    assert_eq!(composite_unique["nulls_distinct"], Value::Str("NO".into()));
    assert_eq!(
        table_constraints
            .rows
            .iter()
            .find(|row| row["constraint_name"] == Value::Str("child_pkey".into()))
            .expect("primary key")["nulls_distinct"],
        Value::Null
    );

    let key_columns = eng
        .sql(
            "SELECT constraint_name, column_name, ordinal_position, position_in_unique_constraint \
             FROM information_schema.key_column_usage \
             WHERE table_name = 'child' \
             ORDER BY constraint_name, ordinal_position",
            &[],
        )
        .unwrap();
    let composite_fk: Vec<_> = key_columns
        .rows
        .iter()
        .filter(|row| row["constraint_name"] == Value::Str("child_a_b_fkey".into()))
        .collect();
    assert_eq!(composite_fk.len(), 2);
    assert_eq!(composite_fk[0]["column_name"], Value::Str("a".into()));
    assert_eq!(composite_fk[0]["ordinal_position"], Value::Int(1));
    assert_eq!(
        composite_fk[0]["position_in_unique_constraint"],
        Value::Int(2)
    );
    assert_eq!(composite_fk[1]["column_name"], Value::Str("b".into()));
    assert_eq!(composite_fk[1]["ordinal_position"], Value::Int(2));
    assert_eq!(
        composite_fk[1]["position_in_unique_constraint"],
        Value::Int(1)
    );
    let scalar_unique = key_columns
        .rows
        .iter()
        .find(|row| row["constraint_name"] == Value::Str("child_u_key".into()))
        .expect("scalar UNIQUE constraint");
    assert_eq!(scalar_unique["ordinal_position"], Value::Int(1));

    eng.sql("ALTER TABLE child RENAME TO renamed_child", &[])
        .unwrap();
    let renamed = eng
        .sql(
            "SELECT conname FROM pg_catalog.pg_constraint \
             WHERE conrelid = (SELECT oid FROM pg_catalog.pg_class WHERE relname = 'renamed_child') \
             ORDER BY conname",
            &[],
        )
        .unwrap();
    let renamed_names: Vec<&str> = renamed
        .rows
        .iter()
        .map(|row| match &row["conname"] {
            Value::Str(name) => name.as_str(),
            value => panic!("expected constraint name, got {value:?}"),
        })
        .collect();
    assert_eq!(renamed_names, names);
}

#[test]
fn generated_constraint_names_survive_table_rename_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("constraint-names.db");
    let expected = vec![
        "durable_a_check".to_string(),
        "durable_a_key".to_string(),
        "durable_a_not_null".to_string(),
        "durable_id_not_null".to_string(),
        "durable_pkey".to_string(),
    ];
    {
        let engine = Engine::open(&path).unwrap();
        engine
            .sql(
                "CREATE TABLE durable (id INTEGER PRIMARY KEY, a INTEGER NOT NULL CHECK (a > 0), UNIQUE (a))",
                &[],
            )
            .unwrap();
        engine
            .sql("ALTER TABLE durable RENAME TO renamed_durable", &[])
            .unwrap();
    }
    let engine = Engine::open(&path).unwrap();
    let result = engine
        .sql(
            "SELECT conname FROM pg_catalog.pg_constraint \
             WHERE conrelid = (SELECT oid FROM pg_catalog.pg_class WHERE relname = 'renamed_durable') \
             ORDER BY conname",
            &[],
        )
        .unwrap();
    let names: Vec<String> = result
        .rows
        .iter()
        .map(|row| match &row["conname"] {
            Value::Str(name) => name.clone(),
            value => panic!("expected constraint name, got {value:?}"),
        })
        .collect();
    assert_eq!(names, expected);
}

#[test]
fn pg_catalog_exposes_indexes_types_functions_and_roles() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)", &[])
        .unwrap();
    eng.sql("CREATE INDEX docs_body_idx ON docs USING gin (body)", &[])
        .unwrap();
    eng.sql("SET search_path TO public", &[]).unwrap();

    let indexes = eng
        .sql(
            "SELECT schemaname, tablename, indexname, indexdef \
             FROM pg_catalog.pg_indexes \
             WHERE tablename = 'docs'",
            &[],
        )
        .unwrap();
    assert_eq!(indexes.rows.len(), 1);
    assert_eq!(
        indexes.rows[0]["indexname"],
        Value::Str("docs_body_idx".into())
    );
    match &indexes.rows[0]["indexdef"] {
        Value::Str(def) => assert!(def.contains("USING gin")),
        other => panic!("expected indexdef string, got {other:?}"),
    }

    let pg_index = eng
        .sql(
            "SELECT i.indisvalid \
             FROM pg_catalog.pg_index i \
             JOIN pg_catalog.pg_class c ON i.indexrelid = c.oid \
             WHERE c.relname = 'docs_body_idx'",
            &[],
        )
        .unwrap();
    assert_eq!(pg_index.rows.len(), 1);
    assert_eq!(pg_index.rows[0]["indisvalid"], Value::Bool(true));

    let types = eng
        .sql("SELECT typname FROM pg_catalog.pg_type WHERE oid = 23", &[])
        .unwrap();
    assert_eq!(types.rows[0]["typname"], Value::Str("int4".into()));

    let procs = eng
        .sql(
            "SELECT proname FROM pg_catalog.pg_proc WHERE proname = 'deep_predict'",
            &[],
        )
        .unwrap();
    assert_eq!(procs.rows.len(), 1);

    let roles = eng
        .sql("SELECT rolname, rolcanlogin FROM pg_catalog.pg_roles", &[])
        .unwrap();
    assert_eq!(roles.rows[0]["rolname"], Value::Str("uqa".into()));
    assert_eq!(roles.rows[0]["rolcanlogin"], Value::Bool(true));

    let settings = eng
        .sql(
            "SELECT setting FROM pg_catalog.pg_settings WHERE name = 'search_path'",
            &[],
        )
        .unwrap();
    assert_eq!(settings.rows[0]["setting"], Value::Str("public".into()));
}

#[test]
fn postgresql_18_builtin_function_catalog_preserves_overloads_and_metadata() {
    let engine = Engine::new();
    let routines = engine
        .sql(
            "SELECT oid, proname, prokind, proisstrict, proleakproof, provolatile, \
                    pronargs, pronargdefaults, prorettype, proargtypes, proargnames, prosrc \
             FROM pg_catalog.pg_proc \
             WHERE oid IN (3261, 6364, 6383, 6389, 6390, 6429, 6430) \
             ORDER BY oid",
            &[],
        )
        .unwrap();
    assert_eq!(routines.rows.len(), 7);
    let row = |oid: i64| {
        routines
            .rows
            .iter()
            .find(|row| row["oid"] == Value::Int(oid))
            .unwrap_or_else(|| panic!("missing pg_proc row {oid}"))
    };
    assert_eq!(row(3261)["pronargs"], Value::Int(2));
    assert_eq!(row(3261)["pronargdefaults"], Value::Int(1));
    assert_eq!(row(3261)["prorettype"], Value::Int(114));
    assert_eq!(
        row(3261)["proargnames"],
        Value::List(vec![
            Value::Str("target".into()),
            Value::Str("strip_in_arrays".into()),
        ])
    );
    assert_eq!(row(6364)["proleakproof"], Value::Bool(true));
    assert_eq!(row(6383)["prosrc"], Value::Str("dgamma".into()));
    assert_eq!(
        row(6389)["proargtypes"],
        Value::List(vec![Value::Int(2277), Value::Int(16)])
    );
    assert_eq!(row(6390)["pronargs"], Value::Int(3));
    assert_eq!(row(6429)["provolatile"], Value::Str("v".into()));
    assert_eq!(row(6430)["prorettype"], Value::Int(2950));

    let information_schema = engine
        .sql(
            "SELECT specific_name, data_type, is_deterministic, external_language \
             FROM information_schema.routines \
             WHERE routine_name = 'uuidv7' \
             ORDER BY specific_name",
            &[],
        )
        .unwrap();
    assert_eq!(information_schema.rows.len(), 2);
    assert_eq!(
        information_schema.rows[0]["specific_name"],
        Value::Str("uuidv7_6429".into())
    );
    assert_eq!(
        information_schema.rows[1]["specific_name"],
        Value::Str("uuidv7_6430".into())
    );
    for row in information_schema.rows {
        assert_eq!(row["data_type"], Value::Str("uuid".into()));
        assert_eq!(row["is_deterministic"], Value::Str("NO".into()));
        assert_eq!(row["external_language"], Value::Str("INTERNAL".into()));
    }
}
