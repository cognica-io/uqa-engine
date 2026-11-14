//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::{
    catalog_role::RoleIdentity,
    memory::{Budgeted, BudgetedString, ProductionControl},
    Value,
};
use uqa_sql::{catalog::table_definition::StoredTableConstraints, ColumnType};
use uqa_storage::{
    inverted_index::AnalyzerDefault,
    key_value::{KeyValueStorageBackend, MemoryKeyValueStore},
    PersistentStorageBackend, StorageBackendError, StorageName,
};

mod capture;

fn text(value: &str, control: &StorageReadControl) -> Budgeted<String> {
    let mut text = BudgetedString::new(control.memory());
    text.push_str(value).unwrap();
    let (text, memory) = text.into_parts();
    Budgeted::new(text, memory)
}

fn contents(control: &StorageReadControl, literal_bytes: usize) -> TableContents {
    let production = ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    );
    let mut column = ColumnDef::nullable("body", ColumnType::Text);
    column.missing_value = Some(Value::Str("v".repeat(literal_bytes)));
    let columns = TableDefinition::with_control(&vec![column], &production).unwrap();
    let constraints = StoredTableConstraints::read_stored_json(
        r#"{"checks":[{"name":"valid","expr":{"Literal":true}}],"row_type_array_name":"_restored"}"#,
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    )
    .unwrap()
    .into_native()
    .unwrap();
    let security = TableMetadata::from_admitted(
        Budgeted::new(
            BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP),
            control.memory().empty_reservation(),
        ),
        control.cancellation(),
    )
    .unwrap();
    let fts_fields = TableMetadata::from_admitted(
        Budgeted::new(Vec::new(), control.memory().empty_reservation()),
        control.cancellation(),
    )
    .unwrap();
    let bytes = serde_json::to_vec(&uqa_analysis::Analyzer::default()).unwrap();
    let memory = control.memory().reserve(bytes.capacity()).unwrap();
    let analyzer =
        AnalyzerDefinition::from_image(Budgeted::new(bytes, memory), control.cancellation())
            .unwrap();
    let backend = KeyValueStorageBackend::new(Arc::new(MemoryKeyValueStore::new()));
    let name = StorageName::from_admitted(text("public.restored", control)).unwrap();
    let documents = backend
        .document_store_with_control(name.clone(), control)
        .unwrap();
    let default = AnalyzerDefault::from_definition(
        analyzer.clone(),
        uqa_analysis::AnalyzerResources::default(),
        control,
    )
    .unwrap();
    let text = backend
        .inverted_index_with_control(name, default, control)
        .unwrap();
    TableContents {
        object_id: [1; 16],
        storage_generation: [2; 16],
        security,
        documents,
        text,
        vectors: VectorIndexes::default(),
        values: ValueIndexes::default(),
        fts_fields,
        columns,
        constraints,
        analyzer,
        next_id: 7,
        maps_integer_keys: false,
        statistics: uqa_planner::ColumnStatistics::default(),
        statistics_loaded: true,
        statistics_dirty: true,
        maintenance: StatisticsMaintenance::default(),
    }
}

#[test]
fn native_table_assembly_keeps_payloads_and_snapshot_headers_after_parent_retirement() {
    let control = StorageReadControl::with_limit(8 << 20);
    let contents = contents(&control, 1 << 20);
    let columns = contents.columns.clone();
    let before = control.memory().used();
    let mut table = None;
    let allocations = allocation_counter::measure(|| {
        table = Some(Arc::new(contents.into_state(Some(&control)).unwrap()));
    });
    assert!(allocations.bytes_total < 16 << 10, "{allocations:?}");
    let table = table.unwrap();
    assert!(table.columns.generation().shares_definition(&columns));
    let next_id = table.next_id.snapshot();
    let definition = table.columns.generation();
    let name = table.row_type_array_name.generation();
    assert!(control.memory().used() > before + size_of::<TableState>());
    drop(columns);
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    *table.next_id.write() = 99;
    table.columns.write().publish(Vec::new()).unwrap();
    *table.row_type_array_name.write() = Some("_changed".into());
    table
        .document_store
        .write()
        .put(
            1,
            BTreeMap::from([("body".into(), Value::Str("live".into()))]),
        )
        .unwrap();
    assert_eq!(table.document_store.read().len().unwrap(), 1);
    assert_eq!(*next_id, 7);
    assert_eq!((**name).as_deref(), Some("_restored"));
    drop(table);
    drop(occupied);
    assert!(control.memory().used() > 1 << 20);
    assert!(
        matches!(&definition[0].missing_value, Some(Value::Str(value)) if value.len() == 1 << 20)
    );
    drop((definition, name));
    assert!(
        control.memory().used() > 0,
        "the scalar snapshot keeps its shared header"
    );
    drop(next_id);
    assert_eq!(control.memory().used(), 0);
}

fn header_sizes(value: &TableContents, control: &StorageReadControl) -> Vec<usize> {
    fn header<T: Clone>(value: &T, control: &StorageReadControl) -> usize {
        let before = control.memory().used();
        let owner = CatalogGeneration::with_budget(value.clone(), control.memory()).unwrap();
        let bytes = control.memory().used() - before;
        drop(owner);
        bytes
    }
    vec![
        size_of::<TableState>(),
        header(&value.security, control),
        header(&value.storage_generation, control),
        header(&value.fts_fields, control),
        header(&value.columns, control),
        header(&false, control),
        header(&value.next_id, control),
        header(&false, control),
        header(&value.analyzer, control),
        header(&value.statistics, control),
        header(&false, control),
        header(&false, control),
        header(&value.maintenance, control),
        header(&value.constraints.checks, control),
        header(&value.constraints.foreign_keys, control),
        header(&value.constraints.key_constraints, control),
        header(&value.constraints.hierarchy, control),
        header(&value.constraints.row_type_array_name, control),
        header(&value.constraints.dropped_attributes, control),
    ]
}

#[test]
fn native_table_assembly_retires_every_denied_header_and_cancelled_candidate() {
    let probe = StorageReadControl::with_limit(1 << 20);
    let original = contents(&probe, 1024);
    let headers = header_sizes(&original, &probe);
    drop(original);
    assert_eq!(probe.memory().used(), 0);
    let mut allowed = 0;
    for header in headers {
        let control = StorageReadControl::with_limit(1 << 20);
        let value = contents(&control, 1024);
        let occupied = control
            .memory()
            .reserve(control.memory().available() - allowed)
            .unwrap();
        assert!(matches!(
            value.into_state(Some(&control)),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(control.memory().used(), occupied.bytes());
        drop(occupied);
        assert_eq!(control.memory().used(), 0);
        allowed += header;
    }
    let control = StorageReadControl::with_limit(1 << 20);
    let value = contents(&control, 1024);
    let occupied = control
        .memory()
        .reserve(control.memory().available() - allowed)
        .unwrap();
    let table = value.into_state(Some(&control)).unwrap();
    drop(table);
    assert_eq!(control.memory().used(), occupied.bytes());
    drop(occupied);
    let value = contents(&control, 1024);
    control.cancellation().cancel();
    assert!(matches!(
        value.into_state(Some(&control)),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn native_table_assembly_preserves_explicit_and_inferred_column_declarations() {
    for declared in [None, Some(false), Some(true)] {
        for nonempty in [false, true] {
            let control = StorageReadControl::with_limit(1 << 20);
            let mut value = contents(&control, 1024);
            value.constraints.columns_declared = declared;
            if !nonempty {
                value.columns = TableDefinition::with_control(
                    &Vec::new(),
                    &ProductionControl::new(
                        control.memory(),
                        control.cancellation(),
                        control.cancellation(),
                    ),
                )
                .unwrap();
            }
            let table = value.into_state(Some(&control)).unwrap();
            assert_eq!(*table.columns_declared.read(), declared.unwrap_or(nonempty));
            assert!(*table.column_stats_loaded.read());
            assert!(*table.column_stats_dirty.read());
            assert_eq!(
                *table.statistics_maintenance.read(),
                StatisticsMaintenance::default()
            );
            assert_eq!(table.object_id(), [1; 16]);
            assert_eq!(table.storage_generation(), [2; 16]);
            drop(table);
            assert_eq!(control.memory().used(), 0);
        }
    }
}
