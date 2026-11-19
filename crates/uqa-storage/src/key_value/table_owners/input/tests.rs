//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    key_value::{catalog::keys::relation_key, KeyValueReadRevision, TAG_TABLE},
    read_control::{KeyValueReadVisitor, ValueReadVisitor},
    RelationSecurityRow,
};
use std::collections::BTreeMap;

struct Read {
    values: BTreeMap<Vec<u8>, Vec<u8>>,
    control: StorageReadControl,
}

impl KeyValueRead for Read {
    fn control(&self) -> &StorageReadControl {
        &self.control
    }
    fn revision(&self, _: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        panic!("original view")
    }
    fn visit_value(
        &self,
        key: &[u8],
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        visit(self.values.get(key).map(Vec::as_slice))
    }
    fn visit_prefix(&self, _: &[u8], _: &mut KeyValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("owner validation does not scan")
    }
    fn visit_value_bounded(
        &self,
        key: &[u8],
        maximum: usize,
        control: &StorageReadControl,
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        let value = self.values.get(key).map(Vec::as_slice);
        if let Some(value) = value {
            control.check_value_size(value.len(), maximum)?;
        }
        visit(value)
    }
}

#[test]
fn catalog_table_owner_input_matches_original_binding_and_identity_predicates() {
    for relation in [
        RelationIdentity::new("public", "item"),
        RelationIdentity::new("Quoted.Schema", "Ta\"ble"),
        RelationIdentity::new("", ""),
        RelationIdentity::new("unicode", "表"),
    ] {
        let table = TableSchema {
            relation,
            security: RelationSecurityRow::bootstrap(),
            object_id: [1; 16],
            storage_generation: [2; 16],
            analyzer_json: "{}".into(),
            fts_fields: Vec::new(),
            vector_fields: Vec::new(),
            columns_json: String::new(),
            constraints_json: String::new(),
        };
        for binding in 0..8 {
            for indexed in 0..3 {
                let control = StorageReadControl::with_limit(65536);
                let name = table.relation.qualified_name();
                let mut read = Read {
                    values: BTreeMap::new(),
                    control: StorageReadControl::with_limit(65536),
                };
                read.values.insert(
                    relation_key(TAG_TABLE, &table.relation).unwrap(),
                    serde_json::to_vec(&table).unwrap(),
                );
                let owner = Owner {
                    object: if binding == 3 {
                        [3; 16]
                    } else if binding == 5 {
                        [0; 16]
                    } else {
                        [1; 16]
                    },
                    generation: if binding == 4 { [4; 16] } else { [2; 16] },
                    catalog: binding != 2,
                };
                if binding != 0 {
                    let value = match binding {
                        6 => vec![1; 33],
                        7 => {
                            let mut value = owner.encode();
                            value[0] = 2;
                            value.to_vec()
                        }
                        _ => owner.encode().to_vec(),
                    };
                    read.values
                        .insert(binding_key(&name, &read.control).unwrap().to_vec(), value);
                }
                if indexed != 0 {
                    read.values.insert(
                        owner.identity_key().to_vec(),
                        if indexed == 1 {
                            name.as_bytes().to_vec()
                        } else {
                            b"other".to_vec()
                        },
                    );
                }
                let expected =
                    super::super::validate_table(&read, &table).map_err(|error| error.to_string());
                let actual = validate_table_with_control(
                    &read,
                    &table,
                    CatalogInputLimits {
                        maximum_key_bytes: 4096,
                        maximum_value_bytes: 4096,
                    },
                    &control,
                )
                .map_err(|error| error.to_string());
                assert_eq!(
                    actual, expected,
                    "binding {binding}, index {indexed}, name {name}"
                );
                assert_eq!(control.memory().used(), 0);
            }
        }
    }
}

#[test]
fn catalog_table_owner_input_rejects_large_physical_binding_before_copying() {
    let control = StorageReadControl::with_limit(65536);
    let table = TableSchema {
        relation: RelationIdentity::new("public", "item"),
        security: RelationSecurityRow::bootstrap(),
        object_id: [1; 16],
        storage_generation: [2; 16],
        analyzer_json: String::new(),
        fts_fields: Vec::new(),
        vector_fields: Vec::new(),
        columns_json: String::new(),
        constraints_json: String::new(),
    };
    let key = binding_key("public.item", &control).unwrap().to_vec();
    let read = Read {
        values: BTreeMap::from([(key, vec![0; 1 << 20])]),
        control: StorageReadControl::with_limit(65536),
    };
    let mut result = None;
    let allocation = allocation_counter::measure(|| {
        result = Some(validate_table_with_control(
            &read,
            &table,
            CatalogInputLimits {
                maximum_key_bytes: 4096,
                maximum_value_bytes: 4096,
            },
            &control,
        ));
    });
    assert!(matches!(
        result,
        Some(Err(crate::StorageBackendError::Memory(_)))
    ));
    assert!(allocation.bytes_max < 4096);
    assert_eq!(control.memory().used(), 0);
}
