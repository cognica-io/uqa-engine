//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    catalog::sequence_load::CatalogSequences,
    key_value::{KeyValueReadRevision, RecordKeyReadVisitor},
    read_control::{KeyValueReadVisitor, ValueReadVisitor},
    CatalogFacade, KeyValueStore, MemoryKeyValueStore, RelationIdentity,
};
use std::{collections::BTreeMap, sync::Arc};

struct Source {
    rows: BTreeMap<Vec<u8>, Vec<u8>>,
    control: StorageReadControl,
    visits: parking_lot::Mutex<Vec<Vec<u8>>>,
}

impl KeyValueRead for Source {
    fn control(&self) -> &StorageReadControl {
        &self.control
    }
    fn revision(&self, _: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        panic!("sequence input must not recompute its view")
    }
    fn visit_value(&self, _: &[u8], _: &mut ValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("sequence values require bounded reads")
    }
    fn visit_prefix(&self, _: &[u8], _: &mut KeyValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("sequence input must not collect provider rows")
    }
    fn visit_record_keys_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut RecordKeyReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        assert_eq!(limit, 1);
        control.check()?;
        if let Some(key) = self
            .rows
            .keys()
            .find(|key| key.starts_with(prefix) && after.is_none_or(|after| key.as_slice() > after))
        {
            visit(key, true)?;
        }
        Ok(())
    }
    fn visit_value_bounded(
        &self,
        key: &[u8],
        maximum: usize,
        control: &StorageReadControl,
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.visits.lock().push(key.to_vec());
        let value = self.rows.get(key).map(Vec::as_slice);
        if let Some(value) = value {
            control.check_value_size(value.len(), maximum)?;
        }
        visit(value)
    }
}

fn key(name: &str) -> Vec<u8> {
    super::super::relation_key(TAG_SEQUENCE, &RelationIdentity::new("public", name)).unwrap()
}
fn definition(id: u8, legacy: bool) -> Vec<u8> {
    serde_json::to_vec(&super::super::StoredSequence {
        security: crate::SequenceSecurityRow::bootstrap(),
        object_id: [id; 16],
        definition_generation: [id + 16; 16],
        start: 1,
        increment: 1,
        current: legacy.then_some(41),
        called: None,
        log_count: None,
        persistence: "p".into(),
        owner: None,
        options: crate::SequenceOptions::default(),
    })
    .unwrap()
}
fn value_key(id: u8) -> Vec<u8> {
    let mut key = vec![TAG_SEQUENCE_VALUE];
    key.extend_from_slice(&[id; 16]);
    key.extend_from_slice(&[id + 16; 16]);
    key
}
fn value() -> Vec<u8> {
    br#"{"current":53,"called":false,"log_count":7,"increment":1,"min_value":1,"max_value":1000,"cycle":false,"cache_size":1}"#.to_vec()
}
fn source(rows: BTreeMap<Vec<u8>, Vec<u8>>) -> Arc<Source> {
    Arc::new(Source {
        rows,
        control: StorageReadControl::with_limit(1 << 20),
        visits: parking_lot::Mutex::default(),
    })
}
fn input(source: Arc<Source>, control: &StorageReadControl) -> CatalogSequenceLoad {
    let limits = CatalogInputLimits {
        maximum_key_bytes: 4096,
        maximum_value_bytes: 65536,
    };
    CatalogSequenceLoad::new(source, &[TAG_SEQUENCE], decode, position, limits, control).unwrap()
}
fn finish(input: &mut CatalogSequenceLoad) -> StorageBackendResult<CatalogSequences> {
    for _ in 0..65536 {
        if let Some(rows) = input.advance(1)? {
            return Ok(rows);
        }
    }
    panic!("sequence input stopped progressing")
}
fn original(source: &Source) -> StorageBackendResult<Vec<crate::SequenceRow>> {
    let store = Arc::new(MemoryKeyValueStore::new());
    for (key, value) in &source.rows {
        store.put(key, value)?;
    }
    KeyValueCatalog::new(store).load_sequence_rows()
}

#[test]
fn sequence_input_finishes_physical_definitions_before_values_and_relation_order() {
    let source = source(
        [
            (key("z"), definition(1, false)),
            (key("legacy"), definition(2, true)),
            (key("longer"), definition(3, false)),
            (value_key(1), value()),
            (value_key(3), value()),
        ]
        .into(),
    );
    let expected = original(&source).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let mut input = input(source.clone(), &control);
    let rows = finish(&mut input).unwrap();
    assert!(rows.iter().map(|row| &**row).eq(expected.iter()));
    let reads = source.visits.lock();
    assert_eq!(reads.len(), 6);
    assert!(reads[..3].iter().all(|key| key[0] == TAG_SEQUENCE));
    assert!(reads[3..].iter().all(|key| key[0] == TAG_SEQUENCE_VALUE));
    drop((rows, input));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn sequence_input_preserves_definition_and_unused_value_field_error_order() {
    for rows in [
        [(key("a"), definition(1, false)), (key("z"), b"{".to_vec())].into(),
        [(key("a"), definition(1, true)), (value_key(1), value())].into(),
        [(key("a"), definition(1, false))].into(),
        [
            (key("a"), definition(1, true)),
            (
                value_key(1),
                String::from_utf8(value())
                    .unwrap()
                    .replace("\"cache_size\":1", "\"cache_size\":null")
                    .into_bytes(),
            ),
        ]
        .into(),
    ] {
        let source = source(rows);
        let expected = original(&source).unwrap_err();
        let control = StorageReadControl::with_limit(1 << 20);
        let mut input = input(source.clone(), &control);
        let Err(actual) = finish(&mut input) else {
            panic!("accepted invalid sequence");
        };
        assert_eq!(
            std::mem::discriminant(&actual),
            std::mem::discriminant(&expected)
        );
        if matches!(expected, StorageBackendError::Other(_)) {
            assert_eq!(actual.to_string(), expected.to_string());
        }
        if source.rows.contains_key(&key("z")) {
            assert!(source
                .visits
                .lock()
                .iter()
                .all(|key| key[0] == TAG_SEQUENCE));
        }
        assert!(input.advance(1).is_err());
        drop(input);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn sequence_input_retires_partial_rows_when_its_original_read_owner_is_cancelled() {
    let source = source(
        [
            (key("a"), definition(1, true)),
            (key("b"), definition(2, true)),
        ]
        .into(),
    );
    let control = StorageReadControl::with_limit(1 << 20);
    let mut input = input(source.clone(), &control);
    for _ in 0..8 {
        assert!(input.advance(1).unwrap().is_none());
        if !source.visits.lock().is_empty() {
            break;
        }
    }
    assert!(control.memory().used() > 0);
    source.control.cancellation().cancel();
    assert!(finish(&mut input).is_err());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn sequence_input_reuses_original_relation_key_validation_before_value_decoding() {
    for schema in ["", "public", "한글\0schema", "a.b"] {
        for name in ["", "sequence", "longer한글\0name", "a.b"] {
            let canonical =
                super::super::relation_key(TAG_SEQUENCE, &RelationIdentity::new(schema, name))
                    .unwrap();
            for mut key in [canonical.clone(), canonical[..canonical.len() - 1].to_vec()] {
                for trailing in [false, true] {
                    if trailing {
                        key.push(0);
                    }
                    for bytes in [definition(1, true), b"{".to_vec()] {
                        let source = source([(key.clone(), bytes.clone())].into());
                        let control = StorageReadControl::with_limit(65536);
                        match (original(&source), decode(&key, &bytes, &control)) {
                            (Ok(expected), Ok(Some(actual))) => {
                                assert_eq!(actual.row.relation, expected[0].relation);
                            }
                            (Err(expected), Err(actual)) => {
                                assert_eq!(
                                    std::mem::discriminant(&actual),
                                    std::mem::discriminant(&expected)
                                );
                                if matches!(expected, StorageBackendError::Other(_)) {
                                    assert_eq!(actual.to_string(), expected.to_string());
                                }
                            }
                            _ => panic!("relation key decoding disagreed"),
                        }
                        assert_eq!(control.memory().used(), 0);
                    }
                }
            }
        }
    }
}
