//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Initial foreign-key target binding must precede index-address conversion.

use std::path::Path;
use uqa_engine::Engine;
use uqa_execution::schema::indexes::constraint_names::KeyConstraintNames;
use uqa_sql::ast::ColumnDef;
use uqa_storage_sqlite::{Catalog, ManagedConnection};

fn fixture(path: &Path, target: &str, ambiguous: bool, legacy: bool) -> Catalog {
    let engine = crate::native_storage::legacy_engine(path);
    engine.sql("CREATE SCHEMA app; CREATE TABLE app.parent(id int PRIMARY KEY); CREATE TABLE app.child(id int PRIMARY KEY, a int REFERENCES app.parent(id), b int, FOREIGN KEY(b) REFERENCES app.parent(id))", &[]).unwrap();
    if ambiguous {
        engine
            .sql("CREATE TABLE public.parent(id int PRIMARY KEY)", &[])
            .unwrap();
    }
    drop(engine);
    let catalog = Catalog::open(ManagedConnection::open(path).unwrap()).unwrap();
    let names = KeyConstraintNames::load(&catalog).unwrap();
    for mut row in catalog.load_tables().unwrap() {
        let mut columns: Vec<ColumnDef> = serde_json::from_str(&row.columns_json).unwrap();
        let mut constraints = names.decode(&row).unwrap();
        for column in &mut columns {
            if let Some(reference) = &mut column.references {
                reference.table = target.into();
                if legacy {
                    reference.referenced_index = None;
                }
            }
        }
        for reference in &mut constraints.foreign_keys {
            reference.ref_table = target.into();
            if legacy {
                reference.referenced_index = None;
            }
        }
        row.columns_json = serde_json::to_string(&columns).unwrap();
        row.constraints_json = if legacy {
            serde_json::to_string(&constraints).unwrap()
        } else {
            uqa_execution::schema::indexes::constraint_names::encode(&constraints).unwrap()
        };
        catalog.save_table(&row).unwrap();
    }
    if legacy {
        catalog
            .delete_metadata("sql_index_registry_version")
            .unwrap();
    }
    catalog
}

#[test]
fn legacy_foreign_key_conversion_canonicalizes_once_and_preserves_the_bound_target() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy-foreign-key.db");
    drop(fixture(&path, "parent", false, true));
    let engine = Engine::open(&path).unwrap();
    engine.sql("SET search_path TO public", &[]).unwrap();
    let references = engine.foreign_keys("app.child").unwrap();
    assert_eq!(references.len(), 2);
    assert!(references
        .iter()
        .all(|key| key.ref_table == "app.parent" && key.referenced_index.is_some()));
    engine.sql("INSERT INTO app.parent VALUES(1); CREATE TABLE public.parent(id int PRIMARY KEY); INSERT INTO public.parent VALUES(2); INSERT INTO app.child VALUES(1,1,1)", &[]).unwrap();
    let error = engine
        .sql("INSERT INTO app.child VALUES(2,2,2)", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("23503"));
    drop(engine);
    let reopened = Engine::open(&path).unwrap();
    assert_eq!(reopened.foreign_keys("app.child").unwrap(), references);
    reopened
        .sql("INSERT INTO app.child VALUES(3,1,1)", &[])
        .unwrap();
}

#[test]
fn unresolved_legacy_foreign_keys_fail_before_conversion_publication() {
    for (target, ambiguous, expected) in [
        ("parent", true, "ambiguous persisted"),
        ("missing", false, "dangling persisted"),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("invalid-legacy-foreign-key.db");
        let catalog = fixture(&path, target, ambiguous, true);
        let before = catalog
            .load_tables()
            .unwrap()
            .into_iter()
            .map(|row| (row.relation, row.columns_json, row.constraints_json))
            .collect::<Vec<_>>();
        let error = Engine::open(&path).err().expect("invalid legacy target");
        assert!(error.to_string().contains(expected), "{error}");
        assert!(catalog
            .get_metadata("sql_index_registry_version")
            .unwrap()
            .is_none());
        assert_eq!(
            catalog
                .load_tables()
                .unwrap()
                .into_iter()
                .map(|row| (row.relation, row.columns_json, row.constraints_json))
                .collect::<Vec<_>>(),
            before
        );
    }
}

#[test]
fn current_foreign_key_target_corruption_is_never_repaired_as_legacy() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("invalid-current-foreign-key.db");
    let catalog = fixture(&path, "parent", false, false);
    let error = Engine::open(&path).err().expect("corrupted current target");
    assert!(
        error.to_string().contains("missing unique index"),
        "{error}"
    );
    assert_eq!(
        catalog
            .get_metadata("sql_index_registry_version")
            .unwrap()
            .as_deref(),
        Some("2")
    );
}
