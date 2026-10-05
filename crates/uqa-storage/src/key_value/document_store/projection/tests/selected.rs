//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::key_value::{
    codec::encode_stored_document_value, document_store::Source, KeyPresenceVisitor,
    KeyValuePointVisitor, KeyValueRead, KeyValueReadKeyIterator, KeyValueReadRevision,
};
use crate::mvcc::{
    DatabaseId, MemoryVersionStore, MergedRecordSnapshot, PrivateRecordChanges, RecordRead,
    RecordWrite,
};
use crate::read_control::{KeyValueReadVisitor, ValueReadVisitor};
use crate::StoredDocument;

#[test]
fn retained_projection_selects_spilled_documents_on_the_original_boundary() {
    let control = StorageReadControl::with_limit(512 << 10);
    let changes = PrivateRecordChanges::new(control.memory());
    for id in 1..=96 {
        let document = StoredDocument::new(Document::from([
            ("body".into(), Value::Str("x".repeat(4096))),
            ("id".into(), Value::Int(id as i64)),
        ]));
        changes
            .apply(
                &[RecordWrite {
                    key: &document_key("docs", id).unwrap(),
                    expected: None,
                    value: (id != 3)
                        .then_some(encode_stored_document_value(&document).unwrap().as_slice()),
                }],
                &control,
            )
            .unwrap();
    }
    let committed = MemoryVersionStore::new(&uqa_core::memory::MemoryBudget::new(1 << 20));
    let view = MergedRecordSnapshot::new(
        Arc::new(committed.snapshot().unwrap()),
        changes.snapshot().unwrap(),
    );
    let read = StorageReadControl::with_limit(128 << 10);
    {
        let key = document_key("docs", 1).unwrap();
        let mut cursor = view.private_cursor(&key, None, &read).unwrap();
        let entry = cursor.next(&read).unwrap().unwrap();
        assert_eq!(entry.key(), key);
        assert!(
            entry.read(&StorageReadControl::with_limit(0)).is_err(),
            "fixture must contain an actual spilled document"
        );
    }
    let retained = RecordRead::new(&view, DatabaseId::from_bytes([9; 16]), &read)
        .retain(&[b"d"])
        .unwrap();
    let documents = KeyValueDocumentStore {
        source: Source::Retained(retained),
        table: "docs".into(),
    };
    changes
        .apply(
            &[RecordWrite {
                key: &document_key("docs", 1).unwrap(),
                expected: None,
                value: None,
            }],
            &control,
        )
        .unwrap();
    let baseline = read.memory().used();
    let requested = [17, 1, 97, 17, 3, 2];
    let mut seen = Vec::new();
    documents
        .for_each_fields_multi_ref_with_presence(
            &requested,
            &["id", "body"],
            &mut |id, present, values| {
                assert_eq!(present, !matches!(id, 3 | 97));
                assert_eq!(
                    values[0],
                    &if present {
                        Value::Int(id as i64)
                    } else {
                        Value::Null
                    }
                );
                assert_eq!(
                    values[1],
                    &if present {
                        Value::Str("x".repeat(4096))
                    } else {
                        Value::Null
                    }
                );
                seen.push(id);
                true
            },
        )
        .unwrap();
    assert_eq!(seen, requested);
    assert_eq!(read.memory().used(), baseline);
    let mut seen = Vec::new();
    documents
        .for_each_fields_multi_ref_with_presence(&requested, &[], &mut |id, present, _| {
            seen.push((id, present));
            true
        })
        .unwrap();
    assert_eq!(seen, requested.map(|id| (id, !matches!(id, 3 | 97))));
    assert_eq!(read.memory().used(), baseline);
    drop(documents);
    assert_eq!(read.memory().used(), 0);
}

struct FaultyBatch {
    mode: u8,
    control: StorageReadControl,
}

impl KeyValueRead for FaultyBatch {
    fn control(&self) -> &StorageReadControl {
        &self.control
    }
    fn revision(&self, _: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        Ok(KeyValueReadRevision::fresh())
    }
    fn visit_value(&self, _: &[u8], _: &mut ValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("fixture supplies batches")
    }
    fn visit_prefix(&self, _: &[u8], _: &mut KeyValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("fixture supplies batches")
    }
    fn visit_values(
        &self,
        keys: &mut KeyValueReadKeyIterator<'_>,
        visit: &mut KeyValuePointVisitor<'_>,
    ) -> StorageBackendResult<()> {
        if self.mode == 0 {
            return Ok(());
        }
        let mut key = keys.next().unwrap()?;
        match self.mode {
            1 => {
                key[0] ^= 1;
                visit(&key, None)?;
            }
            2 => {
                visit(&key, None)?;
                visit(&key, None)?;
            }
            3 => {
                self.control.cancellation().cancel();
                let _ = visit(&key, None);
                return Err(other_error("later provider cleanup error"));
            }
            _ => unreachable!(),
        }
        Ok(())
    }
    fn visit_key_presence(
        &self,
        keys: &mut KeyValueReadKeyIterator<'_>,
        visit: &mut KeyPresenceVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.visit_values(keys, &mut |key, value| visit(key, value.is_some()))
    }
}

#[test]
fn malformed_provider_batches_fail_before_callbacks_and_preserve_the_first_error() {
    for fields in [&[][..], &["body"][..]] {
        for mode in 0..=3 {
            let source = Arc::new(FaultyBatch {
                mode,
                control: StorageReadControl::with_limit(4096),
            });
            let documents = KeyValueDocumentStore {
                source: Source::Retained(source.clone()),
                table: "docs".into(),
            };
            let error = documents
                .visit_projection(&[1, 2], fields, &mut |_, _, _| {
                    panic!("malformed batches cannot publish partial results")
                })
                .unwrap_err();
            if mode == 3 {
                assert!(
                    matches!(error, StorageBackendError::Cancelled(_)),
                    "{error:?}"
                );
            } else {
                assert!(
                    error.to_string().contains("document point visitor"),
                    "{error:?}"
                );
            }
            assert_eq!(source.control.memory().used(), 0);
        }
    }
}

#[test]
fn empty_projection_needs_no_key_or_batch_allocation() {
    let source = Arc::new(FaultyBatch {
        mode: 1,
        control: StorageReadControl::with_limit(0),
    });
    let documents = KeyValueDocumentStore {
        source: Source::Retained(source),
        table: "docs".into(),
    };
    documents
        .visit_projection(&[], &[], &mut |_, _, _| panic!("empty request"))
        .unwrap();
}
