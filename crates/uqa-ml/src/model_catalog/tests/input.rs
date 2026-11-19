//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn decoded(control: &StorageReadControl) -> Budgeted<DeepModel> {
    let source = model(1);
    let mut bytes = Vec::new();
    crate::model_image::write_model_image(&source, &mut bytes, &StorageReadControl::with_limit(0))
        .unwrap();
    drop(source);
    crate::model_image::read_model_image(&mut bytes.as_slice(), control).unwrap()
}

fn name(control: &StorageReadControl) -> Budgeted<String> {
    ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    )
    .copy_text("saved")
    .unwrap()
    .into_budgeted()
    .unwrap()
}

#[test]
fn model_image_registration_keeps_exact_weights_through_new_writes_capture_and_parent_retirement() {
    let input = StorageReadControl::with_limit(1 << 20);
    let model = decoded(&input);
    let DeepLayerSpec::Dense { weights, .. } = &model.layers[1] else {
        unreachable!()
    };
    let weights_address = weights.as_ptr();
    let runtime = MemoryBudget::new(65536);
    let catalog = ModelCatalog::new(input.memory())
        .insert_image(name(&input), model, &input)
        .unwrap();
    let retained = catalog.get("saved").unwrap().clone();
    let identity = retained.identity();
    let mut catalog = catalog.into_mutation_budget(&runtime);
    let full = input.memory().reserve(input.memory().available()).unwrap();
    input.cancellation().cancel();
    catalog.insert("other".into(), super::model(1)).unwrap();
    let capture_control = StorageReadControl::with_limit(1 << 20);
    let captured = catalog.capture(&capture_control).unwrap();
    let image = captured.image("saved").unwrap().unwrap();
    catalog.remove("saved").unwrap();
    drop((catalog, captured, full));
    assert_eq!(runtime.used(), 0);
    assert!(input.memory().used() > 0);
    let DeepLayerSpec::Dense { weights, .. } = &retained.layers[1] else {
        unreachable!()
    };
    assert_eq!(weights.as_ptr(), weights_address);
    assert_eq!(weights[0].to_bits(), (-0.0_f64).to_bits());
    drop(retained);
    assert!(identity.is_live());
    let encoded = super::image(&image);
    let comparison = StorageReadControl::with_limit(1 << 20);
    let decoded =
        crate::model_image::read_model_image(&mut encoded.as_slice(), &comparison).unwrap();
    assert_eq!(*decoded, *super::model(1));
    drop((decoded, image));
    assert!(!identity.is_live());
    assert_eq!(input.memory().used(), 0);
    assert_eq!(capture_control.memory().used(), 0);
    assert_eq!(comparison.memory().used(), 0);
}

#[test]
fn failed_model_image_registration_retires_names_weights_and_prior_candidate_entries() {
    for failure in 0..4 {
        let input = StorageReadControl::with_limit(1 << 20);
        let model = decoded(&input);
        let key = name(&input);
        let foreign = StorageReadControl::with_limit(65536);
        let mut catalog = ModelCatalog::new(input.memory());
        let full = match failure {
            0 => {
                input.cancellation().cancel();
                None
            }
            1 => Some(input.memory().reserve(input.memory().available()).unwrap()),
            2 => {
                catalog = catalog
                    .insert_image(name(&input), decoded(&input), &input)
                    .unwrap();
                None
            }
            _ => None,
        };
        let control = if failure == 3 { &foreign } else { &input };
        assert!(catalog.insert_image(key, model, control).is_err());
        drop(full);
        assert_eq!(input.memory().used(), 0);
        assert_eq!(foreign.memory().used(), 0);
    }
}
