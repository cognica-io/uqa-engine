//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::GraphLabelRegistry;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use uqa_core::image_io::ImageDecoder;
use uqa_storage::{
    mvcc::{IdentifierAllocation, IdentifierAllocator, IdentifierRequest},
    read_control::StorageReadControl,
    StorageBackendResult,
};

#[derive(Default)]
struct Source {
    values: BTreeMap<Vec<u8>, u64>,
    reads: Mutex<Vec<Vec<u8>>>,
}

impl IdentifierAllocator for Source {
    fn identifier_watermark(&self, key: &[u8]) -> StorageBackendResult<Option<u64>> {
        self.reads.lock().unwrap().push(key.to_vec());
        Ok(self.values.get(key).copied())
    }
    fn allocate_identifiers(
        &self,
        _: &[u8],
        _: IdentifierRequest,
    ) -> StorageBackendResult<IdentifierAllocation> {
        panic!("registry restoration cannot reserve or observe an identifier")
    }
}

#[test]
fn native_and_wire_registry_restoration_preserve_order_duplicates_and_unused_sequences() {
    for stored_identity in [[0; 16], [7; 16]] {
        let name = "graph\0한글".repeat(1025);
        let namespace = GraphIdentifierNamespace::new(None, [3; 16]);
        let identity = if stored_identity == [0; 16] {
            namespace.legacy_graph_identity(&name)
        } else {
            stored_identity
        };
        let registry = GraphLabelRegistry {
            allocation_id: stored_identity,
            labels: [
                ("z".into(), 3),
                ("a".into(), 9),
                ("b".into(), 3),
                ("c".into(), 0),
            ]
            .into(),
            sequences: [(2, 400), (3, 30), (99, 999)].into(),
            dropped_label_ids: [1, 3].into(),
            next_label_id: 20,
            ..GraphLabelRegistry::default()
        };
        let source = Arc::new(Source {
            values: [
                (namespace.label_key(identity).to_vec(), 21),
                (namespace.sequence_key(identity, 2).to_vec(), 40),
                (namespace.sequence_key(identity, 3).to_vec(), 300),
                (namespace.sequence_key(identity, 9).to_vec(), 900),
                (namespace.sequence_key(identity, 99).to_vec(), 9999),
            ]
            .into(),
            ..Source::default()
        });
        let expected_reads: Vec<_> = std::iter::once(namespace.label_key(identity).to_vec())
            .chain([2, 0, 3, 3, 9].map(|id| namespace.sequence_key(identity, id).to_vec()))
            .collect();
        let mut expected = registry.clone();
        expected.allocation_id = identity;
        expected.next_label_id = 22;
        expected.sequences.insert(3, 300);
        expected.sequences.insert(9, 900);
        let scope = GraphIdentifierScope::catalog(source.as_ref(), [3; 16]);
        let mut ordinary = registry.clone();
        scope.restore_registry(&name, &mut ordinary).unwrap();
        assert_eq!(ordinary, expected);
        assert_eq!(*source.reads.lock().unwrap(), expected_reads);
        source.reads.lock().unwrap().clear();
        let control = StorageReadControl::with_limit(4 << 20);
        let retained = control.clone();
        let allocator = source.clone();
        let json = serde_json::to_string(&registry).unwrap();
        let input = ImageDecoder::<_, StorageBackendError>::new(
            control.memory(),
            control.cancellation(),
            |mut channel| async move {
                let mut input = channel.input();
                let mut labels =
                    NativeLabels::read_stored_from(Some(&json), &retained, &mut input).await?;
                let production = ProductionControl::new(
                    retained.memory(),
                    retained.cancellation(),
                    input.cancellation,
                );
                GraphIdentifierScope::catalog(allocator.as_ref(), [3; 16])
                    .restore_native_registry_from(&name, &mut labels, &production, &mut input)
                    .await
                    .map_err(|error| StorageBackendError::backend("graph catalog", error))?;
                Ok(labels)
            },
        )
        .unwrap();
        let actual = input.read(&mut std::io::empty()).unwrap();
        assert_eq!(actual.to_record(), expected);
        assert_eq!(*source.reads.lock().unwrap(), expected_reads);
        drop(actual);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn registry_label_watermark_overflow_precedes_all_sequence_reads() {
    for last in [u64::from(u32::MAX), u64::MAX] {
        let namespace = GraphIdentifierNamespace::new(None, [0; 16]);
        let identity = [9; 16];
        let source = Source {
            values: [(namespace.label_key(identity).to_vec(), last)].into(),
            ..Source::default()
        };
        let mut registry = GraphLabelRegistry {
            allocation_id: identity,
            ..GraphLabelRegistry::default()
        };
        let error = GraphIdentifierScope::catalog(&source, [0; 16])
            .restore_registry("graph", &mut registry)
            .unwrap_err();
        assert!(
            matches!(error, GraphStoreError::CorruptGraph(ref message) if message == "invalid label allocation watermark")
        );
        assert_eq!(
            *source.reads.lock().unwrap(),
            [namespace.label_key(identity).to_vec()]
        );
        assert_eq!(registry.next_label_id, 3);
    }
}
