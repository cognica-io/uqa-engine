//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;

#[test]
fn captured_model_images_preserve_every_layer_after_source_replacement_and_loss() {
    for model in fixtures::models() {
        let expected = encode(&model);
        let mut catalog = crate::model_catalog::ModelCatalog::default();
        catalog
            .insert("saved".into(), Arc::new(model.clone()))
            .unwrap();
        let catalog_control = StorageReadControl::with_limit(1 << 20);
        let captured_catalog = catalog.capture(&catalog_control).unwrap();
        let catalog_image = captured_catalog.image("saved").unwrap().unwrap();
        drop((catalog, captured_catalog));
        let mut catalog_bytes = Vec::new();
        catalog_image
            .write_image(&mut catalog_bytes, &StorageReadControl::with_limit(0))
            .unwrap();
        assert_eq!(catalog_bytes, expected);
        drop(catalog_image);
        assert_eq!(catalog_control.memory().used(), 0);
        let mut source = Arc::new(model);
        let control = StorageReadControl::with_limit(1 << 20);
        let captured = source.capture_image_with_control(&control).unwrap();
        Arc::make_mut(&mut source).alpha = 0.75;
        drop(source);
        let mut bytes = Vec::new();
        captured
            .write_image(&mut bytes, &StorageReadControl::with_limit(0))
            .unwrap();
        assert_eq!(bytes, expected);
        drop(captured);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn model_capture_admits_buffer_capacity_without_copying_weights_and_retires_independent_leases() {
    let mut model = small();
    let DeepLayerSpec::Dense { weights, .. } = &mut model.layers[1] else {
        unreachable!()
    };
    weights.reserve_exact(1 << 18);
    let source = Arc::new(model);
    let small = StorageReadControl::with_limit(1 << 20);
    let denied = allocation_counter::measure(|| {
        assert!(matches!(
            source.capture_image_with_control(&small),
            Err(StorageBackendError::Memory(_))
        ));
    });
    assert!(denied.bytes_max < 4096);
    assert_eq!(small.memory().used(), 0);
    assert_eq!(Arc::strong_count(&source), 1);
    let first = StorageReadControl::with_limit(4 << 20);
    let second = StorageReadControl::with_limit(4 << 20);
    let mut captured = None;
    let allocation = allocation_counter::measure(|| {
        captured = Some(source.capture_image_with_control(&first).unwrap());
    });
    assert!(allocation.bytes_max < 4096);
    let captured = captured.unwrap();
    let independent = source.capture_image_with_control(&second).unwrap();
    assert_eq!(Arc::strong_count(&source), 3);
    assert!(first.memory().used() > 2 << 20);
    assert_eq!(first.memory().used(), second.memory().used());
    let weak = Arc::downgrade(&source);
    drop(source);
    let shared = captured.clone();
    drop((captured, independent));
    assert_eq!(second.memory().used(), 0);
    assert!(weak.upgrade().is_some());
    assert!(first.memory().used() > 2 << 20);
    drop(shared);
    assert!(weak.upgrade().is_none());
    assert_eq!(first.memory().used(), 0);
}

#[test]
fn captured_model_output_observes_both_original_and_invoking_cancellation() {
    struct CancelAfterWrite<'a> {
        calls: usize,
        token: &'a uqa_core::CancellationToken,
    }
    impl std::io::Write for CancelAfterWrite<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.calls += 1;
            self.token.cancel();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    for original in [false, true] {
        let source = Arc::new(small());
        let retained = StorageReadControl::with_limit(4096);
        let invoking = StorageReadControl::with_limit(0);
        let captured = source.capture_image_with_control(&retained).unwrap();
        let token = if original {
            retained.cancellation()
        } else {
            invoking.cancellation()
        };
        let mut output = CancelAfterWrite { calls: 0, token };
        assert!(matches!(
            captured.write_image(&mut output, &invoking),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert_eq!(output.calls, 1);
        let mut bytes = Vec::new();
        let retry = captured.write_image(&mut bytes, &StorageReadControl::with_limit(0));
        if original {
            assert!(matches!(retry, Err(StorageBackendError::Cancelled(_))));
            assert!(bytes.is_empty());
        } else {
            retry.unwrap();
            assert_eq!(bytes, encode(&source));
        }
        drop(captured);
        assert_eq!(retained.memory().used(), 0);
    }
}
