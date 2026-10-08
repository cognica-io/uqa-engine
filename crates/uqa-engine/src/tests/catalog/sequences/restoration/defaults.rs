//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Initial-open conversion of legacy creation-bound sequence default inputs.

use crate::{
    tests::relation_lock_support::{sessions, sql},
    Engine,
};
use std::sync::Arc;
use uqa_core::Value;
use uqa_sql::ast::{ColumnDef, Expr};
use uqa_storage::{CatalogFacade, TableSchema};

fn stored_table(catalog: &dyn CatalogFacade) -> TableSchema {
    catalog
        .load_tables()
        .unwrap()
        .into_iter()
        .find(|row| row.relation.qualified_name() == "public.legacy_defaults")
        .unwrap()
}

fn columns(row: &TableSchema) -> Vec<ColumnDef> {
    serde_json::from_str(&row.columns_json).unwrap()
}

fn legacy_default(catalog: &dyn CatalogFacade) -> TableSchema {
    let mut row = stored_table(catalog);
    let mut definitions = columns(&row);
    let Expr::Func { args, binding, .. } = definitions[0].default.as_mut().unwrap() else {
        panic!("expected a selected sequence default")
    };
    assert!(binding.as_ref().unwrap().builtin);
    args[0] = Expr::Literal(Value::Str("app.ids".into()));
    if let Some(value) = args.get_mut(1) {
        *value = Expr::Literal(Value::Int(40));
    }
    binding.as_mut().unwrap().argument_types[0] = "text".into();
    row.columns_json = serde_json::to_string(&definitions).unwrap();
    catalog.save_table(&row).unwrap();
    row
}

fn assert_frozen(row: &TableSchema, oid: &Value) {
    let definitions = columns(row);
    let Expr::Func { args, binding, .. } = definitions[0].default.as_ref().unwrap() else {
        panic!("expected a selected sequence default")
    };
    assert_eq!(
        args[0],
        Expr::TypedLiteral {
            composite_source: None,
            value: oid.clone(),
            ty: "regclass".into()
        }
    );
    assert_eq!(binding.as_ref().unwrap().argument_types[0], "regclass");
}

#[test]
fn legacy_sequence_defaults_migrate_only_on_initial_open_and_keep_oid_after_rename() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(
            &first,
            "CREATE SCHEMA app; CREATE SEQUENCE app.ids; CREATE SEQUENCE late_ids; CREATE FUNCTION app.nextval(text) RETURNS bigint LANGUAGE SQL AS 'SELECT 31337::bigint'; CREATE TABLE legacy_defaults(id bigint DEFAULT pg_catalog.nextval('app.ids'), late bigint DEFAULT nextval('late_ids'::text), chosen bigint DEFAULT app.nextval('anything'))",
        );
        let oid = sql(&first, "SELECT 'app.ids'::regclass::oid AS v").rows[0]["v"].clone();
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        let legacy = legacy_default(raw.catalog.as_ref());
        let preserved = serde_json::to_value(&columns(&legacy)[1..]).unwrap();
        let Err(failure) = first.new_session() else {
            panic!("secondary restore migrated a legacy sequence default")
        };
        assert!(failure.to_string().contains("initial-open"), "{failure}");
        assert_eq!(
            stored_table(raw.catalog.as_ref()).columns_json,
            legacy.columns_json
        );
        drop((first, second));
        let reopened = Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
        let migrated = stored_table(raw.catalog.as_ref());
        assert_frozen(&migrated, &oid);
        assert_eq!(
            serde_json::to_value(&columns(&migrated)[1..]).unwrap(),
            preserved
        );
        sql(
            &reopened,
            "ALTER SEQUENCE app.ids RENAME TO retained_ids; CREATE SEQUENCE app.ids START 500; SET search_path TO public",
        );
        let result = sql(
            &reopened,
            "INSERT INTO legacy_defaults DEFAULT VALUES RETURNING id,late,chosen",
        );
        assert_eq!(result.rows[0]["id"], Value::Int(1));
        assert_eq!(result.rows[0]["late"], Value::Int(1));
        assert_eq!(result.rows[0]["chosen"], Value::Int(31337));
        let peer = reopened.new_session().unwrap();
        assert_eq!(
            sql(
                &peer,
                "INSERT INTO legacy_defaults DEFAULT VALUES RETURNING id"
            )
            .rows[0]["id"],
            Value::Int(2)
        );
        drop((reopened, peer));
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        assert_frozen(&stored_table(raw.catalog.as_ref()), &oid);
        assert_eq!(
            sql(
                &reopened,
                "INSERT INTO legacy_defaults DEFAULT VALUES RETURNING id"
            )
            .rows[0]["id"],
            Value::Int(3)
        );
    }
}

#[test]
fn failed_initial_restore_does_not_publish_legacy_sequence_default_conversion() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(
            &first,
            "CREATE SCHEMA app; CREATE SEQUENCE app.ids; CREATE TABLE legacy_defaults(id bigint DEFAULT setval('app.ids',40))",
        );
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        let legacy = legacy_default(raw.catalog.as_ref());
        let triggers = raw.catalog.get_metadata("sql_triggers_json").unwrap();
        raw.catalog.set_metadata("sql_triggers_json", "{").unwrap();
        drop((first, second));
        let Err(failure) = Engine::from_persistent_provider(Arc::clone(&factory)) else {
            panic!("malformed later metadata must fail initial restoration")
        };
        assert!(failure.to_string().contains("EOF"), "{failure}");
        assert_eq!(
            stored_table(raw.catalog.as_ref()).columns_json,
            legacy.columns_json
        );
        if let Some(triggers) = triggers {
            raw.catalog
                .set_metadata("sql_triggers_json", &triggers)
                .unwrap();
        } else {
            raw.catalog.delete_metadata("sql_triggers_json").unwrap();
        }
        let restored = Engine::from_persistent_provider(factory).unwrap();
        let oid = sql(&restored, "SELECT 'app.ids'::regclass::oid AS v").rows[0]["v"].clone();
        assert_frozen(&stored_table(raw.catalog.as_ref()), &oid);
        let result = sql(
            &restored,
            "SELECT pg_get_expr(adbin,adrelid) AS value FROM pg_attrdef WHERE adrelid='legacy_defaults'::regclass",
        );
        assert_eq!(
            result.rows[0]["value"],
            Value::Str("setval('app.ids'::regclass, (40)::bigint)".into())
        );
    }
}
