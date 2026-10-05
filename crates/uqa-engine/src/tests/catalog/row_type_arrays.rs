//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Initial catalog restoration records legacy relation arrays once without replacing identities.

use crate::tests::relation_lock_support::{sessions, sql};
use crate::Engine;
use serde_json::Value as Json;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use uqa_core::{RelationIdentity, Value};
use uqa_sql::catalog::relation_oids::{RelationCatalogOids, RelationOidKind};
use uqa_storage::CatalogFacade;

const SCHEMA: &str = "legacy_rows";

#[derive(Debug, PartialEq, Eq)]
struct StoredRelation {
    kind: RelationOidKind,
    object_id: [u8; 16],
    definition: Json,
}

impl StoredRelation {
    fn oids(&self) -> RelationCatalogOids {
        serde_json::from_value(self.definition["catalog_oids"].clone()).unwrap()
    }
}

fn definitions(catalog: &dyn CatalogFacade) -> BTreeMap<RelationIdentity, StoredRelation> {
    let mut definitions = BTreeMap::new();
    for row in catalog.load_tables().unwrap() {
        if row.relation.schema == SCHEMA {
            definitions.insert(
                row.relation,
                StoredRelation {
                    kind: RelationOidKind::Table,
                    object_id: row.object_id,
                    definition: serde_json::from_str(&row.constraints_json).unwrap(),
                },
            );
        }
    }
    for row in catalog.load_views().unwrap() {
        if row.relation.schema == SCHEMA {
            let definition: Json = serde_json::from_str(&row.definition_json).unwrap();
            definitions.insert(
                row.relation,
                StoredRelation {
                    kind: RelationOidKind::View,
                    object_id: serde_json::from_value(definition["object_id"].clone()).unwrap(),
                    definition,
                },
            );
        }
    }
    for row in catalog.load_foreign_tables().unwrap() {
        if row.relation.schema == SCHEMA {
            let definition: Json = serde_json::from_str(&row.columns_json).unwrap();
            definitions.insert(
                row.relation,
                StoredRelation {
                    kind: RelationOidKind::ForeignTable,
                    object_id: serde_json::from_value(definition["object_id"].clone()).unwrap(),
                    definition,
                },
            );
        }
    }
    definitions
}

fn strip_metadata(json: &mut String, remove_oids: bool) {
    let mut definition: Json = serde_json::from_str(json).unwrap();
    let fields = definition.as_object_mut().unwrap();
    assert!(fields.remove("row_type_array_name").is_some());
    if remove_oids {
        assert!(fields.remove("catalog_oids").is_some());
    }
    *json = serde_json::to_string(&definition).unwrap();
}

fn make_legacy(catalog: &dyn CatalogFacade, remove_oids: bool) {
    for mut row in catalog.load_tables().unwrap() {
        if row.relation.schema == SCHEMA {
            strip_metadata(&mut row.constraints_json, remove_oids);
            catalog.save_table(&row).unwrap();
        }
    }
    for mut row in catalog.load_views().unwrap() {
        if row.relation.schema == SCHEMA {
            strip_metadata(&mut row.definition_json, remove_oids);
            catalog.save_view(&row).unwrap();
        }
    }
    for mut row in catalog.load_foreign_tables().unwrap() {
        if row.relation.schema == SCHEMA {
            strip_metadata(&mut row.columns_json, remove_oids);
            catalog.save_foreign_table(&row).unwrap();
        }
    }
}

fn fixture(engine: &Engine) {
    sql(engine, "CREATE SCHEMA legacy_rows; CREATE TABLE legacy_rows.item(id integer); INSERT INTO legacy_rows.item VALUES(7),(9); CREATE TABLE legacy_rows._item(id integer); CREATE TABLE legacy_rows.partitioned(id integer) PARTITION BY RANGE(id); CREATE TABLE legacy_rows.partition_child PARTITION OF legacy_rows.partitioned FOR VALUES FROM(0) TO(10); INSERT INTO legacy_rows.partitioned VALUES(3); CREATE TYPE legacy_rows._view AS ENUM('occupied'); CREATE VIEW legacy_rows.view AS SELECT id FROM legacy_rows.item; CREATE MATERIALIZED VIEW legacy_rows.materialized AS SELECT id FROM legacy_rows.item; CREATE DOMAIN legacy_rows._foreign_table AS integer; CREATE SERVER legacy_server FOREIGN DATA WRAPPER memory_fdw OPTIONS(kind 'memory'); CREATE FOREIGN TABLE legacy_rows.foreign_table(id integer) SERVER legacy_server OPTIONS(source 'memory')");
}

fn verify_projection(engine: &Engine, relation: &RelationIdentity, stored: &StoredRelation) {
    let oids = stored.oids();
    let name = relation.qualified_name();
    let rows = sql(engine, &format!("SELECT c.oid AS relation_oid, c.reltype AS row_oid, t.typarray AS array_oid, a.typname::text AS array_name FROM pg_class c JOIN pg_type t ON t.oid=c.reltype JOIN pg_type a ON a.oid=t.typarray WHERE c.oid='{name}'::regclass")).rows;
    assert_eq!(rows.len(), 1, "{name}");
    let row = &rows[0];
    assert_eq!(row["relation_oid"], Value::Int(i64::from(oids.relation)));
    assert_eq!(
        row["row_oid"],
        Value::Int(i64::from(oids.row_type.unwrap()))
    );
    assert_eq!(
        row["array_oid"],
        Value::Int(i64::from(oids.array_type.unwrap()))
    );
    assert_eq!(
        row["array_name"],
        Value::Str(
            stored.definition["row_type_array_name"]
                .as_str()
                .unwrap()
                .into()
        )
    );
    let resolved = sql(
        engine,
        &format!("SELECT '{name}[]'::regtype::oid AS array_oid"),
    );
    assert_eq!(resolved.rows[0]["array_oid"], row["array_oid"]);
}

fn verify_completed(
    engine: &Engine,
    before: &BTreeMap<RelationIdentity, StoredRelation>,
    completed: &BTreeMap<RelationIdentity, StoredRelation>,
    remove_oids: bool,
) {
    assert_eq!(completed.len(), before.len());
    let mut names = BTreeSet::new();
    let scalar_names: BTreeSet<_> = completed
        .keys()
        .map(|relation| relation.name.as_str())
        .collect();
    for (relation, original) in before {
        let current = &completed[relation];
        assert_eq!(current.object_id, original.object_id, "{relation:?}");
        let expected = if remove_oids {
            RelationCatalogOids::legacy(original.kind, &original.object_id)
        } else {
            original.oids()
        };
        let oids = current.oids();
        assert_eq!(oids.relation, expected.relation);
        assert_eq!(oids.row_type, expected.row_type);
        assert_eq!(oids.rule, expected.rule);
        assert!(oids.is_valid_for(original.kind));
        if !remove_oids {
            assert_eq!(oids.array_type, expected.array_type);
        }
        let name = current.definition["row_type_array_name"].as_str().unwrap();
        assert!(names.insert(name), "duplicate array {name}");
        assert!(!scalar_names.contains(name), "row type {name}");
        verify_projection(engine, relation, current);
    }
    assert_eq!(
        completed[&RelationIdentity::new(SCHEMA, "item")].definition["row_type_array_name"],
        "_item_1"
    );
    assert_eq!(
        completed[&RelationIdentity::new(SCHEMA, "view")].definition["row_type_array_name"],
        "_view_1"
    );
    assert_eq!(
        completed[&RelationIdentity::new(SCHEMA, "foreign_table")].definition
            ["row_type_array_name"],
        "_foreign_table_1"
    );
}

#[rstest::rstest]
#[case::sqlite(0)]
#[case::sqlite_key_value(1)]
#[case::redb(2)]
fn legacy_row_arrays_are_completed_once_and_preserve_relation_identities(
    #[case] provider: usize,
    #[values(false, true)] remove_oids: bool,
) {
    let (_directory, first, second) = sessions(provider);
    fixture(&first);
    let before_rows = sql(&first, "SELECT id FROM legacy_rows.item ORDER BY id").rows;
    let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
    let raw = factory.open_session().unwrap();
    let before = definitions(raw.catalog.as_ref());
    assert_eq!(before.len(), 7);
    drop(second);
    drop(first);
    raw.backend.begin_transaction().unwrap();
    make_legacy(raw.catalog.as_ref(), remove_oids);
    raw.backend.commit_transaction().unwrap();
    let restored = Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
    let completed = definitions(raw.catalog.as_ref());
    verify_completed(&restored, &before, &completed, remove_oids);
    assert_eq!(
        sql(
            &restored,
            "SELECT id FROM legacy_rows.materialized ORDER BY id"
        )
        .rows,
        before_rows
    );
    assert_eq!(
        sql(&restored, "SELECT id FROM legacy_rows.view ORDER BY id").rows,
        before_rows
    );
    drop(restored);
    let reopened = Engine::from_persistent_provider(factory).unwrap();
    assert_eq!(definitions(raw.catalog.as_ref()), completed);
    for (relation, current) in &completed {
        verify_projection(&reopened, relation, current);
    }
    assert_eq!(
        sql(&reopened, "SELECT id FROM legacy_rows.item ORDER BY id").rows,
        before_rows
    );
    assert_eq!(
        sql(&reopened, "SELECT id FROM legacy_rows.partitioned").rows[0]["id"],
        Value::Int(3)
    );
}
