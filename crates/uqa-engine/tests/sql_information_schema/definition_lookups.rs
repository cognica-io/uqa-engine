//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Engine, Value};
use std::sync::Arc;

#[test]
fn object_definitions_do_not_require_schema_usage() {
    let directory = tempfile::tempdir().unwrap();
    for provider in 0..4 {
        let path = directory.path().join(format!("definitions-{provider}.db"));
        let engine = match provider {
            0 => Engine::new(),
            1 => Engine::open(&path).unwrap(),
            2 => Engine::from_persistent_provider(Arc::new(
                uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
            ))
            .unwrap(),
            _ => Engine::from_persistent_provider(Arc::new(
                uqa_storage_redb::RedbStorage::open(&path).unwrap(),
            ))
            .unwrap(),
        };
        assert_definition_visibility(&engine);
    }
}

fn assert_definition_visibility(engine: &Engine) {
    engine.sql("CREATE ROLE definition_reader; CREATE SCHEMA hidden_catalog; CREATE TABLE hidden_catalog.items(id integer PRIMARY KEY CONSTRAINT hidden_check CHECK(id > 0)); CREATE TABLE public.visible_items(id integer CONSTRAINT visible_check CHECK(id > 0), hidden_id integer REFERENCES hidden_catalog.items(id)); CREATE FUNCTION public.trigger_body() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN RETURN NEW; END$$; CREATE TRIGGER watch BEFORE INSERT ON public.visible_items FOR EACH ROW EXECUTE FUNCTION public.trigger_body(); CREATE TRIGGER hidden_watch BEFORE INSERT ON hidden_catalog.items FOR EACH ROW EXECUTE FUNCTION public.trigger_body(); CREATE RULE visible_rule AS ON UPDATE TO public.visible_items DO ALSO NOTHING", &[]).unwrap();
    let inquiries = [
        (
            "pg_get_constraintdef",
            "SELECT oid FROM pg_constraint WHERE conname='visible_check'",
        ),
        (
            "pg_get_constraintdef",
            "SELECT oid FROM pg_constraint WHERE conname='hidden_check'",
        ),
        (
            "pg_get_constraintdef",
            "SELECT oid FROM pg_constraint WHERE conname='visible_items_hidden_id_fkey'",
        ),
        (
            "pg_get_triggerdef",
            "SELECT oid FROM pg_trigger WHERE tgname='watch'",
        ),
        (
            "pg_get_triggerdef",
            "SELECT oid FROM pg_trigger WHERE tgname='hidden_watch'",
        ),
        (
            "pg_get_ruledef",
            "SELECT oid FROM pg_rewrite WHERE rulename='visible_rule'",
        ),
        (
            "pg_get_functiondef",
            "SELECT 'public.trigger_body()'::regprocedure::oid",
        ),
    ]
    .map(|(function, sql)| {
        let result = engine.sql(sql, &[]).unwrap();
        let Value::Int(oid) = result.rows[0][&result.columns[0]] else {
            panic!("catalog OID");
        };
        format!("SELECT {function}({oid}) AS definition")
    });
    // Independently captured from PostgreSQL 18.4, including qualification and whitespace.
    let definitions = [
        "CHECK ((id > 0))",
        "CHECK ((id > 0))",
        "FOREIGN KEY (hidden_id) REFERENCES hidden_catalog.items(id)",
        "CREATE TRIGGER watch BEFORE INSERT ON public.visible_items FOR EACH ROW EXECUTE FUNCTION trigger_body()",
        "CREATE TRIGGER hidden_watch BEFORE INSERT ON hidden_catalog.items FOR EACH ROW EXECUTE FUNCTION trigger_body()",
        "CREATE RULE visible_rule AS\n    ON UPDATE TO public.visible_items DO NOTHING;",
        "CREATE OR REPLACE FUNCTION public.trigger_body()\n RETURNS trigger\n LANGUAGE plpgsql\nAS $function$BEGIN RETURN NEW; END$function$\n",
    ];
    for role in ["RESET ROLE", "SET ROLE definition_reader", "RESET ROLE"] {
        engine.sql(role, &[]).unwrap();
        for (sql, definition) in inquiries.iter().zip(definitions) {
            let result = engine
                .sql(sql, &[])
                .unwrap_or_else(|error| panic!("{role}: {sql}: {error}"));
            assert_eq!(result.rows[0]["definition"], Value::Str(definition.into()));
        }
        if role == "SET ROLE definition_reader" {
            for sql in [
                "SELECT * FROM hidden_catalog.items",
                "SELECT 'hidden_catalog.items'::regclass",
            ] {
                let error = engine.sql(sql, &[]).unwrap_err();
                assert_eq!(error.sqlstate(), Some("42501"), "{sql}: {error}");
            }
        }
    }
}
