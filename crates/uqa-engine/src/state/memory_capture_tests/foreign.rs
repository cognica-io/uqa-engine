//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{sql, Engine, StorageReadControl};
use uqa_execution::catalog::foreign::{registry::CapturedForeignTables, ForeignTables};
use uqa_sql::catalog::foreign_server::{CapturedForeignServers, ForeignServers};

#[test]
fn memory_foreign_images_retain_paired_authority_after_rollback_replacement_and_closure() {
    let engine = Engine::new();
    sql(&engine, "CREATE ROLE reader; CREATE SERVER remote FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE remote_rows(gone integer, payload text DEFAULT 'original', CONSTRAINT valid CHECK(length(payload)>0)) SERVER remote OPTIONS(z 'first',a 'second'); ALTER FOREIGN TABLE remote_rows DROP gone; GRANT SELECT(payload) ON remote_rows TO reader WITH GRANT OPTION");
    let original = engine.durable.foreign_catalog.snapshot();
    let key = engine.durable.foreign_catalog.key().unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let cut = engine
        .storage
        .memory_capture
        .as_ref()
        .unwrap()
        .capture(&control)
        .unwrap();
    sql(&engine, "BEGIN; REVOKE SELECT(payload) ON remote_rows FROM reader; DROP FOREIGN TABLE remote_rows; ROLLBACK; DROP FOREIGN TABLE remote_rows; CREATE FOREIGN TABLE remote_rows(replacement integer) SERVER remote");
    drop(engine);
    let saved = key.retained(&cut).unwrap().unwrap();
    let bytes = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let output_control = StorageReadControl::with_limit(64 << 10);
                let mut source = saved.get().unwrap().image_reader(&output_control).unwrap();
                let mut buffer = [0; 4096];
                let mut bytes = Vec::new();
                loop {
                    let count = source.read(&mut buffer).unwrap();
                    if count == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                }
                bytes
            })
            .join()
            .unwrap()
    });
    let restored_control = StorageReadControl::with_limit(1 << 20);
    let restored = uqa_execution::catalog::foreign::ForeignCatalog::read_image(
        &mut bytes.as_slice(),
        &restored_control,
    )
    .unwrap();
    assert_eq!(restored.security(), original.security());
    let token = uqa_core::CancellationToken::new();
    let capture = restored
        .capture(&uqa_core::memory::ProductionControl::new(
            restored_control.memory(),
            &token,
            &token,
        ))
        .unwrap();
    same_tables(&capture.tables, original.tables());
    drop((capture, restored, original, saved, cut));
    assert_eq!(restored_control.memory().used(), 0);
    assert_eq!(control.memory().used(), 0);
}

fn same_servers(saved: &CapturedForeignServers, source: &ForeignServers) {
    assert_eq!(saved.len(), source.len());
    for ((name, value), (original_name, original)) in saved.iter().zip(source) {
        assert_eq!(name, original_name);
        assert_eq!(value.name, original.name);
        assert_eq!(value.fdw_type, original.fdw_type);
        assert_eq!(value.metadata, original.metadata);
        assert!(value.options.iter().eq(original.options.iter()));
    }
}

fn same_tables(saved: &CapturedForeignTables, source: &ForeignTables) {
    assert_eq!(saved.len(), source.len());
    for ((name, value), (original_name, original)) in saved.iter().zip(source) {
        assert_eq!(name, original_name);
        assert_eq!(value.name, original.name);
        assert_eq!(value.persistence, original.persistence);
        assert_eq!(value.object_id, original.object_id);
        assert_eq!(value.catalog_oids, original.catalog_oids);
        assert_eq!(value.row_type_array_name, original.row_type_array_name);
        assert_eq!(value.server_name, original.server_name);
        assert_eq!(value.server_reference, original.server_reference);
        assert_eq!(value.dropped_attributes, original.dropped_attributes);
        assert_eq!(value.option_order, original.option_order);
        assert!(value.options.iter().eq(original.options.iter()));
        assert_eq!(
            serde_json::to_value(&value.columns).unwrap(),
            serde_json::to_value(&original.columns).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&value.checks).unwrap(),
            serde_json::to_value(&original.checks).unwrap()
        );
    }
}

#[test]
fn memory_foreign_capture_keeps_definitions_through_rollback_replacement_and_unread_closure() {
    for mutate in [false, true] {
        let engine = Engine::new();
        sql(&engine, "CREATE FOREIGN DATA WRAPPER custom HANDLER memory_fdw_handler OPTIONS (z 'first',a 'second'); CREATE SERVER remote TYPE 'kind' VERSION 'v1' FOREIGN DATA WRAPPER custom OPTIONS (z 'first',a 'second'); CREATE FOREIGN TABLE remote_rows(gone integer, payload text DEFAULT 'original', CONSTRAINT valid CHECK(length(payload)>0)) SERVER remote OPTIONS(z 'first',a 'second'); ALTER FOREIGN TABLE remote_rows DROP gone");
        let wrappers = engine.durable.foreign_wrappers.snapshot();
        let servers = engine.durable.foreign_servers.snapshot();
        let tables = engine.durable.foreign_catalog.snapshot().tables_snapshot();
        assert_eq!(tables.len(), 1);
        assert_eq!(tables.iter().next().unwrap().1.dropped_attributes.len(), 1);
        let wrapper_key = engine.durable.foreign_wrappers.key().unwrap();
        let server_key = engine.durable.foreign_servers.key().unwrap();
        let table_key = engine.durable.foreign_catalog.key().unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let cut = engine
            .storage
            .memory_capture
            .as_ref()
            .unwrap()
            .capture(&control)
            .unwrap();
        assert!(control.memory().used() < 4096);
        if mutate {
            sql(&engine, "BEGIN; DROP FOREIGN TABLE remote_rows; SAVEPOINT removed; DROP SERVER remote; ROLLBACK TO removed; ROLLBACK");
            sql(&engine, "DROP FOREIGN DATA WRAPPER custom CASCADE; CREATE FOREIGN DATA WRAPPER custom HANDLER memory_fdw_handler; CREATE SERVER remote FOREIGN DATA WRAPPER custom; CREATE FOREIGN TABLE remote_rows(replacement integer) SERVER remote");
            assert_ne!(
                engine.durable.foreign_wrappers.read()["custom"].identity,
                wrappers["custom"].identity
            );
        }
        drop(engine);
        let saved_wrappers = wrapper_key.retained(&cut).unwrap().unwrap();
        let saved_servers = server_key.retained(&cut).unwrap().unwrap();
        let saved_tables = table_key.retained(&cut).unwrap().unwrap();
        assert!(saved_wrappers.get().unwrap().iter().eq(wrappers.iter()));
        same_servers(saved_servers.get().unwrap(), &servers);
        same_tables(&saved_tables.get().unwrap().tables, &tables);
        drop(cut);
        control.cancellation().cancel();
        assert!(saved_wrappers.get().is_err());
        assert!(saved_servers.get().is_err());
        assert!(saved_tables.get().is_err());
        drop((saved_wrappers, saved_servers, saved_tables));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn denied_foreign_capture_keeps_ordinary_ddl_and_independent_cuts_usable() {
    let engine = Engine::new();
    sql(
        &engine,
        &format!(
            "CREATE FOREIGN DATA WRAPPER large OPTIONS (payload '{}')",
            "x".repeat(32768)
        ),
    );
    let key = engine.durable.foreign_wrappers.key().unwrap();
    let small = StorageReadControl::with_limit(4096);
    let good = StorageReadControl::with_limit(1 << 20);
    let registry = engine.storage.memory_capture.as_ref().unwrap();
    let denied = registry.capture(&small).unwrap();
    let cut = registry.capture(&good).unwrap();
    sql(
        &engine,
        "DROP FOREIGN DATA WRAPPER large; CREATE FOREIGN DATA WRAPPER large",
    );
    assert!(engine.durable.foreign_wrappers.read()["large"]
        .options
        .is_empty());
    assert!(denied.check().is_err());
    let saved = key.retained(&cut).unwrap().unwrap();
    let original = saved.get().unwrap().get("large").unwrap();
    assert_eq!(original.options[0].1.len(), 32768);
    drop((saved, cut, denied, engine));
    assert_eq!(small.memory().used(), 0);
    assert_eq!(good.memory().used(), 0);
}

#[test]
fn wrapper_images_keep_self_removed_callback_identity_after_name_reuse_and_engine_closure() {
    use uqa_core::{memory::MemoryBudget, CancellationToken};
    use uqa_sql::catalog::foreign_wrapper::{validate_wrappers, ForeignWrappers};

    for mutate in [false, true] {
        let engine = Engine::new();
        sql(&engine, "CREATE FUNCTION orphan_validator(text[],oid) RETURNS integer LANGUAGE plpgsql AS $$ BEGIN EXECUTE 'DROP FUNCTION orphan_validator(text[],oid)'; RETURN 1; END $$; CREATE FOREIGN DATA WRAPPER orphan_fdw VALIDATOR orphan_validator OPTIONS (z 'original', a '')");
        let roles = engine.durable.roles.snapshot();
        let wrappers = engine.durable.foreign_wrappers.snapshot();
        let original = wrappers["orphan_fdw"].validator.as_ref().unwrap();
        let key = engine.durable.foreign_wrappers.key().unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let cut = engine
            .storage
            .memory_capture
            .as_ref()
            .unwrap()
            .capture(&control)
            .unwrap();
        sql(
            &engine,
            "CREATE FUNCTION orphan_validator(text[],oid) RETURNS integer LANGUAGE SQL RETURN 7",
        );
        let error = engine
            .sql(
                "CREATE SERVER orphan_server FOREIGN DATA WRAPPER orphan_fdw",
                &[],
            )
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("XX000"));
        assert_eq!(
            error.to_string(),
            format!("cache lookup failed for function {}", original.oid)
        );
        if mutate {
            sql(&engine, "BEGIN; DROP FOREIGN DATA WRAPPER orphan_fdw; SAVEPOINT removed; CREATE FOREIGN DATA WRAPPER private_fdw; ROLLBACK TO removed; ROLLBACK");
            assert_eq!(
                engine.durable.foreign_wrappers.read()["orphan_fdw"].validator,
                wrappers["orphan_fdw"].validator
            );
            sql(&engine, "DROP FOREIGN DATA WRAPPER orphan_fdw; CREATE FOREIGN DATA WRAPPER orphan_fdw VALIDATOR orphan_validator");
            assert_ne!(
                engine.durable.foreign_wrappers.read()["orphan_fdw"].identity,
                wrappers["orphan_fdw"].identity
            );
        }
        drop(engine);
        let saved = key.retained(&cut).unwrap().unwrap();
        drop(cut);
        let image = std::thread::spawn(move || {
            let mut image = Vec::new();
            saved
                .get()
                .unwrap()
                .write_image(&mut image, &CancellationToken::new())
                .unwrap();
            image
        })
        .join()
        .unwrap();
        assert_eq!(control.memory().used(), 0);
        let memory = MemoryBudget::new(1 << 20);
        let restored =
            ForeignWrappers::read_image(&mut image.as_slice(), &memory, &CancellationToken::new())
                .unwrap();
        assert_eq!(
            serde_json::to_value(&restored).unwrap(),
            serde_json::to_value(&*wrappers).unwrap()
        );
        assert_eq!(restored["orphan_fdw"].validator.as_ref(), Some(original));
        validate_wrappers(&restored, &roles).unwrap();
        drop(restored);
        assert_eq!(memory.used(), 0);
    }
}
