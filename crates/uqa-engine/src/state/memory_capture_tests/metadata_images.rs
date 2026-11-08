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
fn memory_index_and_scoring_images_preserve_selected_definitions_after_engine_retirement() {
    let engine = Engine::new();
    sql(&engine, "CREATE TABLE image_indexed(id integer, body text); CREATE INDEX saved_index ON image_indexed(id); CREATE TEMP TABLE private_indexed(id integer); CREATE INDEX private_index ON private_indexed(id)");
    engine
        .save_scoring_params("signal", "{ \"x\": 1.00, \"x\": 2 }")
        .unwrap();
    let expected = {
        let indexes = engine.durable.catalog_indexes.read();
        indexes
            .iter()
            .filter(|(key, _)| indexes.is_durable(key))
            .map(|(_, row)| row.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(expected.len(), 1);
    let index_key = engine.durable.catalog_indexes.key().unwrap();
    let params_key = engine.durable.scoring_params.key().unwrap();
    let control = StorageReadControl::with_limit(8 << 20);
    let cut = engine
        .storage
        .memory_capture
        .as_ref()
        .unwrap()
        .capture(&control)
        .unwrap();
    sql(&engine, "BEGIN; DROP INDEX saved_index; ROLLBACK; DROP INDEX saved_index; CREATE INDEX saved_index ON image_indexed(body)");
    engine.save_scoring_params("signal", "replacement").unwrap();
    drop(engine);
    let indexes = index_key.retained(&cut).unwrap().unwrap();
    let params = params_key.retained(&cut).unwrap().unwrap();
    drop(cut);
    let output = control.clone();
    let (indexes, params) = std::thread::spawn(move || {
        fn bytes(mut reader: uqa_storage::image_io::ImageReader<'_>) -> Vec<u8> {
            let mut bytes = Vec::new();
            let mut part = [0; 257];
            loop {
                let count = reader.read(&mut part).unwrap();
                if count == 0 {
                    return bytes;
                }
                bytes.extend_from_slice(&part[..count]);
            }
        }
        (
            bytes(indexes.get().unwrap().image_reader(&output).unwrap()),
            bytes(params.get().unwrap().image_reader(&output).unwrap()),
        )
    })
    .join()
    .unwrap();
    assert_eq!(control.memory().used(), 0);
    let restored = StorageReadControl::with_limit(1 << 20);
    let indexes = uqa_execution::catalog::index::registry::CatalogIndexes::read_image(
        &mut indexes.as_slice(),
        restored.memory(),
        restored.cancellation(),
    )
    .unwrap();
    assert_eq!(indexes.len(), expected.len());
    for original in expected {
        let actual = indexes.get(&original.relation).unwrap();
        assert_eq!(actual.relation, original.relation);
        assert_eq!(actual.index_type, original.index_type);
        assert_eq!(actual.table_name, original.table_name);
        assert_eq!(actual.columns_json, original.columns_json);
        assert_eq!(actual.parameters_json, original.parameters_json);
        assert_eq!(actual.definition_json, original.definition_json);
    }
    let params = uqa_storage::catalog::scoring_params::ScoringParameters::read_image(
        &mut params.as_slice(),
        &restored,
    )
    .unwrap();
    assert_eq!(
        params.get("signal").map(String::as_str),
        Some("{ \"x\": 1.00, \"x\": 2 }")
    );
    drop((indexes, params));
    assert_eq!(restored.memory().used(), 0);
}

#[test]
fn memory_comment_images_preserve_original_addresses_after_engine_retirement() {
    let engine = Engine::new();
    sql(&engine, "CREATE TYPE described_type AS ENUM('original'); COMMENT ON TYPE described_type IS 'original 한글'; CREATE TYPE pg_temp.private_object AS ENUM('private'); COMMENT ON TYPE pg_temp.private_object IS 'private description'");
    let expected = {
        let comments = engine.durable.comments.read();
        comments
            .iter()
            .filter(|(address, _)| !comments.is_temporary(address))
            .map(|(address, text)| (*address, text.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(expected.len(), 1);
    let key = engine.durable.comments.key().unwrap();
    let control = StorageReadControl::with_limit(8 << 20);
    let cut = engine
        .storage
        .memory_capture
        .as_ref()
        .unwrap()
        .capture(&control)
        .unwrap();
    sql(&engine, "BEGIN; COMMENT ON TYPE described_type IS 'private'; ROLLBACK; DROP TYPE described_type; CREATE TYPE described_type AS ENUM('replacement'); COMMENT ON TYPE described_type IS 'replacement description'");
    drop(engine);
    let captured = key.retained(&cut).unwrap().unwrap();
    drop(cut);
    let output_control = control.clone();
    let image = std::thread::spawn(move || {
        let mut reader = captured
            .get()
            .unwrap()
            .image_reader(&output_control)
            .unwrap();
        let mut bytes = Vec::new();
        let mut part = [0; 257];
        loop {
            let count = reader.read(&mut part).unwrap();
            if count == 0 {
                return bytes;
            }
            bytes.extend_from_slice(&part[..count]);
        }
    })
    .join()
    .unwrap();
    assert_eq!(control.memory().used(), 0);
    let memory = MemoryBudget::new(1 << 20);
    let restored = uqa_execution::catalog::comments::CommentRegistry::read_image(
        &mut image.as_slice(),
        &memory,
        &CancellationToken::new(),
    )
    .unwrap();
    assert!(restored
        .iter()
        .map(|(address, text)| (*address, text))
        .eq(expected.iter().map(|(address, text)| (*address, text))));
    drop(restored);
    assert_eq!(memory.used(), 0);
}

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

#[test]
fn captured_composite_and_range_images_preserve_native_definitions_after_engine_closure() {
    use uqa_sql::catalog::{
        composite_type::{validate_composite_registry, StoredComposite},
        range_type::{validate_range_registry, StoredRange},
    };
    for mutate in [false, true] {
        let engine = Engine::new();
        super::types::fixture(&engine);
        sql(&engine,"CREATE ROLE reader; ALTER TYPE retained_composite ADD ATTRIBUTE typed retained_domain; ALTER TYPE retained_composite ADD ATTRIBUTE enum_array retained_enum[]; GRANT USAGE ON TYPE retained_composite TO reader WITH GRANT OPTION");
        let roles = engine.durable.roles.snapshot();
        let composites = engine.durable.composites.snapshot();
        let ranges = engine.durable.ranges.snapshot();
        let composite_key = engine.durable.composites.key().unwrap();
        let range_key = engine.durable.ranges.key().unwrap();
        let control = StorageReadControl::with_limit(2 << 20);
        let cut = engine
            .storage
            .memory_capture
            .as_ref()
            .unwrap()
            .capture(&control)
            .unwrap();
        if mutate {
            sql(&engine,"BEGIN; ALTER TYPE retained_composite ADD ATTRIBUTE private integer; SAVEPOINT changed; ALTER TYPE retained_composite DROP ATTRIBUTE kept; ROLLBACK TO changed; ROLLBACK");
            assert_eq!(
                serde_json::to_value(&*engine.durable.composites.read()).unwrap(),
                serde_json::to_value(&*composites).unwrap()
            );
            sql(&engine,"DROP TYPE retained_composite; CREATE TYPE retained_composite AS (replacement boolean)");
            engine.durable.ranges.write().clear();
            assert_ne!(
                engine.durable.composites.read()["public.retained_composite"].object_id,
                composites["public.retained_composite"].object_id
            );
        }
        drop(engine);
        let saved_composites = composite_key.retained(&cut).unwrap().unwrap();
        let saved_ranges = range_key.retained(&cut).unwrap().unwrap();
        drop(cut);
        let (composite_image, range_image) = std::thread::spawn(move || {
            let token = CancellationToken::new();
            let mut composite_image = vec![];
            let mut range_image = vec![];
            saved_composites
                .get()
                .unwrap()
                .write_image(&mut composite_image, &token)
                .unwrap();
            saved_ranges
                .get()
                .unwrap()
                .write_image(&mut range_image, &token)
                .unwrap();
            (composite_image, range_image)
        })
        .join()
        .unwrap();
        assert_eq!(control.memory().used(), 0);
        let token = CancellationToken::new();
        let memory = MemoryBudget::new(2 << 20);
        let restored_composites = TypeDefinitions::<StoredComposite>::read_image(
            &mut composite_image.as_slice(),
            &memory,
            &token,
        )
        .unwrap();
        let restored_ranges = TypeDefinitions::<StoredRange>::read_image(
            &mut range_image.as_slice(),
            &memory,
            &token,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&restored_composites).unwrap(),
            serde_json::to_value(&*composites).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&restored_ranges).unwrap(),
            serde_json::to_value(&*ranges).unwrap()
        );
        validate_composite_registry(&restored_composites, &roles).unwrap();
        validate_range_registry(&restored_ranges, &roles).unwrap();
        let attributes = &restored_composites["public.retained_composite"].attributes;
        assert!(attributes[0].dropped);
        assert_eq!(attributes[0].number, 1);
        assert_eq!(attributes[1].number, 2);
        assert_eq!(
            restored_ranges["public.retained_range"].subtype_operator_class,
            1978
        );
        drop((restored_composites, restored_ranges));
        assert_eq!(memory.used(), 0);
    }
}
