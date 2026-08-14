//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `information_schema.tables` / `information_schema.columns` and
//! `pg_catalog.pg_tables` virtual views.

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::{ast::ColumnType, ResultRow};

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
fn empty_pg18_catalog_relations_keep_their_declared_row_types() {
    let eng = Engine::new();

    let descriptions = eng
        .sql("SELECT * FROM pg_catalog.pg_description", &[])
        .unwrap();
    assert_eq!(
        descriptions.columns,
        ["objoid", "classoid", "objsubid", "description"]
    );
    assert_eq!(
        descriptions.column_types,
        [
            Some(ColumnType::Oid),
            Some(ColumnType::Oid),
            Some(ColumnType::Integer),
            Some(ColumnType::Text),
        ]
    );
    assert!(descriptions.rows.is_empty());

    let matviews = eng
        .sql("SELECT * FROM pg_catalog.pg_matviews", &[])
        .unwrap();
    assert_eq!(
        matviews.columns,
        [
            "schemaname",
            "matviewname",
            "matviewowner",
            "tablespace",
            "hasindexes",
            "ispopulated",
            "definition",
        ]
    );
    assert_eq!(
        matviews.column_types,
        [
            Some(ColumnType::Name),
            Some(ColumnType::Name),
            Some(ColumnType::Name),
            Some(ColumnType::Name),
            Some(ColumnType::Boolean),
            Some(ColumnType::Boolean),
            Some(ColumnType::Text),
        ]
    );
    assert!(matviews.rows.is_empty());
}

#[test]
fn pg18_catalog_star_uses_catalog_order_and_excludes_removed_columns() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE defaults (value INTEGER DEFAULT 7)", &[])
        .unwrap();

    let attrdefs = eng.sql("SELECT * FROM pg_catalog.pg_attrdef", &[]).unwrap();
    assert_eq!(attrdefs.columns, ["oid", "adrelid", "adnum", "adbin"]);
    assert_eq!(
        attrdefs.column_types,
        [
            Some(ColumnType::Oid),
            Some(ColumnType::Oid),
            Some(ColumnType::SmallInteger),
            Some(ColumnType::PgNodeTree),
        ]
    );
    assert_eq!(attrdefs.rows.len(), 1);
    assert!(!attrdefs.rows[0].contains_key("adsrc"));
}

#[test]
fn information_schema_uses_pg18_domain_type_identities() {
    let eng = Engine::new();
    let result = eng
        .sql(
            "SELECT table_catalog, ordinal_position, is_nullable \
             FROM information_schema.columns WHERE table_name = '__missing__'",
            &[],
        )
        .unwrap();

    assert!(result.rows.is_empty());
    assert_eq!(
        result.column_types,
        [
            Some(ColumnType::Domain {
                schema: "information_schema".into(),
                name: "sql_identifier".into(),
                oid: 13_312,
                base: Box::new(ColumnType::Name),
            }),
            Some(ColumnType::Domain {
                schema: "information_schema".into(),
                name: "cardinal_number".into(),
                oid: 13_307,
                base: Box::new(ColumnType::Integer),
            }),
            Some(ColumnType::Domain {
                schema: "information_schema".into(),
                name: "yes_or_no".into(),
                oid: 13_320,
                base: Box::new(ColumnType::Varchar(Some(3))),
            }),
        ]
    );
}

#[test]
fn information_schema_routines_has_the_pg18_shape() {
    let eng = Engine::new();
    let result = eng
        .sql(
            "SELECT * FROM information_schema.routines WHERE routine_name = '__missing__'",
            &[],
        )
        .unwrap();

    assert!(result.rows.is_empty());
    assert_eq!(result.columns.len(), 82);
    assert_eq!(result.columns[0], "specific_catalog");
    assert_eq!(result.columns[54], "created");
    assert_eq!(result.columns[81], "result_cast_dtd_identifier");
    assert!(!result
        .columns
        .iter()
        .any(|column| column == "result_cast_from_null"));
    assert!(matches!(
        result.column_types[54],
        Some(ColumnType::Domain { oid: 13_318, .. })
    ));
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

fn create_constraint_catalog_fixture(eng: &Engine) {
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
}

fn assert_pg_constraint_structure(eng: &Engine) -> Vec<String> {
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
    let names: Vec<String> = constraints
        .rows
        .iter()
        .map(|row| match &row["conname"] {
            Value::Str(name) => name.clone(),
            value => panic!("expected constraint name, got {value:?}"),
        })
        .collect();
    assert_eq!(
        names,
        vec![
            "child_a_b_fkey".to_string(),
            "child_a_b_key".to_string(),
            "child_check".to_string(),
            "child_ck_check".to_string(),
            "child_fk_fkey".to_string(),
            "child_id_not_null".to_string(),
            "child_nn_not_null".to_string(),
            "child_pkey".to_string(),
            "child_u_key".to_string(),
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
    names
}

fn assert_information_schema_constraint_structure(eng: &Engine) {
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
}

#[test]
fn postgresql_18_constraint_catalog_preserves_table_and_composite_structure() {
    let eng = Engine::new();
    create_constraint_catalog_fixture(&eng);
    let names = assert_pg_constraint_structure(&eng);
    assert_information_schema_constraint_structure(&eng);

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
    let renamed_names: Vec<String> = renamed
        .rows
        .iter()
        .map(|row| match &row["conname"] {
            Value::Str(name) => name.clone(),
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
fn pg_catalog_type_storage_metadata_matches_postgresql_18() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE type_layouts (
            small_value SMALLINT,
            big_value BIGINT,
            boolean_value BOOLEAN,
            text_value TEXT,
            name_value NAME,
            uuid_value UUID,
            real_value REAL,
            double_value DOUBLE PRECISION,
            interval_value INTERVAL,
            timetz_value TIME WITH TIME ZONE,
            numeric_value NUMERIC,
            big_array BIGINT[]
        )",
        &[],
    )
    .unwrap();

    let attributes = eng
        .sql(
            "SELECT attname, attlen, attbyval, attalign, attstorage
             FROM pg_catalog.pg_attribute
             WHERE attrelid = (
                 SELECT oid FROM pg_catalog.pg_class WHERE relname = 'type_layouts'
             )
             ORDER BY attnum",
            &[],
        )
        .unwrap();
    let layouts = attributes
        .rows
        .iter()
        .map(|row| {
            (
                row["attname"].clone(),
                row["attlen"].clone(),
                row["attbyval"].clone(),
                row["attalign"].clone(),
                row["attstorage"].clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        layouts,
        vec![
            layout("small_value", 2, true, "s", "p"),
            layout("big_value", 8, true, "d", "p"),
            layout("boolean_value", 1, true, "c", "p"),
            layout("text_value", -1, false, "i", "x"),
            layout("name_value", 64, false, "c", "p"),
            layout("uuid_value", 16, false, "c", "p"),
            layout("real_value", 4, true, "i", "p"),
            layout("double_value", 8, true, "d", "p"),
            layout("interval_value", 16, false, "d", "p"),
            layout("timetz_value", 12, false, "d", "p"),
            layout("numeric_value", -1, false, "i", "m"),
            layout("big_array", -1, false, "d", "x"),
        ]
    );
}

#[test]
fn pg_catalog_scalar_and_array_type_storage_metadata_matches_postgresql_18() {
    let eng = Engine::new();
    let types = eng
        .sql(
            "SELECT typname, typlen, typbyval, typalign, typstorage, typelem, typarray
             FROM pg_catalog.pg_type
             WHERE typname IN ('int8', '_int8', 'timetz')
             ORDER BY oid",
            &[],
        )
        .unwrap();
    assert_eq!(types.rows.len(), 3);
    assert_eq!(types.rows[0]["typname"], Value::Str("int8".into()));
    assert_eq!(types.rows[0]["typarray"], Value::Int(1016));
    assert_eq!(types.rows[1]["typname"], Value::Str("_int8".into()));
    assert_eq!(types.rows[1]["typelem"], Value::Int(20));
    assert_eq!(types.rows[1]["typalign"], Value::Str("d".into()));
    assert_eq!(types.rows[2]["typname"], Value::Str("timetz".into()));
    assert_eq!(types.rows[2]["typlen"], Value::Int(12));
}

#[test]
fn pg_catalog_system_type_storage_metadata_matches_postgresql_18() {
    let eng = Engine::new();
    let system_types = eng
        .sql(
            "SELECT typname, typlen, typbyval, typtype, typcategory, typispreferred,
                    typalign, typstorage, typelem, typarray, typsubscript, typcollation
             FROM pg_catalog.pg_type
             WHERE typname IN ('char', 'int2vector', 'regproc', 'oid', 'xid',
                               'oidvector', 'pg_node_tree', 'aclitem', 'regtype', 'anyarray')
             ORDER BY oid",
            &[],
        )
        .unwrap();
    let system_layouts = system_types
        .rows
        .iter()
        .map(pg_type_layout)
        .collect::<Vec<_>>();
    assert_eq!(
        system_layouts,
        vec![
            type_layout("char", 1, true, "b", "Z", false, "c", "p", 0, 1002, "-", 0),
            type_layout(
                "int2vector",
                -1,
                false,
                "b",
                "A",
                false,
                "i",
                "p",
                21,
                1006,
                "array_subscript_handler",
                0,
            ),
            type_layout("regproc", 4, true, "b", "N", false, "i", "p", 0, 1008, "-", 0),
            type_layout("oid", 4, true, "b", "N", true, "i", "p", 0, 1028, "-", 0),
            type_layout("xid", 4, true, "b", "U", false, "i", "p", 0, 1011, "-", 0),
            type_layout(
                "oidvector",
                -1,
                false,
                "b",
                "A",
                false,
                "i",
                "p",
                26,
                1013,
                "array_subscript_handler",
                0,
            ),
            type_layout(
                "pg_node_tree",
                -1,
                false,
                "b",
                "Z",
                false,
                "i",
                "x",
                0,
                0,
                "-",
                100,
            ),
            type_layout("aclitem", 16, false, "b", "U", false, "d", "p", 0, 1034, "-", 0,),
            type_layout("regtype", 4, true, "b", "N", false, "i", "p", 0, 2211, "-", 0,),
            type_layout("anyarray", -1, false, "p", "P", false, "d", "x", 0, 0, "-", 0,),
        ]
    );
}

#[test]
fn pg_catalog_system_type_arrays_match_postgresql_18() {
    let eng = Engine::new();
    let system_arrays = eng
        .sql(
            "SELECT base.typname AS base_name, array_type.typname AS array_name, array_type.typelem
             FROM pg_catalog.pg_type AS base
             JOIN pg_catalog.pg_type AS array_type ON array_type.oid = base.typarray
             WHERE base.typname IN ('char', 'int2vector', 'regproc', 'oid', 'xid',
                                    'oidvector', 'aclitem', 'regtype')
             ORDER BY base.oid",
            &[],
        )
        .unwrap();
    assert_eq!(system_arrays.rows.len(), 8);
    assert_eq!(
        system_arrays.rows[0]["array_name"],
        Value::Str("_char".into())
    );
    assert_eq!(system_arrays.rows[0]["typelem"], Value::Int(18));
    assert_eq!(
        system_arrays.rows[7]["array_name"],
        Value::Str("_regtype".into())
    );
    assert_eq!(system_arrays.rows[7]["typelem"], Value::Int(2206));
}

#[test]
fn information_schema_domain_storage_metadata_matches_postgresql_18() {
    let eng = Engine::new();
    let namespace = eng
        .sql(
            "SELECT oid FROM pg_catalog.pg_namespace WHERE nspname = 'information_schema'",
            &[],
        )
        .unwrap();
    assert_eq!(namespace.rows[0]["oid"], Value::Int(13_293));

    let domains = eng
        .sql(
            "SELECT typname, oid, typlen, typbyval, typcategory, typarray,
                    typalign, typstorage, typcollation, typbasetype, typtypmod
             FROM pg_catalog.pg_type
             WHERE typnamespace = 13293 AND typtype = 'd'
             ORDER BY oid",
            &[],
        )
        .unwrap();
    let domain_layouts = domains
        .rows
        .iter()
        .map(|row| {
            [
                "typname",
                "oid",
                "typlen",
                "typbyval",
                "typcategory",
                "typarray",
                "typalign",
                "typstorage",
                "typcollation",
                "typbasetype",
                "typtypmod",
            ]
            .into_iter()
            .map(|column| row[column].clone())
            .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        domain_layouts,
        pg18_routines::expected_information_schema_domain_layouts()
    );
}

#[test]
fn information_schema_domain_arrays_match_postgresql_18() {
    let eng = Engine::new();
    let domain_arrays = eng
        .sql(
            "SELECT domain_type.typname AS domain_name, array_type.typname AS array_name,
                    array_type.oid AS array_oid, array_type.typelem
             FROM pg_catalog.pg_type AS domain_type
             JOIN pg_catalog.pg_type AS array_type ON array_type.oid = domain_type.typarray
             WHERE domain_type.typnamespace = 13293 AND domain_type.typtype = 'd'
             ORDER BY domain_type.oid",
            &[],
        )
        .unwrap();
    assert_eq!(domain_arrays.rows.len(), 5);
    assert_eq!(
        domain_arrays.rows[0]["array_name"],
        Value::Str("_cardinal_number".into())
    );
    assert_eq!(domain_arrays.rows[0]["array_oid"], Value::Int(13_306));
    assert_eq!(domain_arrays.rows[0]["typelem"], Value::Int(13_307));
    assert_eq!(
        domain_arrays.rows[4]["array_name"],
        Value::Str("_yes_or_no".into())
    );
    assert_eq!(domain_arrays.rows[4]["array_oid"], Value::Int(13_319));
    assert_eq!(domain_arrays.rows[4]["typelem"], Value::Int(13_320));
}

fn layout(
    name: &str,
    len: i64,
    by_value: bool,
    align: &str,
    storage: &str,
) -> (Value, Value, Value, Value, Value) {
    (
        Value::Str(name.into()),
        Value::Int(len),
        Value::Bool(by_value),
        Value::Str(align.into()),
        Value::Str(storage.into()),
    )
}

#[allow(clippy::too_many_arguments)]
fn type_layout(
    name: &str,
    len: i64,
    by_value: bool,
    kind: &str,
    category: &str,
    preferred: bool,
    align: &str,
    storage: &str,
    element_oid: i64,
    array_oid: i64,
    subscript: &str,
    collation_oid: i64,
) -> Vec<Value> {
    vec![
        Value::Str(name.into()),
        Value::Int(len),
        Value::Bool(by_value),
        Value::Str(kind.into()),
        Value::Str(category.into()),
        Value::Bool(preferred),
        Value::Str(align.into()),
        Value::Str(storage.into()),
        Value::Int(element_oid),
        Value::Int(array_oid),
        Value::Str(subscript.into()),
        Value::Int(collation_oid),
    ]
}

fn pg_type_layout(row: &ResultRow) -> Vec<Value> {
    [
        "typname",
        "typlen",
        "typbyval",
        "typtype",
        "typcategory",
        "typispreferred",
        "typalign",
        "typstorage",
        "typelem",
        "typarray",
        "typsubscript",
        "typcollation",
    ]
    .into_iter()
    .map(|column| row[column].clone())
    .collect()
}

#[allow(clippy::too_many_arguments)]
fn domain_layout(
    name: &str,
    oid: i64,
    len: i64,
    by_value: bool,
    category: &str,
    array_oid: i64,
    align: &str,
    storage: &str,
    collation_oid: i64,
    base_oid: i64,
    type_modifier: i64,
) -> Vec<Value> {
    vec![
        Value::Str(name.into()),
        Value::Int(oid),
        Value::Int(len),
        Value::Bool(by_value),
        Value::Str(category.into()),
        Value::Int(array_oid),
        Value::Str(align.into()),
        Value::Str(storage.into()),
        Value::Int(collation_oid),
        Value::Int(base_oid),
        Value::Int(type_modifier),
    ]
}

#[test]
fn postgresql_18_type_catalog_preserves_io_routines_and_pseudo_types() {
    let eng = Engine::new();

    let pseudo_types = eng
        .sql(
            "SELECT oid, typname, typnamespace, typowner, typlen, typbyval, typtype,
                    typcategory, typispreferred, typisdefined, typdelim, typrelid,
                    typsubscript, typelem, typarray, typinput, typoutput, typreceive,
                    typsend, typmodin, typmodout, typanalyze, typalign, typstorage,
                    typnotnull, typbasetype, typtypmod, typndims, typcollation
             FROM pg_catalog.pg_type
             WHERE oid IN (2249, 2278, 2287)
             ORDER BY oid",
            &[],
        )
        .unwrap();
    assert_eq!(
        pseudo_types
            .rows
            .iter()
            .map(pg_type_full_layout)
            .collect::<Vec<_>>(),
        vec![
            pseudo_type_layout(
                2249, "record", -1, false, 0, "-", 0, 2287, 2290, 2291, 2402, 2403, 0, "d", "x",
            ),
            pseudo_type_layout(
                2278, "void", 4, true, 0, "-", 0, 0, 2298, 2299, 3120, 3121, 0, "i", "p",
            ),
            pseudo_type_layout(
                2287,
                "_record",
                -1,
                false,
                0,
                "array_subscript_handler",
                2249,
                0,
                750,
                751,
                2400,
                2401,
                3816,
                "d",
                "x",
            ),
        ]
    );

    let routine_types = eng
        .sql(
            "SELECT typname, typinput, typoutput, typreceive, typsend,
                    typmodin, typmodout, typanalyze
             FROM pg_catalog.pg_type
             WHERE typname IN (
                 'bool', 'int4', 'oid', 'bpchar', 'timestamptz', 'numeric', 'jsonb',
                 'aclitem', '_int4', '_bpchar', '_numeric', '_aclitem',
                 'cardinal_number', 'sql_identifier',
                 '_cardinal_number', '_sql_identifier'
             )",
            &[],
        )
        .unwrap();
    for (name, expected) in [
        ("bool", [1242, 1243, 2436, 2437, 0, 0, 0]),
        ("int4", [42, 43, 2406, 2407, 0, 0, 0]),
        ("oid", [1798, 1799, 2418, 2419, 0, 0, 0]),
        ("bpchar", [1044, 1045, 2430, 2431, 2913, 2914, 0]),
        ("timestamptz", [1150, 1151, 2476, 2477, 2907, 2908, 0]),
        ("numeric", [1701, 1702, 2460, 2461, 2917, 2918, 0]),
        ("jsonb", [3806, 3804, 3805, 3803, 0, 0, 0]),
        ("aclitem", [1031, 1032, 0, 0, 0, 0, 0]),
        ("_int4", [750, 751, 2400, 2401, 0, 0, 3816]),
        ("_bpchar", [750, 751, 2400, 2401, 2913, 2914, 3816]),
        ("_numeric", [750, 751, 2400, 2401, 2917, 2918, 3816]),
        ("_aclitem", [750, 751, 2400, 2401, 0, 0, 3816]),
        ("cardinal_number", [2597, 43, 2598, 2407, 0, 0, 0]),
        ("sql_identifier", [2597, 35, 2598, 2423, 0, 0, 0]),
        ("_cardinal_number", [750, 751, 2400, 2401, 0, 0, 3816]),
        ("_sql_identifier", [750, 751, 2400, 2401, 0, 0, 3816]),
    ] {
        let row = routine_types
            .rows
            .iter()
            .find(|row| row["typname"] == Value::Str(name.into()))
            .unwrap_or_else(|| panic!("missing PostgreSQL 18 type {name}"));
        assert_eq!(pg_type_routine_layout(row), expected, "type {name}");
    }
}

#[test]
fn information_schema_catalog_name_preserves_its_pg18_composite_identity() {
    let eng = Engine::new();

    let catalog_name = eng
        .sql(
            "SELECT * FROM information_schema.information_schema_catalog_name",
            &[],
        )
        .unwrap();
    assert_eq!(catalog_name.columns, ["catalog_name"]);
    assert_eq!(
        catalog_name.column_types,
        [Some(ColumnType::Domain {
            schema: "information_schema".into(),
            name: "sql_identifier".into(),
            oid: 13_312,
            base: Box::new(ColumnType::Name),
        })]
    );
    assert_eq!(catalog_name.rows.len(), 1);
    assert_eq!(
        catalog_name.rows[0]["catalog_name"],
        Value::Str("uqa".into())
    );
}

#[test]
fn information_schema_catalog_name_preserves_its_pg18_class_identity() {
    let eng = Engine::new();
    let class = eng
        .sql(
            "SELECT oid, relname, relnamespace, reltype, relowner, relpages, reltuples,
                    relallvisible, relallfrozen, relhasindex, relpersistence, relkind,
                    relnatts, relchecks, relhasrules, relhastriggers, relispopulated,
                    relreplident, relispartition
             FROM pg_catalog.pg_class
             WHERE oid = 13313",
            &[],
        )
        .unwrap();
    let class = &class.rows[0];
    for (column, expected) in [
        ("oid", Value::Int(13_313)),
        (
            "relname",
            Value::Str("information_schema_catalog_name".into()),
        ),
        ("relnamespace", Value::Int(13_293)),
        ("reltype", Value::Int(13_315)),
        ("relowner", Value::Int(10)),
        ("relpages", Value::Int(0)),
        ("reltuples", Value::Float(-1.0)),
        ("relallvisible", Value::Int(0)),
        ("relallfrozen", Value::Int(0)),
        ("relhasindex", Value::Bool(false)),
        ("relpersistence", Value::Str("p".into())),
        ("relkind", Value::Str("v".into())),
        ("relnatts", Value::Int(1)),
        ("relchecks", Value::Int(0)),
        ("relhasrules", Value::Bool(true)),
        ("relhastriggers", Value::Bool(false)),
        ("relispopulated", Value::Bool(true)),
        ("relreplident", Value::Str("n".into())),
        ("relispartition", Value::Bool(false)),
    ] {
        assert_eq!(class[column], expected, "pg_class.{column}");
    }
}

#[test]
fn information_schema_catalog_name_preserves_its_pg18_attribute_identity() {
    let eng = Engine::new();
    let attribute = eng
        .sql(
            "SELECT attrelid, attname, atttypid, attstattarget, attlen, attnum,
                    atttypmod, attndims, attbyval, attalign, attstorage, attcompression,
                    attnotnull, atthasdef, atthasmissing, attidentity, attgenerated,
                    attisdropped, attislocal, attinhcount, attcollation
             FROM pg_catalog.pg_attribute
             WHERE attrelid = 13313 AND attnum = 1",
            &[],
        )
        .unwrap();
    let attribute = &attribute.rows[0];
    for (column, expected) in [
        ("attrelid", Value::Int(13_313)),
        ("attname", Value::Str("catalog_name".into())),
        ("atttypid", Value::Int(13_312)),
        ("attstattarget", Value::Null),
        ("attlen", Value::Int(64)),
        ("attnum", Value::Int(1)),
        ("atttypmod", Value::Int(-1)),
        ("attndims", Value::Int(0)),
        ("attbyval", Value::Bool(false)),
        ("attalign", Value::Str("c".into())),
        ("attstorage", Value::Str("p".into())),
        ("attcompression", Value::Str(String::new())),
        ("attnotnull", Value::Bool(false)),
        ("atthasdef", Value::Bool(false)),
        ("atthasmissing", Value::Bool(false)),
        ("attidentity", Value::Str(String::new())),
        ("attgenerated", Value::Str(String::new())),
        ("attisdropped", Value::Bool(false)),
        ("attislocal", Value::Bool(true)),
        ("attinhcount", Value::Int(0)),
        ("attcollation", Value::Int(950)),
    ] {
        assert_eq!(attribute[column], expected, "pg_attribute.{column}");
    }
}

#[test]
fn information_schema_catalog_name_preserves_its_pg18_type_identity() {
    let eng = Engine::new();
    let types = eng
        .sql(
            "SELECT oid, typname, typnamespace, typtype, typcategory, typrelid,
                    typsubscript, typelem, typarray, typinput, typoutput, typreceive,
                    typsend, typmodin, typmodout, typanalyze, typalign, typstorage
             FROM pg_catalog.pg_type
             WHERE oid IN (13314, 13315)
             ORDER BY oid",
            &[],
        )
        .unwrap();
    assert_eq!(types.rows.len(), 2);
    let array_type = &types.rows[0];
    assert_eq!(
        array_type["typname"],
        Value::Str("_information_schema_catalog_name".into())
    );
    assert_eq!(array_type["typtype"], Value::Str("b".into()));
    assert_eq!(array_type["typcategory"], Value::Str("A".into()));
    assert_eq!(array_type["typrelid"], Value::Int(0));
    assert_eq!(
        array_type["typsubscript"],
        Value::Str("array_subscript_handler".into())
    );
    assert_eq!(array_type["typelem"], Value::Int(13_315));
    assert_eq!(array_type["typarray"], Value::Int(0));
    assert_eq!(
        pg_type_routine_layout(array_type),
        [750, 751, 2400, 2401, 0, 0, 3816]
    );

    let composite_type = &types.rows[1];
    assert_eq!(
        composite_type["typname"],
        Value::Str("information_schema_catalog_name".into())
    );
    assert_eq!(composite_type["typtype"], Value::Str("c".into()));
    assert_eq!(composite_type["typcategory"], Value::Str("C".into()));
    assert_eq!(composite_type["typrelid"], Value::Int(13_313));
    assert_eq!(composite_type["typsubscript"], Value::Str("-".into()));
    assert_eq!(composite_type["typelem"], Value::Int(0));
    assert_eq!(composite_type["typarray"], Value::Int(13_314));
    assert_eq!(
        pg_type_routine_layout(composite_type),
        [2290, 2291, 2402, 2403, 0, 0, 0]
    );
}

fn pg_type_full_layout(row: &ResultRow) -> Vec<Value> {
    [
        "oid",
        "typname",
        "typnamespace",
        "typowner",
        "typlen",
        "typbyval",
        "typtype",
        "typcategory",
        "typispreferred",
        "typisdefined",
        "typdelim",
        "typrelid",
        "typsubscript",
        "typelem",
        "typarray",
        "typinput",
        "typoutput",
        "typreceive",
        "typsend",
        "typmodin",
        "typmodout",
        "typanalyze",
        "typalign",
        "typstorage",
        "typnotnull",
        "typbasetype",
        "typtypmod",
        "typndims",
        "typcollation",
    ]
    .into_iter()
    .map(|column| row[column].clone())
    .collect()
}

#[allow(clippy::too_many_arguments)]
fn pseudo_type_layout(
    oid: i64,
    name: &str,
    len: i64,
    by_value: bool,
    relation_oid: i64,
    subscript: &str,
    element_oid: i64,
    array_oid: i64,
    input: i64,
    output: i64,
    receive: i64,
    send: i64,
    analyze: i64,
    align: &str,
    storage: &str,
) -> Vec<Value> {
    vec![
        Value::Int(oid),
        Value::Str(name.into()),
        Value::Int(11),
        Value::Int(10),
        Value::Int(len),
        Value::Bool(by_value),
        Value::Str("p".into()),
        Value::Str("P".into()),
        Value::Bool(false),
        Value::Bool(true),
        Value::Str(",".into()),
        Value::Int(relation_oid),
        Value::Str(subscript.into()),
        Value::Int(element_oid),
        Value::Int(array_oid),
        Value::Int(input),
        Value::Int(output),
        Value::Int(receive),
        Value::Int(send),
        Value::Int(0),
        Value::Int(0),
        Value::Int(analyze),
        Value::Str(align.into()),
        Value::Str(storage.into()),
        Value::Bool(false),
        Value::Int(0),
        Value::Int(-1),
        Value::Int(0),
        Value::Int(0),
    ]
}

fn pg_type_routine_layout(row: &ResultRow) -> [i64; 7] {
    [
        "typinput",
        "typoutput",
        "typreceive",
        "typsend",
        "typmodin",
        "typmodout",
        "typanalyze",
    ]
    .map(|column| match row[column] {
        Value::Int(value) => value,
        ref value => panic!("expected integer routine OID for {column}, got {value:?}"),
    })
}

#[path = "sql_information_schema/pg18_routines.rs"]
mod pg18_routines;
