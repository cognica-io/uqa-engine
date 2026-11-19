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
use std::sync::atomic::{AtomicUsize, Ordering};

mod ordering;
mod retirement;

#[derive(Default)]
struct Counts {
    keys: AtomicUsize,
    values: AtomicUsize,
}

struct Source {
    control: StorageReadControl,
    counts: Arc<Counts>,
    failure: &'static str,
    failure_key: &'static [u8],
}

impl KeyValueRead for Source {
    fn control(&self) -> &StorageReadControl {
        &self.control
    }
    fn revision(&self, _: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        panic!("the catalog view has already been selected")
    }
    fn visit_value(&self, _: &[u8], _: &mut ValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("only bounded values may be read")
    }
    fn visit_prefix(&self, _: &[u8], _: &mut KeyValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("input must not collect a complete physical family")
    }
    fn visit_record_keys_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut RecordKeyReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        control.check()?;
        assert_eq!(limit, 1);
        for (key, live) in [
            (b"b/0", false),
            (b"b/1", true),
            (b"b/2", true),
            (b"l/1", true),
        ] {
            if key.starts_with(prefix) && after.is_none_or(|after| key.as_slice() > after) {
                self.counts.keys.fetch_add(1, Ordering::SeqCst);
                visit(key, live)?;
                break;
            }
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
        if key == self.failure_key {
            match self.failure {
                "error" => return Err(StorageBackendError::Other("selected input failed".into())),
                "panic" => panic!("selected input panicked"),
                "missing" => return visit(None),
                _ => (),
            }
        }
        control.check_value_size(1, maximum)?;
        visit(Some(b"x"))
    }
}

fn binding(
    key: &[u8],
    _: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<CatalogAnalyzerBindingRow>>> {
    if key == b"b/1" {
        return Ok(None);
    }
    CatalogAnalyzerBindingRow::copy("table", "field", " { \"x\": 1.00 } ", control).map(Some)
}

fn label(
    _: &[u8],
    _: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<CatalogAnalyzerLabelRow>>> {
    CatalogAnalyzerLabelRow::copy("table", "field", "SEARCH", "", control).map(Some)
}

fn source(failure: &'static str, failure_key: &'static [u8]) -> Arc<Source> {
    Arc::new(Source {
        control: StorageReadControl::with_limit(65536),
        counts: Arc::default(),
        failure,
        failure_key,
    })
}

fn limits() -> CatalogInputLimits {
    CatalogInputLimits {
        maximum_key_bytes: 64,
        maximum_value_bytes: 64,
    }
}

fn input(source: Arc<Source>, control: &StorageReadControl) -> CatalogAnalyzerInput {
    CatalogAnalyzerInput::new(source, (b"b/", binding), (b"l/", label), limits(), control).unwrap()
}

#[test]
fn catalog_analyzer_input_bounds_slots_and_preserves_the_binding_semantic_boundary() {
    let source = source("none", b"");
    let counts = source.counts.clone();
    let weak = Arc::downgrade(&source);
    let control = StorageReadControl::with_limit(65536);
    let mut input = input(source, &control);
    assert_eq!(counts.keys.load(Ordering::SeqCst), 0);
    assert_eq!(counts.values.load(Ordering::SeqCst), 0);
    for (keys, values) in [(1, 0), (2, 1)] {
        assert!(matches!(
            input.advance().unwrap(),
            CatalogAnalyzerInputStep::Pending
        ));
        assert_eq!(counts.keys.load(Ordering::SeqCst), keys);
        assert_eq!(counts.values.load(Ordering::SeqCst), values);
    }
    let CatalogAnalyzerInputStep::Binding(binding) = input.advance().unwrap() else {
        panic!("the live binding follows the deleted and absent slots");
    };
    assert_eq!(binding.binding_json, " { \"x\": 1.00 } ");
    assert!(matches!(
        input.advance().unwrap(),
        CatalogAnalyzerInputStep::BindingsComplete
    ));
    assert_eq!(counts.keys.load(Ordering::SeqCst), 3);
    assert_eq!(counts.values.load(Ordering::SeqCst), 2);
    assert!(weak.upgrade().is_some());
    let CatalogAnalyzerInputStep::Label(label) = input.advance().unwrap() else {
        panic!("the original label follows the explicit binding boundary");
    };
    assert_eq!(label.phase, "SEARCH");
    assert_eq!(label.analyzer_name, "");
    assert_eq!(counts.keys.load(Ordering::SeqCst), 4);
    assert_eq!(counts.values.load(Ordering::SeqCst), 3);
    assert!(matches!(
        input.advance().unwrap(),
        CatalogAnalyzerInputStep::Complete
    ));
    assert!(weak.upgrade().is_none());
    assert_eq!(
        control.memory().used(),
        binding.reserved_bytes() + label.reserved_bytes()
    );
    control.cancellation().cancel();
    assert!(matches!(
        input.advance().unwrap(),
        CatalogAnalyzerInputStep::Complete
    ));
    drop((binding, label));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn catalog_analyzer_rows_admit_original_text_before_copying_and_keep_exact_capacities() {
    let table = "Quoted.Schema\0\"".repeat(257);
    let field = "한글.\nfield".repeat(1024);
    let json = " [ \"x\", 1.00 ] ".repeat(4096);
    let control = StorageReadControl::with_limit(1 << 20);
    let mut rows = None;
    let measured = allocation_counter::measure(|| {
        rows = Some((
            CatalogAnalyzerBindingRow::copy(&table, &field, &json, &control).unwrap(),
            CatalogAnalyzerLabelRow::copy(&table, &field, "Search", "", &control).unwrap(),
        ));
    });
    let (binding, label) = rows.unwrap();
    let bytes = binding.table.capacity()
        + binding.field.capacity()
        + binding.binding_json.capacity()
        + label.table.capacity()
        + label.field.capacity()
        + label.phase.capacity()
        + label.analyzer_name.capacity();
    assert_eq!(measured.bytes_current, i64::try_from(bytes).unwrap());
    assert_eq!(control.memory().used(), bytes);
    assert_eq!(binding.binding_json, json);
    assert_eq!(label.field, field);
    drop((binding, label));
    assert_eq!(control.memory().used(), 0);
    for limit in [0, 1, 32, 256, 1024, 8192] {
        let denied = StorageReadControl::with_limit(limit);
        assert!(CatalogAnalyzerBindingRow::copy(&table, &field, &json, &denied).is_err());
        assert!(CatalogAnalyzerLabelRow::copy(&table, &field, "Search", "", &denied).is_err());
        assert_eq!(denied.memory().used(), 0);
    }
    control.cancellation().cancel();
    assert!(CatalogAnalyzerBindingRow::copy(&table, &field, &json, &control).is_err());
    assert!(CatalogAnalyzerLabelRow::copy(&table, &field, "Search", "", &control).is_err());
    assert_eq!(control.memory().used(), 0);
}
