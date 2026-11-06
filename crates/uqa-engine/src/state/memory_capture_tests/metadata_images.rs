//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{sql, Engine, StorageReadControl};
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_sql::catalog::{
    enum_type::{validate_enum_registry, StoredEnum},
    foreign_server::{validate_foreign_servers, ForeignServers},
    graph_oids::GraphOidRegistry,
    shell_type::{validate_shell_registry, StoredShellType},
    type_definitions::TypeDefinitions,
};

#[test]
fn captured_type_images_preserve_original_identities_labels_and_authority_after_engine_closure() {
    for mutate in [false, true] {
        let engine = Engine::new();
        sql(&engine, "CREATE ROLE reader; CREATE TYPE saved_enum AS ENUM('first','last'); ALTER TYPE saved_enum ADD VALUE 'middle' BEFORE 'last'; GRANT USAGE ON TYPE saved_enum TO reader WITH GRANT OPTION; CREATE TYPE saved_shell");
        let roles = engine.durable.roles.snapshot();
        let enums = engine.durable.enums.snapshot();
        let shells = engine.durable.shells.snapshot();
        let enum_key = engine.durable.enums.key().unwrap();
        let shell_key = engine.durable.shells.key().unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let cut = engine
            .storage
            .memory_capture
            .as_ref()
            .unwrap()
            .capture(&control)
            .unwrap();
        if mutate {
            sql(&engine, "BEGIN; ALTER TYPE saved_enum RENAME VALUE 'middle' TO 'private'; SAVEPOINT changed; DROP TYPE saved_shell; ROLLBACK TO changed; ROLLBACK");
            assert_eq!(
                engine.durable.enums.read()["public.saved_enum"].labels,
                enums["public.saved_enum"].labels
            );
            sql(&engine, "DROP TYPE saved_enum; CREATE TYPE saved_enum AS ENUM('replacement'); DROP TYPE saved_shell; CREATE TYPE saved_shell");
            assert_ne!(
                engine.durable.enums.read()["public.saved_enum"].object_id,
                enums["public.saved_enum"].object_id
            );
        }
        drop(engine);
        let saved_enums = enum_key.retained(&cut).unwrap().unwrap();
        let saved_shells = shell_key.retained(&cut).unwrap().unwrap();
        drop(cut);
        let token = CancellationToken::new();
        let mut enum_image = vec![];
        let mut shell_image = vec![];
        saved_enums
            .get()
            .unwrap()
            .write_image(&mut enum_image, &token)
            .unwrap();
        saved_shells
            .get()
            .unwrap()
            .write_image(&mut shell_image, &token)
            .unwrap();
        drop((saved_enums, saved_shells));
        assert_eq!(control.memory().used(), 0);
        let memory = MemoryBudget::new(1 << 20);
        let restored_enums =
            TypeDefinitions::<StoredEnum>::read_image(&mut enum_image.as_slice(), &memory, &token)
                .unwrap();
        let restored_shells = TypeDefinitions::<StoredShellType>::read_image(
            &mut shell_image.as_slice(),
            &memory,
            &token,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&restored_enums).unwrap(),
            serde_json::to_value(&*enums).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&restored_shells).unwrap(),
            serde_json::to_value(&*shells).unwrap()
        );
        validate_enum_registry(&restored_enums, &roles).unwrap();
        validate_shell_registry(&restored_shells, &roles).unwrap();
        for label in &enums["public.saved_enum"].labels {
            assert_eq!(
                restored_enums["public.saved_enum"].label_by_key(&label.key),
                Some(label)
            );
        }
        drop((restored_enums, restored_shells));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn captured_server_and_graph_images_preserve_bound_metadata_after_replacement_and_closure() {
    for mutate in [false, true] {
        let engine = Engine::new();
        sql(&engine, "CREATE FOREIGN DATA WRAPPER custom HANDLER memory_fdw_handler; CREATE SERVER remote TYPE '' VERSION 'original' FOREIGN DATA WRAPPER custom OPTIONS (z 'first', a 'second')");
        engine.create_graph("saved_graph").unwrap();
        engine
            .add_graph_vertex(uqa_core::Vertex::new(1, "item"), "saved_graph")
            .unwrap();
        let roles = engine.durable.roles.snapshot();
        let wrappers = engine.durable.foreign_wrappers.snapshot();
        let servers = engine.durable.foreign_servers.snapshot();
        let graphs = engine.durable.graph_catalog_oids.snapshot();
        let server_key = engine.durable.foreign_servers.key().unwrap();
        let graph_key = engine.durable.graph_catalog_oids.key().unwrap();
        let control = StorageReadControl::with_limit(2 << 20);
        let cut = engine
            .storage
            .memory_capture
            .as_ref()
            .unwrap()
            .capture(&control)
            .unwrap();
        if mutate {
            sql(&engine, "BEGIN; DROP SERVER remote; SAVEPOINT changed; DROP FOREIGN DATA WRAPPER custom; ROLLBACK TO changed; ROLLBACK");
            assert!(*engine.durable.foreign_servers.read() == *servers);
            sql(&engine, "DROP FOREIGN DATA WRAPPER custom CASCADE; CREATE FOREIGN DATA WRAPPER custom; CREATE SERVER remote FOREIGN DATA WRAPPER custom");
            engine.drop_graph("saved_graph").unwrap();
            engine.create_graph("saved_graph").unwrap();
            assert_ne!(
                engine.durable.foreign_servers.read()["remote"]
                    .metadata
                    .object_id,
                servers["remote"].metadata.object_id
            );
        }
        drop(engine);
        let saved_servers = server_key.retained(&cut).unwrap().unwrap();
        let saved_graphs = graph_key.retained(&cut).unwrap().unwrap();
        drop(cut);
        let token = CancellationToken::new();
        let mut server_image = vec![];
        let mut graph_image = vec![];
        saved_servers
            .get()
            .unwrap()
            .write_image(&mut server_image, &token)
            .unwrap();
        saved_graphs
            .get()
            .unwrap()
            .write_image(&mut graph_image, &token)
            .unwrap();
        drop((saved_servers, saved_graphs));
        assert_eq!(control.memory().used(), 0);
        let memory = MemoryBudget::new(2 << 20);
        let restored_servers =
            ForeignServers::read_image(&mut server_image.as_slice(), &memory, &token).unwrap();
        let restored_graphs =
            GraphOidRegistry::read_image(&mut graph_image.as_slice(), &memory, &token).unwrap();
        assert!(restored_servers == *servers);
        assert!(restored_graphs == *graphs);
        validate_foreign_servers(&restored_servers, &roles).unwrap();
        assert_eq!(
            restored_servers["remote"].bound_wrapper(&wrappers).unwrap(),
            &wrappers["custom"]
        );
        drop((restored_servers, restored_graphs));
        assert_eq!(memory.used(), 0);
    }
}
