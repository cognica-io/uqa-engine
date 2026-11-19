//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    catalog::input::CatalogInputLimits,
    key_value::{KeyValueRead, KeyValueReadRevision, RecordKeyReadVisitor},
    read_control::{KeyValueReadVisitor, ValueReadVisitor},
};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering as AtomicOrdering},
        Arc,
    },
};

struct Source {
    rows: BTreeMap<Vec<u8>, Vec<u8>>,
    control: StorageReadControl,
    visits: Arc<AtomicUsize>,
}

impl KeyValueRead for Source {
    fn control(&self) -> &StorageReadControl {
        &self.control
    }
    fn revision(&self, _: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        panic!("schema input must not recompute its view")
    }
    fn visit_value(&self, _: &[u8], _: &mut ValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("schema values require bounded reads")
    }
    fn visit_prefix(&self, _: &[u8], _: &mut KeyValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("schema input must not collect provider rows")
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
            self.visits.fetch_add(1, AtomicOrdering::SeqCst);
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
        let value = self.rows.get(key).map(Vec::as_slice);
        if let Some(value) = value {
            control.check_value_size(value.len(), maximum)?;
        }
        visit(value)
    }
}

fn decode(
    _: &[u8],
    bytes: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<SchemaRow>>> {
    super::super::input::row(bytes, control).map(Some)
}

fn fixture(control: &StorageReadControl) -> Arc<Source> {
    Arc::new(Source {
        rows: [
            (
                b"k/0".to_vec(),
                br#"{"name":"z","role_owner":"first"}"#.to_vec(),
            ),
            (
                b"k/1".to_vec(),
                br#"{"name":"a","role_owner":"second"}"#.to_vec(),
            ),
            (
                b"k/2".to_vec(),
                br#"{"name":"a","role_owner":"third"}"#.to_vec(),
            ),
        ]
        .into(),
        control: control.clone(),
        visits: Arc::default(),
    })
}

fn cursor(source: Arc<Source>, control: &StorageReadControl) -> CatalogInput<SchemaRow> {
    CatalogInput::new(
        source,
        b"k/",
        decode,
        CatalogInputLimits {
            maximum_key_bytes: 4096,
            maximum_value_bytes: 65536,
        },
        control,
    )
    .unwrap()
}

#[test]
fn complete_schema_input_preserves_physical_order_and_stable_duplicate_name_order() {
    for order in [CatalogSchemaOrder::Physical, CatalogSchemaOrder::Name] {
        let control = StorageReadControl::with_limit(1 << 20);
        let source = fixture(&control);
        let weak = Arc::downgrade(&source);
        let visits = Arc::clone(&source.visits);
        let mut input = CatalogSchemaLoad::new(cursor(source, &control), order, &control).unwrap();
        let mut completed = None;
        for _ in 0..256 {
            let before = visits.load(AtomicOrdering::SeqCst);
            completed = input.advance(1).unwrap();
            assert!(visits.load(AtomicOrdering::SeqCst) <= before + 1);
            if completed.is_some() {
                break;
            }
        }
        let rows = completed.unwrap();
        let owners: Vec<_> = rows
            .iter()
            .map(|row| match &**row {
                SchemaRow::Legacy(row) => row.role_owner.as_str(),
                SchemaRow::Bound(_) => unreachable!(),
            })
            .collect();
        let expected = match order {
            CatalogSchemaOrder::Physical => ["first", "second", "third"],
            CatalogSchemaOrder::Name => ["second", "third", "first"],
        };
        assert_eq!(owners, expected);
        assert_eq!(visits.load(AtomicOrdering::SeqCst), 3);
        assert!(weak.upgrade().is_none());
        drop((rows, input));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn complete_schema_input_rejects_late_physical_corruption_without_exposing_prefixes() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut source = fixture(&control);
    Arc::get_mut(&mut source)
        .unwrap()
        .rows
        .insert(b"k/3".to_vec(), b"{".to_vec());
    let weak = Arc::downgrade(&source);
    let mut input =
        CatalogSchemaLoad::new(cursor(source, &control), CatalogSchemaOrder::Name, &control)
            .unwrap();
    let mut failed = false;
    for _ in 0..256 {
        match input.advance(1) {
            Ok(None) => (),
            Ok(Some(_)) => panic!("incomplete schema prefix escaped"),
            Err(_) => {
                failed = true;
                break;
            }
        }
    }
    assert!(failed);
    assert!(weak.upgrade().is_none());
    assert!(input.advance(1).is_err());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn complete_schema_input_rejects_changed_memory_or_cancellation_owners() {
    for memory in [true, false] {
        let original = StorageReadControl::with_limit(1 << 20);
        let other = StorageReadControl::with_limit(1 << 20);
        let invoked = if memory {
            StorageReadControl::new(other.memory(), original.cancellation())
        } else {
            StorageReadControl::new(original.memory(), other.cancellation())
        };
        let source = fixture(&original);
        let weak = Arc::downgrade(&source);
        assert!(CatalogSchemaLoad::new(
            cursor(source, &original),
            CatalogSchemaOrder::Name,
            &invoked
        )
        .is_err());
        assert!(weak.upgrade().is_none());
        assert_eq!(original.memory().used(), 0);
        assert_eq!(other.memory().used(), 0);
    }
}
