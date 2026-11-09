//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{backup::memory::table_cursor::TableCursor, Engine};
use uqa_core::memory::Budgeted;
use uqa_core::RelationIdentity;
use uqa_storage::{
    backup::{
        read_memory_image_address, read_memory_table_metadata, DecodedMemoryImageAddress,
        NativeImageStep, UnitAddress,
    },
    diskann_index::build::DiskANNTemporaryBudget,
};

struct Unit {
    address: Vec<u8>,
    bytes: Vec<u8>,
}

fn encoded_table(engine: &Engine) -> Vec<Unit> {
    let control = StorageReadControl::with_limit(32 << 20);
    let cut = engine.capture_committed_memory(&control).unwrap();
    let (name, table) = cut.next_table(None).unwrap().unwrap();
    let mut cursor = TableCursor::new(name, table, &control).unwrap();
    let mut units = Vec::new();
    loop {
        let mut unit = match cursor.next(&cut.cut, &control).unwrap() {
            NativeImageStep::Pending => continue,
            NativeImageStep::Complete => break,
            NativeImageStep::Unit(unit) => unit,
        };
        while !unit.reader.prepare(1).unwrap().ready {}
        let mut bytes = Vec::new();
        let mut buffer = [0; 31];
        loop {
            let n = unit.reader.read(&mut buffer).unwrap();
            if n == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..n]);
        }
        units.push(Unit {
            address: unit.key.to_vec(),
            bytes,
        });
    }
    drop((cursor, cut));
    assert_eq!(control.memory().used(), 0);
    units
}

fn address(
    unit: &Unit,
    input: &StorageReadControl,
) -> StorageBackendResult<DecodedMemoryImageAddress> {
    read_memory_image_address(
        UnitAddress {
            namespace: uqa_storage::backup::memory_image_namespace(),
            generation: 0,
            family: uqa_storage::backup::MemoryImageFamily::Table as u16,
            key: &unit.address,
        },
        input,
    )
}

fn reconstruct(
    destination: &Engine,
    units: &[Unit],
    input: &StorageReadControl,
) -> StorageBackendResult<(Budgeted<RelationIdentity>, Arc<TableState>)> {
    let mut table = input::TableInput::open(
        &address(&units[0], input)?,
        &mut units[0].bytes.as_slice(),
        input,
    )?;
    let temporary = DiskANNTemporaryBudget::new(8 << 20);
    let resources = uqa_analysis::AnalyzerResources::default();
    let fields = destination.durable.table_field_analyzers.read();
    let context = input::TableDestination {
        state: &destination.storage,
        fields: &fields,
        resources: &resources,
        maximum_value_bytes: 8 << 20,
        temporary: &temporary,
    };
    for unit in &units[1..] {
        let DecodedMemoryImageAddress::Table {
            unit: component, ..
        } = address(unit, input)?
        else {
            unreachable!()
        };
        table = table.read(&component, &mut unit.bytes.as_slice(), &context, input)?;
    }
    table.finish(&destination.storage, input)
}

fn install(
    destination: &mut Engine,
    name: Budgeted<RelationIdentity>,
    table: Arc<TableState>,
    input: &StorageReadControl,
) {
    let registry = destination.storage.memory_capture.as_ref().unwrap();
    let tables = crate::state::TableRegistryInput::new(registry, input)
        .unwrap()
        .add_table(name, table)
        .unwrap()
        .finish()
        .unwrap();
    destination.storage.tables = Arc::new(tables);
}

#[test]
fn reconstructed_table_keeps_native_rows_definitions_and_new_writes_after_input_retirement() {
    let source = Engine::new();
    source.sql("CREATE TABLE items(id integer PRIMARY KEY, body text NOT NULL DEFAULT 'default'); INSERT INTO items VALUES(1, 'original'), (9, 'last'); ANALYZE items", &[]).unwrap();
    let expected = source.sql("SELECT * FROM items ORDER BY id", &[]).unwrap();
    let units = encoded_table(&source);
    drop(source);
    let mut destination = Engine::new();
    let input = StorageReadControl::with_limit(32 << 20);
    let (name, table) = reconstruct(&destination, &units, &input).unwrap();
    let retained = table.columns.generation();
    install(&mut destination, name, table.clone(), &input);
    input.cancellation().cancel();
    assert_eq!(
        destination
            .sql("SELECT * FROM items ORDER BY id", &[])
            .unwrap()
            .rows,
        expected.rows
    );
    destination.sql("BEGIN; INSERT INTO items(id) VALUES(12); UPDATE items SET body='private' WHERE id=1; ROLLBACK", &[]).unwrap();
    assert_eq!(
        destination
            .sql("SELECT * FROM items ORDER BY id", &[])
            .unwrap()
            .rows,
        expected.rows
    );
    destination
        .sql("INSERT INTO items(id) VALUES(12)", &[])
        .unwrap();
    assert_eq!(
        destination
            .sql("SELECT body FROM items WHERE id=12", &[])
            .unwrap()
            .rows[0]["body"],
        uqa_core::Value::Str("default".into())
    );
    assert!(destination
        .sql("INSERT INTO items VALUES(1, 'duplicate')", &[])
        .is_err());
    assert!(input.memory().used() > 0);
    drop((table, destination));
    assert!(input.memory().used() > 0);
    drop(retained);
    assert_eq!(input.memory().used(), 0);
}

#[test]
fn rejected_table_input_retires_partial_native_components_and_never_publishes() {
    let source = Engine::new();
    source
        .sql(
            "CREATE TABLE items(id integer, body text); INSERT INTO items VALUES(1,'original')",
            &[],
        )
        .unwrap();
    let mut units = encoded_table(&source);
    drop(source);
    units.last_mut().unwrap().bytes.push(0);
    let destination = Engine::new();
    let input = StorageReadControl::with_limit(32 << 20);
    assert!(reconstruct(&destination, &units, &input).is_err());
    assert_eq!(input.memory().used(), 0);
    assert!(destination.storage.tables.read().is_empty());
    let properties = read_memory_table_metadata(&mut units[0].bytes.as_slice(), &input).unwrap();
    let metadata = metadata::MetadataInput::new(properties);
    assert!(metadata
        .read(2, &mut units[2].bytes.as_slice(), &input)
        .is_err());
    assert_eq!(input.memory().used(), 0);
}

#[test]
fn table_registration_failure_retires_complete_input_and_keeps_the_destination_private() {
    let source = Engine::new();
    source
        .sql(
            "CREATE TABLE items(body text); INSERT INTO items VALUES('original')",
            &[],
        )
        .unwrap();
    let units = encoded_table(&source);
    drop(source);
    for cancelled in [false, true] {
        let destination = Engine::new();
        let input = StorageReadControl::with_limit(8 << 20);
        let (name, table) = reconstruct(&destination, &units, &input).unwrap();
        let registry = destination.storage.memory_capture.as_ref().unwrap();
        let tables = crate::state::TableRegistryInput::new(registry, &input).unwrap();
        let full = input.memory().reserve(input.memory().available()).unwrap();
        if cancelled {
            input.cancellation().cancel();
        }
        assert!(tables.add_table(name, table).is_err());
        assert!(destination.storage.tables.read().is_empty());
        drop(full);
        assert_eq!(input.memory().used(), 0);
    }
    let input = StorageReadControl::with_limit(8 << 20);
    let mut address = address(&units[0], &input).unwrap();
    let DecodedMemoryImageAddress::Table { object, .. } = &mut address else {
        unreachable!()
    };
    object[0] ^= 1;
    let names = input.memory().used();
    assert!(input::TableInput::open(&address, &mut units[0].bytes.as_slice(), &input).is_err());
    assert_eq!(input.memory().used(), names);
    drop(address);
    assert_eq!(input.memory().used(), 0);
}

fn source_with_historical_analyzers() -> Engine {
    let source = Engine::new();
    source
        .create_table(
            "items",
            uqa_analysis::standard_analyzer("english"),
            vec!["body".into()],
        )
        .unwrap();
    source
        .sql("ALTER TABLE items ADD COLUMN body text", &[])
        .unwrap();
    source
        .register_named_analyzer("historical", r#"{"tokenizer":{"type":"whitespace"}}"#)
        .unwrap();
    source
        .set_table_field_analyzer("items", "body", "historical", "index")
        .unwrap();
    source
        .register_named_analyzer("historical", r#"{"tokenizer":{"type":"keyword"}}"#)
        .unwrap();
    source
        .set_table_field_analyzer("items", "body", "historical", "search")
        .unwrap();
    source
}

struct FieldImage {
    key: (String, String),
    bytes: Vec<u8>,
}

fn encoded_field(engine: &Engine) -> FieldImage {
    let captured = StorageReadControl::with_limit(32 << 20);
    let cut = engine.capture_committed_memory(&captured).unwrap();
    let (field_key, mut reader) =
        crate::backup::memory::field_analyzers::next(&cut, None, &captured)
            .unwrap()
            .unwrap();
    let mut wire = Vec::new();
    let mut buffer = [0; 19];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        wire.extend_from_slice(&buffer[..count]);
    }
    let key = (*field_key).clone();
    drop((reader, cut, field_key));
    assert_eq!(captured.memory().used(), 0);
    FieldImage { key, bytes: wire }
}

#[test]
fn reconstructed_table_uses_original_field_revisions_and_live_publication_after_input_retirement() {
    use uqa_core::{
        memory::{MemoryBudget, ProductionControl},
        Value,
    };
    use uqa_storage::{analyzer_binding::registry::FieldAnalyzerCatalog, FieldAnalyzerBinding};
    let source = source_with_historical_analyzers();
    let document =
        |text: &str| std::collections::BTreeMap::from([("body".into(), Value::Str(text.into()))]);
    source
        .add_document("items", 1, document("Cats DOGS"))
        .unwrap();
    let units = encoded_table(&source);
    let image = encoded_field(&source);
    drop(source);
    let input = StorageReadControl::with_limit(32 << 20);
    let production =
        ProductionControl::new(input.memory(), input.cancellation(), input.cancellation());
    let (table, table_memory) = production.copy_text(&image.key.0).unwrap().into_parts();
    let (field, field_memory) = production.copy_text(&image.key.1).unwrap().into_parts();
    let key = production
        .finish(
            (table, field),
            production.combine(table_memory, field_memory),
        )
        .unwrap()
        .into_budgeted()
        .unwrap();
    let resources = uqa_analysis::AnalyzerResources::default();
    let binding =
        FieldAnalyzerBinding::read_image(&mut image.bytes.as_slice(), &resources, &input).unwrap();
    let fields = FieldAnalyzerCatalog::new(input.memory())
        .insert_image(key, binding, &input)
        .unwrap();
    let mut destination = Engine::new();
    *destination.durable.table_field_analyzers.write() =
        fields.into_mutation_budget(&MemoryBudget::new(usize::MAX));
    let (name, table) = reconstruct(&destination, &units, &input).unwrap();
    install(&mut destination, name, table.clone(), &input);
    input.cancellation().cancel();
    let full = input.memory().reserve(input.memory().available()).unwrap();
    {
        let index = table.inverted_index.read();
        assert_eq!(
            index
                .index_analyzer_revision("body")
                .unwrap()
                .analyze("Cats DOGS")
                .unwrap(),
            ["Cats", "DOGS"]
        );
        assert_eq!(
            index
                .search_analyzer_revision("body")
                .unwrap()
                .analyze("Cats DOGS")
                .unwrap(),
            ["Cats DOGS"]
        );
        assert_eq!(index.get_posting_list("body", "Cats").unwrap().len(), 1);
    }
    destination
        .add_document("items", 2, document("More Cats"))
        .unwrap();
    assert_eq!(
        table
            .inverted_index
            .read()
            .get_posting_list("body", "Cats")
            .unwrap()
            .len(),
        2
    );
    destination
        .sql("BEGIN; DELETE FROM items; ROLLBACK", &[])
        .unwrap();
    assert_eq!(table.document_store.read().len().unwrap(), 2);
    destination.create_default_table("later", vec![]).unwrap();
    let output = StorageReadControl::with_limit(32 << 20);
    let cut = destination.capture_committed_memory(&output).unwrap();
    assert_eq!(
        cut.names().unwrap(),
        [
            RelationIdentity::new("public", "items"),
            RelationIdentity::new("public", "later")
        ]
    );
    assert_eq!(
        cut.table(&RelationIdentity::new("public", "items"))
            .unwrap()
            .document_store
            .len()
            .unwrap(),
        2
    );
    drop((cut, table, destination, full));
    assert_eq!(output.memory().used(), 0);
    assert_eq!(input.memory().used(), 0);
}
