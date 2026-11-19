//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::{
    catalog::{
        input::CatalogInputLimits,
        relation_acl::RelationAclTuple,
        table_input::{CatalogTableInput, CatalogTableInputStep},
    },
    read_control::StorageReadControl,
    RelationSecurityRow,
};

fn native() -> (ManagedConnection, Catalog) {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
        .unwrap();
    (connection, catalog)
}

fn limits() -> CatalogInputLimits {
    CatalogInputLimits {
        maximum_key_bytes: 65536,
        maximum_value_bytes: 8 << 20,
    }
}

fn collect(input: &mut CatalogTableInput) -> Vec<TableSchema> {
    let mut tables = Vec::new();
    let mut definitions_complete = false;
    loop {
        match input.advance().unwrap() {
            CatalogTableInputStep::Pending => (),
            CatalogTableInputStep::Definition(row) => {
                assert!(!definitions_complete);
                tables.push((**row).clone());
            }
            CatalogTableInputStep::DefinitionsComplete => {
                assert!(!definitions_complete);
                definitions_complete = true;
                tables.sort_unstable_by(|a, b| a.relation.cmp(&b.relation));
            }
            CatalogTableInputStep::AclReplacement(row) => {
                assert!(definitions_complete);
                for table in tables
                    .iter_mut()
                    .filter(|table| table.relation == row.relation)
                {
                    let RelationSecurityRow::Bound(security) = &mut table.security else {
                        panic!("bound fixture");
                    };
                    row.tuple.apply(row.column.as_deref(), security).unwrap();
                }
            }
            CatalogTableInputStep::Complete => {
                assert!(definitions_complete);
                break;
            }
        }
    }
    assert!(matches!(
        input.advance().unwrap(),
        CatalogTableInputStep::Complete
    ));
    tables
}

fn assert_tables(actual: &[TableSchema], expected: &[TableSchema]) {
    assert_eq!(
        serde_json::to_value(actual).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(actual.security, expected.security);
    }
}

#[test]
fn native_table_input_retains_matching_definition_and_acl_generations_after_source_closure() {
    let (connection, catalog) = native();
    catalog.save_schema("schema").unwrap();
    let mut table = empty_table("schema", "table");
    table.security = RelationSecurityRow::bootstrap();
    table.fts_fields = vec!["text".into(), "text".into(), "日本語".into()];
    table.vector_fields = vec![VectorFieldSchema {
        field: "vector".into(),
        dimensions: 3,
    }];
    table.analyzer_json = "original analyzer".into();
    catalog.save_table(&table).unwrap();
    let tuple = RelationAclTuple::new(Some(Vec::new())).unwrap();
    catalog
        .save_relation_acl(&table.relation, None, &tuple)
        .unwrap();
    catalog
        .save_relation_acl(&table.relation, Some("column"), &tuple)
        .unwrap();
    catalog
        .set_metadata("unrelated", &"x".repeat(4 << 20))
        .unwrap();
    let expected = catalog.load_tables().unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let mut input = catalog.begin_table_input(limits(), &control).unwrap();
    table.analyzer_json = "replacement analyzer".into();
    catalog.save_table(&table).unwrap();
    catalog
        .save_relation_acl(&table.relation, None, &RelationAclTuple::new(None).unwrap())
        .unwrap();
    assert_ne!(
        catalog.load_tables().unwrap()[0].security,
        expected[0].security
    );
    drop(catalog);
    drop(connection);
    assert_tables(&collect(&mut input), &expected);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn native_table_input_keeps_private_membership_through_rollback_and_name_reuse() {
    let (connection, catalog) = native();
    let original = empty_table("public", "original");
    catalog.save_table(&original).unwrap();
    connection.begin_transaction().unwrap();
    let mut private = empty_table("public", "private");
    private.object_id = [2; 16];
    private.storage_generation = [3; 16];
    catalog.save_table(&private).unwrap();
    let expected = catalog.load_tables().unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let mut input = catalog.begin_table_input(limits(), &control).unwrap();
    connection.rollback_transaction().unwrap();
    private.object_id = [4; 16];
    private.storage_generation = [5; 16];
    private.columns_json = "replacement".into();
    catalog.save_table(&private).unwrap();
    drop(catalog);
    drop(connection);
    assert_tables(&collect(&mut input), &expected);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn native_table_input_retires_both_streams_on_cancel_limits_and_abandonment() {
    let (_connection, catalog) = native();
    let mut table = empty_table("public", "original");
    table.security = RelationSecurityRow::bootstrap();
    catalog.save_table(&table).unwrap();
    catalog
        .save_relation_acl(&table.relation, None, &RelationAclTuple::new(None).unwrap())
        .unwrap();
    for cancel_after_definitions in [false, true] {
        let control = StorageReadControl::with_limit(1 << 20);
        let sibling = control.memory().reserve(19).unwrap();
        let mut input = catalog.begin_table_input(limits(), &control).unwrap();
        if cancel_after_definitions {
            loop {
                if matches!(
                    input.advance().unwrap(),
                    CatalogTableInputStep::DefinitionsComplete
                ) {
                    break;
                }
            }
        }
        control.cancellation().cancel();
        assert!(matches!(
            input.advance(),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert_eq!(control.memory().used(), sibling.bytes());
        assert!(input.advance().is_err());
    }
    for maximum_value_bytes in [1, 64] {
        let control = StorageReadControl::with_limit(1 << 20);
        let mut input = catalog
            .begin_table_input(
                CatalogInputLimits {
                    maximum_value_bytes,
                    ..limits()
                },
                &control,
            )
            .unwrap();
        assert!(matches!(
            input.advance(),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(control.memory().used(), 0);
        assert!(input.advance().is_err());
    }
    let control = StorageReadControl::with_limit(1 << 20);
    let input = catalog.begin_table_input(limits(), &control).unwrap();
    assert!(control.memory().used() > 0);
    drop(input);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn native_table_input_denies_large_definition_buffers_before_copying() {
    for size in [1 << 20, 4 << 20] {
        let (_connection, catalog) = native();
        let mut table = empty_table("public", "original");
        table.columns_json = "x".repeat(size);
        catalog.save_table(&table).unwrap();
        let control = StorageReadControl::with_limit(65536);
        let mut input = catalog.begin_table_input(limits(), &control).unwrap();
        let measured = allocation_counter::measure(|| {
            assert!(matches!(
                input.advance(),
                Err(StorageBackendError::Memory(_))
            ));
        });
        assert!(
            measured.bytes_max < size as u64,
            "copied a denied field: {measured:?}"
        );
        assert_eq!(control.memory().used(), 0);
    }
}
