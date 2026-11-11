//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    key_value::{KeyValueReadRevision, RecordKeyReadVisitor},
    read_control::{KeyValueReadVisitor, ValueReadVisitor},
    RelationSecurityRow,
};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct Counts {
    keys: AtomicUsize,
    values: AtomicUsize,
}

struct Source {
    control: StorageReadControl,
    counts: Arc<Counts>,
    failure: &'static str,
}

impl KeyValueRead for Source {
    fn control(&self) -> &StorageReadControl {
        &self.control
    }
    fn revision(&self, _: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        panic!("retained view has already been selected")
    }
    fn visit_value(&self, _: &[u8], _: &mut ValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("bounded values are required")
    }
    fn visit_prefix(&self, _: &[u8], _: &mut KeyValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("a complete scan is not an input step")
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
        for (key, live) in [(b"a/1", true), (b"d/0", false), (b"d/1", true)] {
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
        if key == b"a/1" {
            match self.failure {
                "error" => return Err(StorageBackendError::Other("ACL source failure".into())),
                "panic" => panic!("ACL source failure"),
                _ => (),
            }
        }
        control.check_value_size(1, maximum)?;
        visit(Some(b"x"))
    }
}

fn definition(
    _: &[u8],
    _: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<TableSchema>>> {
    control.check()?;
    Ok(Some(Budgeted::new(
        TableSchema {
            relation: RelationIdentity {
                schema: String::new(),
                name: String::new(),
            },
            analyzer_json: String::new(),
            fts_fields: Vec::new(),
            vector_fields: Vec::new(),
            columns_json: String::new(),
            constraints_json: String::new(),
            object_id: [1; 16],
            storage_generation: [2; 16],
            security: RelationSecurityRow::bootstrap(),
        },
        control.memory().empty_reservation(),
    )))
}

fn acl(
    _: &[u8],
    _: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<CatalogTableAcl>>> {
    CatalogTableAcl::decode(
        r#"uqa.relation_acl.v1:{"schema":"s","name":"t"}:null"#,
        br#"{"format":1,"revision":[1,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"acl":null}"#,
        control,
    )
    .map(Some)
}

fn source(failure: &'static str) -> Arc<Source> {
    Arc::new(Source {
        control: StorageReadControl::with_limit(65536),
        counts: Arc::default(),
        failure,
    })
}

fn input(source: Arc<Source>, control: &StorageReadControl) -> CatalogTableInput {
    CatalogTableInput::new(
        source,
        (b"d/", definition),
        (b"a/", acl),
        CatalogInputLimits {
            maximum_key_bytes: 64,
            maximum_value_bytes: 64,
        },
        control,
    )
    .unwrap()
}

#[test]
fn catalog_table_input_bounds_slots_and_keeps_one_view_through_the_definition_boundary() {
    let source = source("none");
    let counts = source.counts.clone();
    let weak = Arc::downgrade(&source);
    let control = StorageReadControl::with_limit(65536);
    let mut input = input(source, &control);
    assert_eq!(counts.keys.load(Ordering::SeqCst), 0);
    assert!(matches!(
        input.advance().unwrap(),
        CatalogTableInputStep::Pending
    ));
    assert_eq!(counts.keys.load(Ordering::SeqCst), 1);
    assert_eq!(counts.values.load(Ordering::SeqCst), 0);
    assert!(matches!(
        input.advance().unwrap(),
        CatalogTableInputStep::Definition(_)
    ));
    assert_eq!(counts.keys.load(Ordering::SeqCst), 2);
    assert_eq!(counts.values.load(Ordering::SeqCst), 1);
    assert!(matches!(
        input.advance().unwrap(),
        CatalogTableInputStep::DefinitionsComplete
    ));
    assert!(weak.upgrade().is_some());
    assert_eq!(counts.values.load(Ordering::SeqCst), 1);
    let CatalogTableInputStep::AclReplacement(row) = input.advance().unwrap() else {
        panic!("original ACL record follows definitions");
    };
    assert_eq!(row.relation, RelationIdentity::new("s", "t"));
    assert_eq!(row.column, None);
    assert_eq!(row.tuple.acl, None);
    assert_eq!(counts.values.load(Ordering::SeqCst), 2);
    assert!(matches!(
        input.advance().unwrap(),
        CatalogTableInputStep::Complete
    ));
    assert!(weak.upgrade().is_none());
    assert_eq!(control.memory().used(), row.reserved_bytes());
    control.cancellation().cancel();
    assert!(matches!(
        input.advance().unwrap(),
        CatalogTableInputStep::Complete
    ));
    drop(row);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn catalog_table_input_retires_both_sources_after_late_error_unwind_and_cancellation() {
    for mode in ["error", "panic", "input_cancel", "source_cancel", "drop"] {
        let source = source(mode);
        let counts = source.counts.clone();
        let original = source.control.clone();
        let weak = Arc::downgrade(&source);
        let control = StorageReadControl::with_limit(65536);
        let sibling = control.memory().reserve(19).unwrap();
        let mut input = input(source, &control);
        while !matches!(
            input.advance().unwrap(),
            CatalogTableInputStep::DefinitionsComplete
        ) {}
        if mode == "input_cancel" {
            control.cancellation().cancel();
        }
        if mode == "source_cancel" {
            original.cancellation().cancel();
        }
        if mode != "drop" {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| input.advance()));
            assert!(matches!(result, Err(_) | Ok(Err(_))), "{mode}");
            assert!(weak.upgrade().is_none());
            let reads = counts.values.load(Ordering::SeqCst);
            assert!(input.advance().is_err());
            assert_eq!(counts.values.load(Ordering::SeqCst), reads);
        }
        drop(input);
        assert!(weak.upgrade().is_none());
        assert_eq!(control.memory().used(), sibling.bytes());
    }
}
