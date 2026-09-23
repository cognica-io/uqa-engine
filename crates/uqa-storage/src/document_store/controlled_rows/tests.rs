//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    DocumentMetadata, MemoryDocumentStore, ReadOnlySnapshot, RetainedDocumentStoreBuilder,
    StoredDocument,
};
use std::sync::Arc;
use uqa_core::DocId;

struct Unsupported;
impl DocumentStore for Unsupported {
    fn put_stored(&mut self, _: DocId, _: StoredDocument) -> StorageBackendResult<()> {
        panic!("write");
    }
    fn get_stored(&self, _: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        panic!("uncontrolled row copy");
    }
    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        panic!("write");
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        panic!("write");
    }
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        panic!("whole corpus");
    }
    fn len(&self) -> StorageBackendResult<usize> {
        panic!("count");
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        panic!("replacement view");
    }
}

#[test]
fn unsupported_whole_row_reads_never_invoke_owned_materializers() {
    let control = StorageReadControl::with_limit(0);
    assert!(matches!(
        read_stored_documents(&Unsupported, &[1], &control),
        Err(StorageBackendError::Other(_))
    ));
    assert!(read_stored_documents(&Unsupported, &[], &control)
        .unwrap()
        .is_empty());
    assert!(Unsupported
        .get_stored_many_controlled(&[], &control)
        .unwrap()
        .is_empty());
    control.cancellation().cancel();
    assert!(matches!(
        read_stored_documents(&Unsupported, &[], &control),
        Err(StorageBackendError::Cancelled(_))
    ));
}

#[derive(Clone, Copy)]
enum Fault {
    ForeignPage,
    ForeignFields,
    Short,
    Long,
    Cancel,
}
struct Faulty {
    fault: Fault,
    foreign: StorageReadControl,
}
impl DocumentStore for Faulty {
    fn put_stored(&mut self, _: DocId, _: StoredDocument) -> StorageBackendResult<()> {
        panic!("write");
    }
    fn get_stored(&self, _: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        panic!("uncontrolled row copy");
    }
    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        panic!("write");
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        panic!("write");
    }
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        panic!("whole corpus");
    }
    fn len(&self) -> StorageBackendResult<usize> {
        panic!("count");
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        panic!("replacement view");
    }
    fn get_stored_many_controlled(
        &self,
        ids: &[DocId],
        control: &StorageReadControl,
    ) -> StorageBackendResult<RetainedDocumentPage> {
        let mut page = BudgetedVec::new(if matches!(self.fault, Fault::ForeignPage) {
            self.foreign.memory()
        } else {
            control.memory()
        });
        let fields_control = if matches!(self.fault, Fault::ForeignFields) {
            &self.foreign
        } else {
            control
        };
        let count = match self.fault {
            Fault::Short => ids.len() - 1,
            Fault::Long => ids.len() + 1,
            _ => ids.len(),
        };
        for _ in 0..count {
            let fields = copy_fields([("key", &Value::Int(1))], fields_control)?;
            page.push(Some(RetainedStoredDocument::with_metadata(
                fields,
                DocumentMetadata::default(),
            )))?;
        }
        if matches!(self.fault, Fault::Cancel) {
            control.cancellation().cancel();
        }
        Ok(page)
    }
}

#[test]
fn whole_row_boundary_rejects_foreign_payloads_malformed_counts_and_final_cancellation() {
    for fault in [
        Fault::ForeignPage,
        Fault::ForeignFields,
        Fault::Short,
        Fault::Long,
        Fault::Cancel,
    ] {
        let source = Faulty {
            fault,
            foreign: StorageReadControl::with_limit(1 << 20),
        };
        let control = StorageReadControl::with_limit(1 << 20);
        let result = read_stored_documents(&source, &[1, 2], &control);
        if matches!(fault, Fault::Cancel) {
            assert!(matches!(result, Err(StorageBackendError::Cancelled(_))));
        } else {
            assert!(matches!(result, Err(StorageBackendError::Other(_))));
        }
        assert_eq!(control.memory().used(), 0);
        assert_eq!(source.foreign.memory().used(), 0);
    }
}

fn document() -> StoredDocument {
    StoredDocument::with_metadata(
        [
            ("text".into(), Value::Str("한글".repeat(4096))),
            (
                "nested".into(),
                Value::Map([("bytes".into(), Value::Bytes(vec![9; 16 << 10]))].into()),
            ),
            ("null".into(), Value::Null),
        ]
        .into(),
        DocumentMetadata::with_tuple_xmin(71),
    )
}

#[test]
fn copied_whole_rows_preserve_presence_order_metadata_and_payloads_after_source_drop() {
    let expected = document();
    let mut memory = MemoryDocumentStore::new();
    memory.put_stored(3, expected.clone()).unwrap();
    let owner = StorageReadControl::with_limit(1 << 20);
    let mut builder = RetainedDocumentStoreBuilder::new(&owner);
    builder.add_document(3, expected.clone()).unwrap();
    let retained = builder.finish().unwrap();
    let read_only = ReadOnlySnapshot::new(memory.snapshot().unwrap());
    let owner_used = owner.memory().used();
    let control = StorageReadControl::with_limit(1 << 20);
    for source in [&memory as &dyn DocumentStore, &retained, &read_only] {
        let page = read_stored_documents(source, &[3, 99, 3], &control).unwrap();
        assert_eq!(page.len(), 3);
        assert!(page[1].is_none());
        for row in [&page[0], &page[2]] {
            let row = row.as_ref().unwrap();
            assert_eq!(row.fields(), expected.fields());
            assert_eq!(row.metadata(), expected.metadata());
        }
        assert!(control.memory().used() > 2 * (16 << 10));
        let row = page[0].as_ref().unwrap().clone();
        drop(page);
        assert!(control.memory().used() >= 16 << 10);
        assert_eq!(row.fields(), expected.fields());
        drop(row);
        assert_eq!(control.memory().used(), 0);
        assert_eq!(owner.memory().used(), owner_used);
        let tiny = StorageReadControl::with_limit(4096);
        assert!(matches!(
            read_stored_documents(source, &[99, 3], &tiny),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(tiny.memory().used(), 0);
    }
    let page = read_stored_documents(&memory, &[3], &control).unwrap();
    memory.clear().unwrap();
    drop(memory);
    assert_eq!(page[0].as_ref().unwrap().fields(), expected.fields());
    drop(page);
    assert_eq!(control.memory().used(), 0);
    owner.cancellation().cancel();
    assert!(matches!(
        read_stored_documents(&retained, &[3], &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
}
