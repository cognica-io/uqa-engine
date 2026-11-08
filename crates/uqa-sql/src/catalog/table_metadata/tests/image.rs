//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{ForeignKey, TableKeyConstraint},
    catalog::{image, relation_attributes::DroppedAttribute, table_metadata::TableMetadataWrite},
    SQLError,
};
use std::io::Read;
use uqa_core::memory::Budgeted;

trait Image: TableMetadataValue + std::fmt::Debug {
    fn output<'a>(
        &'a self,
        memory: &'a MemoryBudget,
        token: &'a CancellationToken,
    ) -> Result<image::DefinitionImageReader<'a>, SQLError>;
    fn raw(
        input: &mut dyn Read,
        memory: &MemoryBudget,
        token: &CancellationToken,
    ) -> Result<Budgeted<Self>, SQLError>;
    fn native(
        input: &mut dyn Read,
        memory: &MemoryBudget,
        token: &CancellationToken,
    ) -> Result<TableMetadata<Self>, SQLError>;
}

macro_rules! image_type {
    ($ty:ty, $output:ident, $input:ident) => {
        impl Image for $ty {
            fn output<'a>(
                &'a self,
                memory: &'a MemoryBudget,
                token: &'a CancellationToken,
            ) -> Result<image::DefinitionImageReader<'a>, SQLError> {
                image::$output(self, memory, token)
            }
            fn raw(
                input: &mut dyn Read,
                memory: &MemoryBudget,
                token: &CancellationToken,
            ) -> Result<Budgeted<Self>, SQLError> {
                image::$input(input, memory, token)
            }
            fn native(
                input: &mut dyn Read,
                memory: &MemoryBudget,
                token: &CancellationToken,
            ) -> Result<TableMetadata<Self>, SQLError> {
                TableMetadata::<Self>::read_image(input, memory, token)
            }
        }
    };
}
image_type!(
    Vec<ForeignKey>,
    foreign_keys_image_reader,
    read_foreign_keys_image
);
image_type!(
    Vec<TableKeyConstraint>,
    table_keys_image_reader,
    read_table_keys_image
);
image_type!(
    Vec<DroppedAttribute>,
    dropped_attributes_image_reader,
    read_dropped_attributes_image
);

fn encoded<T: Image>(value: &T) -> Vec<u8> {
    let memory = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    let mut bytes = Vec::new();
    let mut output = value.output(&memory, &token).unwrap();
    let mut buffer = [0; 127];
    loop {
        let count = output.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    drop(output);
    assert_eq!(memory.used(), 0);
    bytes
}

fn keys() -> Vec<TableKeyConstraint> {
    let crate::Statement::CreateTable(table) =
        crate::compile("CREATE TABLE t(id integer, PRIMARY KEY(id))")
            .unwrap()
            .remove(0)
    else {
        panic!("table")
    };
    table.key_constraints
}

fn round_trip<T: Image>(value: T) {
    let bytes = encoded(&value);
    drop(value);
    let token = CancellationToken::new();
    let probe = MemoryBudget::new(8 << 20);
    let mut raw = None;
    let raw_allocations = allocation_counter::measure(|| {
        raw = Some(T::raw(&mut bytes.as_slice(), &probe, &token).unwrap());
    });
    let raw = raw.unwrap();
    assert_eq!(probe.used(), raw.reserved_bytes());
    let limit = probe.peak() + size_of::<Budgeted<T>>();
    drop(raw);
    assert_eq!(probe.used(), 0);
    let memory = MemoryBudget::new(limit);
    let mut restored = None;
    let native_allocations = allocation_counter::measure(|| {
        restored = Some(T::native(&mut bytes.as_slice(), &memory, &token).unwrap());
    });
    assert!(
        native_allocations.bytes_total <= raw_allocations.bytes_total + 4096,
        "native adoption copied payload: {raw_allocations:?} -> {native_allocations:?}"
    );
    let restored = restored.unwrap();
    assert_eq!(encoded(&*restored), bytes);
    let retained = memory.used();
    let snapshot = restored.snapshot();
    capture_bytes(&restored, retained);
    let independent = MemoryBudget::new(retained);
    let control = ProductionControl::new(&independent, &token, &token);
    let mut captured = None;
    let allocation = allocation_counter::measure(|| {
        captured = Some(restored.capture_with_control(&control).unwrap());
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
    assert!(snapshot.shares_definition(&captured.as_ref().unwrap().snapshot()));
    drop((restored, captured));
    assert_eq!(independent.used(), 0);
    assert_eq!(memory.used(), retained);
    std::thread::spawn(move || assert_eq!(encoded(snapshot.as_ref()), bytes))
        .join()
        .unwrap();
    assert_eq!(memory.used(), 0);
}

#[test]
fn restored_table_metadata_retains_admission_in_every_shared_consumer() {
    let mut key = keys();
    key[0].name = Some("k".repeat(1 << 20));
    round_trip(key);
    let foreign: ForeignKey = serde_json::from_str(r#"{"referenced_key":"pk","name":"fk","local_columns":["local"],"ref_table":"public.parent","ref_columns":["id"],"on_delete_set_columns":["local"],"referenced_partitions":[]}"#).unwrap();
    round_trip(vec![foreign]);
    round_trip(vec![DroppedAttribute {
        number: 2,
        type_length: -1,
        type_modifier: -1,
        dimensions: 1,
        by_value: false,
        alignment: "i".into(),
        storage: "x".into(),
        collation: 100,
        identity: "d".into(),
        is_local: false,
        inheritance_count: 3,
    }]);
}

#[test]
fn restored_table_metadata_mutation_detaches_admission_and_preserves_selected_generations() {
    let bytes = encoded(&keys());
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(1 << 20);
    let mut value = TableMetadata::<Vec<TableKeyConstraint>>::read_image(
        &mut bytes.as_slice(),
        &memory,
        &token,
    )
    .unwrap();
    let selected = value.snapshot();
    let original_bytes = memory.used();
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut write = TableMetadata::write(&mut value);
        write[0].name = Some(String::with_capacity(4096));
        panic!("metadata publication unwind");
    }));
    assert!(unwind.is_err());
    assert!(!selected.shares_definition(&value.snapshot()));
    assert_eq!(encoded(selected.as_ref()), bytes);
    assert_eq!(memory.used(), original_bytes);
    capture_bytes(&value, super::super::footprint::retained(&*value).unwrap());
    let mut write: TableMetadataWrite<_, _> = TableMetadata::write(&mut value);
    write.clear();
    std::mem::forget(write);
    assert!(value
        .capture_with_control(&ProductionControl::uncontrolled())
        .is_err());
    *TableMetadata::write(&mut value) = vec![];
    capture_bytes(&value, size_of::<Vec<TableKeyConstraint>>());
    drop(selected);
    assert_eq!(memory.used(), 0);
    assert!(value.is_empty());
}

#[test]
fn restored_table_metadata_releases_failed_input_and_shared_owner_admission() {
    let bytes = encoded(&keys());
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(1 << 20);
    for end in [0, 7, bytes.len() / 2, bytes.len() - 1] {
        assert!(TableMetadata::<Vec<TableKeyConstraint>>::read_image(
            &mut &bytes[..end],
            &memory,
            &token
        )
        .is_err());
        assert_eq!(memory.used(), 0);
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(TableMetadata::<Vec<TableKeyConstraint>>::read_image(
        &mut trailing.as_slice(),
        &memory,
        &token
    )
    .is_err());
    assert_eq!(memory.used(), 0);
    drop(
        TableMetadata::<Vec<TableKeyConstraint>>::read_image(
            &mut bytes.as_slice(),
            &memory,
            &token,
        )
        .unwrap(),
    );
    for limit in [0, memory.peak() - 1] {
        let denied = MemoryBudget::new(limit);
        assert_eq!(
            TableMetadata::<Vec<TableKeyConstraint>>::read_image(
                &mut bytes.as_slice(),
                &denied,
                &token
            )
            .unwrap_err()
            .sqlstate(),
            Some("53200")
        );
        assert_eq!(denied.used(), 0);
    }
    token.cancel();
    assert!(TableMetadata::<Vec<TableKeyConstraint>>::read_image(
        &mut bytes.as_slice(),
        &memory,
        &token
    )
    .is_err());
    assert_eq!(memory.used(), 0);
}
