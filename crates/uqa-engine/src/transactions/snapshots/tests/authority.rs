//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{capture::capture, *};
use crate::state::{DurableCatalogSnapshot, SnapshotCut};

fn fixture(engine: &Engine) {
    sql(engine, "CREATE TABLE docs(id integer); INSERT INTO docs VALUES(1); CREATE ROLE team; CREATE ROLE reader LOGIN CONNECTION LIMIT 7; GRANT team TO reader WITH ADMIN OPTION; GRANT CREATE ON DATABASE uqa TO reader; GRANT UPDATE ON pg_class TO reader; GRANT SELECT(relname) ON pg_class TO reader; REVOKE EXECUTE ON FUNCTION lower(text) FROM PUBLIC; GRANT EXECUTE ON FUNCTION lower(text) TO reader; CREATE FOREIGN DATA WRAPPER custom HANDLER memory_fdw_handler OPTIONS (z 'first',a 'second'); CREATE SERVER remote FOREIGN DATA WRAPPER custom OPTIONS (z 'first',a 'second'); CREATE FOREIGN TABLE remote_rows(gone integer, payload text DEFAULT 'original') SERVER remote; GRANT SELECT(payload) ON remote_rows TO reader WITH GRANT OPTION; ALTER FOREIGN TABLE remote_rows DROP gone");
}

fn same_authority(selected: &SnapshotCut, original: &DurableCatalogSnapshot) {
    let roles = selected.catalogs.roles.captured(&selected.cut).unwrap();
    let roles = roles.get().unwrap();
    assert_eq!(roles.len(), original.roles.len());
    for ((name, role), (old_name, old)) in roles.iter().zip(original.roles.iter()) {
        assert_eq!(name, old_name);
        assert_eq!(role.object_id, old.object_id);
        assert_eq!(role.oid, old.oid);
        assert_eq!(role.revision, old.revision);
        assert_eq!(role.name, old.name);
        assert_eq!(role.connection_limit, old.connection_limit);
        assert!(role.attributes.iter().eq(old.attributes.iter()));
    }
    macro_rules! same_map {
        ($($field:ident),+) => {$ ({
            let saved = selected.catalogs.$field.captured(&selected.cut).unwrap();
            assert!(saved.get().unwrap().iter().eq(original.$field.iter()));
        })+};
    }
    same_map!(role_memberships, builtin_routine_security, foreign_wrappers);
    let database = selected
        .catalogs
        .database_security
        .captured(&selected.cut)
        .unwrap();
    assert_eq!(
        database.get().unwrap().as_ref(),
        original.database_security.as_ref().as_ref()
    );
    let system = selected
        .catalogs
        .system_relation_security
        .captured(&selected.cut)
        .unwrap();
    assert_eq!(
        system.get().unwrap().len(),
        original.system_relation_security.len()
    );
    for ((name, value), (old_name, old)) in system
        .get()
        .unwrap()
        .iter()
        .zip(original.system_relation_security.iter())
    {
        assert_eq!(name, old_name);
        assert_eq!(value.table, old.table);
        assert!(value.columns.iter().eq(old.columns.iter()));
    }
    let servers = selected
        .catalogs
        .foreign_servers
        .captured(&selected.cut)
        .unwrap();
    assert_eq!(servers.get().unwrap().len(), original.foreign_servers.len());
    for ((name, value), (old_name, old)) in servers
        .get()
        .unwrap()
        .iter()
        .zip(original.foreign_servers.iter())
    {
        assert_eq!(name, old_name);
        assert_eq!(value.name, old.name);
        assert_eq!(value.fdw_type, old.fdw_type);
        assert_eq!(value.metadata, old.metadata);
        assert!(value.options.iter().eq(old.options.iter()));
    }
}

fn same_foreign(selected: &SnapshotCut, original: &DurableCatalogSnapshot) {
    let saved = selected.foreign.captured(&selected.cut).unwrap();
    let saved = saved.get().unwrap();
    assert_eq!(saved.tables.len(), 1);
    assert_eq!(saved.security.len(), 1);
    let (relation, definition) = saved.tables.iter().next().unwrap();
    let original_definition = &original.foreign_catalog.tables()[relation];
    assert_eq!(definition.object_id, original_definition.object_id);
    assert_eq!(definition.catalog_oids, original_definition.catalog_oids);
    assert_eq!(
        definition.server_reference,
        original_definition.server_reference
    );
    assert_eq!(
        definition.dropped_attributes,
        original_definition.dropped_attributes
    );
    assert_eq!(
        serde_json::to_value(&definition.columns).unwrap(),
        serde_json::to_value(&original_definition.columns).unwrap()
    );
    assert_eq!(
        saved.security.get(relation).unwrap().security(),
        &original.foreign_catalog.security()[relation]
    );
}

#[rstest::rstest]
#[case("COMMIT")]
#[case("ROLLBACK")]
fn transaction_authority_sources_preserve_matching_foreign_and_role_generations_after_retirement(
    #[case] completion: &str,
) {
    let engine = Engine::new();
    fixture(&engine);
    let original = engine.durable.snapshot();
    sql(&engine, "BEGIN; UPDATE docs SET id=2; ALTER ROLE reader CONNECTION LIMIT 9; REVOKE team FROM reader; REVOKE CREATE ON DATABASE uqa FROM reader; REVOKE UPDATE ON pg_class FROM reader; REVOKE EXECUTE ON FUNCTION lower(text) FROM reader; REVOKE SELECT(payload) ON remote_rows FROM reader; DROP SERVER remote CASCADE; CREATE SERVER remote FOREIGN DATA WRAPPER custom OPTIONS (z 'private'); CREATE FOREIGN TABLE remote_rows(replacement integer) SERVER remote; SAVEPOINT nested; DROP FOREIGN TABLE remote_rows; ROLLBACK TO nested");
    let control = StorageReadControl::with_limit(128 << 10);
    let selected = capture(&engine, &control);
    assert!(selected.foreign.retained(&selected.cut).unwrap().is_none());
    assert!(selected
        .catalogs
        .roles
        .retained(&selected.cut)
        .unwrap()
        .is_none());
    sql(&engine, completion);
    sql(&engine, "DROP FOREIGN DATA WRAPPER custom CASCADE; CREATE FOREIGN DATA WRAPPER custom HANDLER memory_fdw_handler; CREATE SERVER remote FOREIGN DATA WRAPPER custom; CREATE FOREIGN TABLE remote_rows(replacement boolean) SERVER remote; DROP TABLE docs; CREATE TABLE docs(replacement boolean)");
    drop(engine);
    same_authority(&selected, &original);
    same_foreign(&selected, &original);
    assert_eq!(
        selected
            .table(&RelationIdentity::new("public", "docs"))
            .unwrap()
            .document_store
            .get_field(1, "id")
            .unwrap(),
        Some(Value::Int(1))
    );
    let foreign = selected.foreign.captured(&selected.cut).unwrap();
    assert!(foreign.budget().shares_allowance(control.memory()));
    drop((selected, original));
    control.cancellation().cancel();
    assert!(foreign.get().is_err());
    drop(foreign);
    assert_eq!(control.memory().used(), 0);
}
