//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

mod ownership;

const NAMES: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/parity/pg18/index_namespace_oracle.expected.txt"
));
const OWNERS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/parity/pg18/index_ownership_oracle.expected.txt"
));

fn reference<'a>(source: &'a str, label: &str) -> &'a str {
    source
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{label}|")))
        .unwrap_or_else(|| panic!("missing PostgreSQL reference {label}"))
}

fn command(engine: &Engine, source: &str, label: &str, statement: &str) {
    let expected = reference(source, label);
    let (state, message) = expected
        .split_once('|')
        .map_or((expected, None), |(state, message)| (state, Some(message)));
    match engine.sql(statement, &[]) {
        Ok(_) => assert_eq!(state, "ok", "{label}: {statement}"),
        Err(error) => {
            assert_eq!(
                error.sqlstate(),
                Some(state),
                "{label}: {statement}: {error}"
            );
            if let Some(message) = message {
                assert_eq!(error.to_string(), message, "{label}: {statement}");
            }
        }
    }
}

fn scalar(engine: &Engine, statement: &str) -> Value {
    let result = sql(engine, statement);
    result.rows[0][&result.columns[0]].clone()
}

fn value(engine: &Engine, source: &str, label: &str, statement: &str) {
    assert_eq!(
        scalar(engine, statement),
        Value::Str(reference(source, label).into()),
        "{label}"
    );
}

#[test]
fn diskann_shared_index_names_match_postgresql_on_every_provider() {
    super::definitions::owners(|engine| {
        sql(engine, "CREATE SCHEMA uqa_index_alpha; CREATE SCHEMA uqa_index_beta; CREATE SCHEMA uqa_index_hidden; CREATE SCHEMA \"uqa_index.dot\"; CREATE ROLE uqa_index_caller; REVOKE ALL ON SCHEMA uqa_index_hidden FROM PUBLIC");
        for schema in ["uqa_index_alpha", "uqa_index_beta"] {
            sql(engine, &format!("CREATE TABLE {schema}.items(embedding vector(2)); INSERT INTO {schema}.items VALUES(ARRAY[1,0]); CREATE INDEX shared_idx ON {schema}.items USING diskann(embedding)"));
        }
        value(engine, NAMES, "same-local", "SELECT string_agg(schemaname||'.'||indexname,',' ORDER BY schemaname) FROM pg_indexes WHERE indexname='shared_idx'");
        assert_eq!(scalar(engine, "SELECT 'uqa_index_alpha.shared_idx'::regclass::oid <> 'uqa_index_beta.shared_idx'::regclass::oid"), Value::Bool(true));
        value(engine, NAMES, "regclass-text", "SELECT 'uqa_index_alpha.shared_idx'::regclass::text||','||'uqa_index_beta.shared_idx'::regclass::text");
        sql(engine, "CREATE TABLE uqa_index_alpha.occupied_name(id int)");
        for (label, statement) in [
            ("duplicate-index", "CREATE INDEX shared_idx ON uqa_index_alpha.items USING diskann(embedding)"),
            ("table-name-collision", "CREATE INDEX occupied_name ON uqa_index_alpha.items USING diskann(embedding)"),
            ("index-name-collision", "CREATE TABLE uqa_index_alpha.shared_idx(id int)"),
            ("duplicate-index-if-not-exists", "CREATE INDEX IF NOT EXISTS shared_idx ON uqa_index_alpha.items USING diskann(embedding)"),
            ("table-name-if-not-exists", "CREATE INDEX IF NOT EXISTS occupied_name ON uqa_index_alpha.items USING diskann(embedding)"),
        ] {
            command(engine, NAMES, label, statement);
        }
        sql(
            engine,
            "SET search_path=uqa_index_beta,uqa_index_alpha,pg_catalog",
        );
        command(engine, NAMES, "drop-search-path", "DROP INDEX shared_idx");
        sql(engine, "RESET search_path");
        value(engine, NAMES, "drop-survivor", "SELECT string_agg(schemaname||'.'||indexname,',' ORDER BY schemaname) FROM pg_indexes WHERE indexname='shared_idx'");
        sql(engine, "CREATE TABLE uqa_index_alpha.shadow_idx(id int); CREATE INDEX shadow_idx ON uqa_index_beta.items USING diskann(embedding); SET search_path=uqa_index_alpha,uqa_index_beta,pg_catalog");
        command(
            engine,
            NAMES,
            "drop-wrong-kind-first",
            "DROP INDEX shadow_idx",
        );
        sql(engine, "RESET search_path");
        for (label, statement) in [
            (
                "drop-missing-index",
                "DROP INDEX uqa_index_alpha.absent_idx",
            ),
            (
                "drop-missing-index-if-exists",
                "DROP INDEX IF EXISTS uqa_index_alpha.absent_idx",
            ),
            (
                "drop-missing-schema",
                "DROP INDEX uqa_index_absent.absent_idx",
            ),
            (
                "drop-missing-schema-if-exists",
                "DROP INDEX IF EXISTS uqa_index_absent.absent_idx",
            ),
        ] {
            command(engine, NAMES, label, statement);
        }
        sql(engine, "CREATE TABLE uqa_index_hidden.items(embedding vector(2)); CREATE INDEX visible_drop_idx ON uqa_index_hidden.items USING diskann(embedding); SET ROLE uqa_index_caller");
        command(
            engine,
            NAMES,
            "drop-hidden-qualified",
            "DROP INDEX uqa_index_hidden.visible_drop_idx",
        );
        command(
            engine,
            NAMES,
            "drop-hidden-if-exists",
            "DROP INDEX IF EXISTS uqa_index_hidden.absent_idx",
        );
        sql(engine, "RESET ROLE; GRANT USAGE, CREATE ON SCHEMA uqa_index_alpha TO uqa_index_caller; SET ROLE uqa_index_caller; CREATE TABLE uqa_index_alpha.caller_items(embedding vector(2)); CREATE INDEX visible_drop_idx ON uqa_index_alpha.caller_items USING diskann(embedding); SET search_path=uqa_index_hidden,uqa_index_alpha,pg_catalog");
        command(
            engine,
            NAMES,
            "drop-skips-hidden-schema",
            "DROP INDEX visible_drop_idx",
        );
        sql(engine, "RESET ROLE; RESET search_path");
        value(engine, NAMES, "hidden-survivor", "SELECT string_agg(schemaname||'.'||indexname,',' ORDER BY schemaname) FROM pg_indexes WHERE indexname='visible_drop_idx'");
        sql(engine, "CREATE TABLE \"uqa_index.dot\".items(embedding vector(2)); CREATE INDEX \"shared.dot\" ON \"uqa_index.dot\".items USING diskann(embedding)");
        value(
            engine,
            NAMES,
            "quoted-identity",
            "SELECT '\"uqa_index.dot\".\"shared.dot\"'::regclass::text",
        );
    });
}
