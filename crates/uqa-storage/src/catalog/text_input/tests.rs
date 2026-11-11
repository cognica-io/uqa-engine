//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    key_value::{KeyValueReadRevision, RecordKeyReadVisitor},
    read_control::{KeyValueReadVisitor, ValueReadVisitor},
};
use std::{
    collections::BTreeMap,
    ops::Bound::{Excluded, Included, Unbounded},
    sync::atomic::{AtomicUsize, Ordering},
};

#[derive(Default)]
struct Counts {
    keys: AtomicUsize,
    values: AtomicUsize,
    decodes: AtomicUsize,
}

struct Source {
    rows: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    counts: Arc<Counts>,
    control: StorageReadControl,
    failure: &'static str,
}

impl KeyValueRead for Source {
    fn control(&self) -> &StorageReadControl {
        &self.control
    }

    fn revision(&self, _: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        panic!("text input does not recompute snapshot identity")
    }

    fn visit_value(&self, _: &[u8], _: &mut ValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("text input must check value size before materialization")
    }

    fn visit_prefix(&self, _: &[u8], _: &mut KeyValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("text input must not collect a whole catalog")
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
        let lower = after.map_or(Included(prefix), Excluded);
        for (key, value) in self.rows.range::<[u8], _>((lower, Unbounded)).take(limit) {
            if !key.starts_with(prefix) {
                break;
            }
            self.counts.keys.fetch_add(1, Ordering::SeqCst);
            visit(key, value.is_some())?;
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
        self.counts.values.fetch_add(1, Ordering::SeqCst);
        match self.failure {
            "error" => return Err(invalid("selected value failure")),
            "panic" => panic!("selected value failure"),
            _ => (),
        }
        let value = if self.failure == "missing" {
            None
        } else {
            self.rows.get(key).and_then(Option::as_deref)
        };
        if let Some(value) = value {
            control.check_value_size(value.len(), maximum)?;
        }
        self.counts.decodes.fetch_add(1, Ordering::SeqCst);
        visit(value)
    }
}

fn decode(
    key: &[u8],
    value: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<CatalogTextRow>>> {
    let name = std::str::from_utf8(key).map_err(|_| invalid("invalid key"))?;
    let value = std::str::from_utf8(value).map_err(|_| invalid("invalid value"))?;
    CatalogTextRow::copy(name, value, control).map(Some)
}

fn limits() -> CatalogTextLimits {
    CatalogTextLimits {
        maximum_key_bytes: 128,
        maximum_value_bytes: 8192,
    }
}

fn source(failure: &'static str) -> Arc<Source> {
    Arc::new(Source {
        rows: BTreeMap::from([(
            b"k/name".to_vec(),
            Some(match failure {
                "malformed" => vec![255],
                "unfunded" => vec![b'x'; 8192],
                _ => b"original".to_vec(),
            }),
        )]),
        counts: Arc::default(),
        control: StorageReadControl::with_limit(65536),
        failure,
    })
}

#[test]
fn catalog_text_input_bounds_tombstones_and_preserves_original_owned_text() {
    let mut rows = BTreeMap::new();
    for index in 0..257 {
        rows.insert(format!("k/{index:03}").into_bytes(), None);
    }
    let value = "{ \"original\": 1.00, \"quoted\": \"한글\\u0000\" }";
    rows.insert(b"k/name".to_vec(), Some(value.as_bytes().to_vec()));
    let source = Arc::new(Source {
        rows,
        counts: Arc::default(),
        control: StorageReadControl::with_limit(65536),
        failure: "none",
    });
    let weak = Arc::downgrade(&source);
    let counts = source.counts.clone();
    let control = StorageReadControl::with_limit(65536);
    let mut input = CatalogTextInput::new(source, b"k/", decode, limits(), &control).unwrap();
    assert_eq!(counts.keys.load(Ordering::SeqCst), 0);
    assert_eq!(counts.values.load(Ordering::SeqCst), 0);
    for index in 1..=257 {
        assert!(matches!(input.advance().unwrap(), CatalogTextStep::Pending));
        assert_eq!(counts.keys.load(Ordering::SeqCst), index);
        assert_eq!(counts.values.load(Ordering::SeqCst), 0);
    }
    let CatalogTextStep::Row(row) = input.advance().unwrap() else {
        panic!("the original live row follows the tombstones")
    };
    assert_eq!(row.name, "k/name");
    assert_eq!(row.value, value);
    assert_eq!(counts.values.load(Ordering::SeqCst), 1);
    assert!(matches!(
        input.advance().unwrap(),
        CatalogTextStep::Complete
    ));
    assert!(weak.upgrade().is_none());
    assert_eq!(
        control.memory().used(),
        row.name.capacity() + row.value.capacity()
    );
    control.cancellation().cancel();
    assert!(matches!(
        input.advance().unwrap(),
        CatalogTextStep::Complete
    ));
    assert_eq!(row.value, value);
    drop(row);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn catalog_text_input_retires_failed_cancelled_and_abandoned_original_views() {
    for mode in [
        "error",
        "panic",
        "malformed",
        "missing",
        "unfunded",
        "source_cancel",
        "input_cancel",
        "drop",
    ] {
        let source = source(mode);
        let weak = Arc::downgrade(&source);
        let original = source.control.clone();
        let control = StorageReadControl::with_limit(if mode == "unfunded" {
            size_of::<Input>() + 128
        } else {
            65536
        });
        let mut input = CatalogTextInput::new(source, b"k/", decode, limits(), &control).unwrap();
        if mode == "source_cancel" {
            original.cancellation().cancel();
        } else if mode == "input_cancel" {
            control.cancellation().cancel();
        }
        if mode != "drop" {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| input.advance()));
            assert!(matches!(result, Err(_) | Ok(Err(_))), "{mode}");
            assert!(input.advance().is_err());
            assert!(weak.upgrade().is_none());
            assert_eq!(control.memory().used(), 0);
        }
        drop(input);
        assert!(weak.upgrade().is_none());
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn catalog_text_input_checks_encoded_limits_before_value_decoding() {
    for key_limit in [true, false] {
        let source = source("none");
        let counts = source.counts.clone();
        let weak = Arc::downgrade(&source);
        let control = StorageReadControl::with_limit(65536);
        let mut bound = limits();
        if key_limit {
            bound.maximum_key_bytes = 2;
        } else {
            bound.maximum_value_bytes = 2;
        }
        let mut input = CatalogTextInput::new(source, b"k/", decode, bound, &control).unwrap();
        assert!(input.advance().is_err());
        assert_eq!(counts.keys.load(Ordering::SeqCst), 1);
        assert_eq!(
            counts.values.load(Ordering::SeqCst),
            usize::from(!key_limit)
        );
        assert_eq!(counts.decodes.load(Ordering::SeqCst), 0);
        assert!(weak.upgrade().is_none());
        assert_eq!(control.memory().used(), 0);
    }
    for bound in [
        CatalogTextLimits {
            maximum_key_bytes: 0,
            ..limits()
        },
        CatalogTextLimits {
            maximum_value_bytes: 0,
            ..limits()
        },
    ] {
        let control = StorageReadControl::with_limit(65536);
        assert!(CatalogTextInput::new(source("none"), b"k/", decode, bound, &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
    let denied = StorageReadControl::with_limit(0);
    assert!(CatalogTextInput::new(source("none"), b"k/", decode, limits(), &denied).is_err());
    assert_eq!(denied.memory().used(), 0);
}
