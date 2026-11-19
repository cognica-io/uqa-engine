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
    RelationIdentity,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering as AtomicOrdering},
    Arc,
};
use uqa_core::memory::BudgetedString;

struct Source {
    rows: Vec<(Vec<u8>, Vec<u8>)>,
    control: StorageReadControl,
    reads: Arc<AtomicUsize>,
}

impl KeyValueRead for Source {
    fn control(&self) -> &StorageReadControl {
        &self.control
    }
    fn revision(&self, _: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        panic!("index input retains the selected source")
    }
    fn visit_value(&self, _: &[u8], _: &mut ValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("index input must bound its original value")
    }
    fn visit_prefix(&self, _: &[u8], _: &mut KeyValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("index input must bound physical slots")
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
        if let Some((key, _)) = self.rows.iter().find(|(key, _)| {
            key.starts_with(prefix) && after.is_none_or(|last| key.as_slice() > last)
        }) {
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
        let value = &self.rows.iter().find(|(found, _)| found == key).unwrap().1;
        control.check_value_size(value.len(), maximum)?;
        self.reads.fetch_add(1, AtomicOrdering::SeqCst);
        visit(Some(value))
    }
}

fn copy(text: &str, control: &StorageReadControl) -> StorageBackendResult<Budgeted<String>> {
    let mut output = BudgetedString::new(control.memory());
    output.push_str(text)?;
    let (text, memory) = output.into_parts();
    Ok(Budgeted::new(text, memory))
}

fn decode(
    key: &[u8],
    value: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<CatalogIndexRow>>> {
    let text = std::str::from_utf8(value)
        .map_err(|_| StorageBackendError::Other("malformed physical index row".into()))?;
    let (schema, name) = text.split_once('\0').unwrap();
    let (schema, mut memory) = copy(schema, control)?.into_parts();
    let (name, name_memory) = copy(name, control)?.into_parts();
    memory.absorb(name_memory);
    Ok(Some(super::super::input::index::row(
        Budgeted::new(RelationIdentity::new(schema, name), memory),
        [
            copy("gin", control)?,
            copy("public.missing", control)?,
            copy("malformed keys", control)?,
            copy("malformed parameters", control)?,
        ],
        Some(copy(std::str::from_utf8(key).unwrap(), control)?),
    )))
}

fn source(names: &[(&str, &str)], control: &StorageReadControl) -> Arc<Source> {
    Arc::new(Source {
        rows: names
            .iter()
            .enumerate()
            .map(|(index, (schema, name))| {
                (
                    format!("k/{index:04}").into_bytes(),
                    format!("{schema}\0{name}").into_bytes(),
                )
            })
            .collect(),
        control: control.clone(),
        reads: Arc::default(),
    })
}

fn input(source: Arc<Source>, control: &StorageReadControl) -> CatalogInput<CatalogIndexRow> {
    CatalogInput::new(
        source,
        b"k/",
        decode,
        CatalogInputLimits {
            maximum_key_bytes: 128,
            maximum_value_bytes: 1 << 20,
        },
        control,
    )
    .unwrap()
}

fn finish(load: &mut CatalogIndexLoad, steps: usize) -> StorageBackendResult<CatalogIndexes> {
    for _ in 0..100_000 {
        if let Some(rows) = load.advance(steps)? {
            return Ok(rows);
        }
    }
    panic!("index catalog load stopped progressing")
}

#[test]
fn complete_index_load_preserves_physical_and_stable_relation_order_without_copying_rows() {
    let names = [("z", "a"), ("a", "z"), ("a", "a"), ("a", "a"), ("한", "x")];
    for order in [CatalogIndexOrder::Physical, CatalogIndexOrder::Relation] {
        let control = StorageReadControl::with_limit(1 << 20);
        let source = source(&names, &control);
        let weak = Arc::downgrade(&source);
        let reads = source.reads.clone();
        let mut load = CatalogIndexLoad::new(input(source, &control), order, &control).unwrap();
        let rows = finish(&mut load, 1).unwrap();
        assert_eq!(reads.load(AtomicOrdering::SeqCst), names.len());
        assert!(weak.upgrade().is_none());
        let mut expected = names.iter().copied().enumerate().collect::<Vec<_>>();
        if matches!(order, CatalogIndexOrder::Relation) {
            expected.sort_by_key(|(_, name)| *name);
        }
        assert_eq!(rows.len(), expected.len());
        for (row, (index, (schema, name))) in rows.iter().zip(expected) {
            assert_eq!((&*row.relation.schema, &*row.relation.name), (schema, name));
            assert_eq!(
                row.definition_json.as_deref(),
                Some(format!("k/{index:04}").as_str())
            );
        }
        let addresses = rows
            .iter()
            .map(|row| row.columns_json.as_ptr())
            .collect::<Vec<_>>();
        let (rows, memory) = rows.into_parts();
        let rows = rows
            .into_iter()
            .map(CatalogIndexRecord::into_budgeted)
            .collect::<Vec<_>>();
        assert_eq!(
            rows.iter()
                .map(|row| row.columns_json.as_ptr())
                .collect::<Vec<_>>(),
            addresses
        );
        assert!(load.advance(1).is_err());
        drop((rows, memory, load));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn complete_index_load_resumes_long_relation_comparisons_and_retires_ordering_scratch() {
    let common = "한schema".repeat(4096);
    let names = (0..24)
        .rev()
        .map(|i| format!("{common}_{i:03}"))
        .collect::<Vec<_>>();
    let names = names
        .iter()
        .map(|name| (common.as_str(), name.as_str()))
        .collect::<Vec<_>>();
    for outcome in ["complete", "cancel", "zero", "drop", "unwind"] {
        let control = StorageReadControl::with_limit(8 << 20);
        let source = source(&names, &control);
        let weak = Arc::downgrade(&source);
        let mut load = CatalogIndexLoad::new(
            input(source, &control),
            CatalogIndexOrder::Relation,
            &control,
        )
        .unwrap();
        // Source retirement marks the original complete-family boundary; the long common prefixes still require suspended ordering work.
        for _ in 0..4096 {
            assert!(load.advance(1).unwrap().is_none());
            if weak.upgrade().is_none() {
                break;
            }
        }
        assert!(weak.upgrade().is_none());
        let occupied = control
            .memory()
            .reserve(control.memory().available())
            .unwrap();
        if outcome == "cancel" {
            control.cancellation().cancel();
        }
        if outcome == "unwind" {
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let _load = load;
                    panic!("abandoned index ordering");
                }))
                .is_err()
            );
        } else {
            match outcome {
                "complete" => {
                    let rows = finish(&mut load, 1).unwrap();
                    assert!(rows
                        .windows(2)
                        .all(|rows| rows[0].relation <= rows[1].relation));
                    assert!(load.advance(1).is_err());
                    drop(rows);
                }
                "cancel" | "zero" => {
                    assert!(finish(&mut load, usize::from(outcome != "zero")).is_err());
                    assert!(load.advance(1).is_err());
                }
                "drop" => (),
                _ => unreachable!(),
            }
            drop(load);
        }
        assert_eq!(control.memory().used(), occupied.bytes());
        drop(occupied);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn complete_index_load_rejects_foreign_controls_and_late_physical_corruption_before_semantics() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut source = source(&[("public", "first"), ("public", "last")], &control);
    Arc::get_mut(&mut source).unwrap().rows[1].1 = vec![255];
    let weak = Arc::downgrade(&source);
    assert!(CatalogIndexLoad::new(
        input(source.clone(), &control),
        CatalogIndexOrder::Relation,
        &StorageReadControl::with_limit(1 << 20),
    )
    .is_err());
    let mut load = CatalogIndexLoad::new(
        input(source, &control),
        CatalogIndexOrder::Relation,
        &control,
    )
    .unwrap();
    assert_eq!(
        finish(&mut load, 1).err().unwrap().to_string(),
        "malformed physical index row"
    );
    assert!(weak.upgrade().is_none());
    assert!(load.advance(1).is_err());
    drop(load);
    assert_eq!(control.memory().used(), 0);
}
