//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::analyzers::gin::RetainedGinAnalyzerOwners;

fn owners(control: &StorageReadControl, name: &'static str) -> RetainedGinAnalyzerOwners {
    let mut decoder = ImageDecoder::new(
        control.memory(),
        control.cancellation(),
        |mut reader| async move {
            let mut input = reader.input();
            let (table, mut memory) = input.copy_text("public.docs").await?.into_parts();
            let (field, field_memory) = input.copy_text("body").await?.into_parts();
            memory.absorb(field_memory);
            let analyzer = input.copy_text(name).await?;
            let mut owners = RetainedGinAnalyzerOwners::new();
            owners
                .insert_from(Budgeted::new((table, field), memory), analyzer, &mut input)
                .await?;
            Ok::<_, StorageBackendError>(owners)
        },
    )
    .unwrap();
    loop {
        if let Some(owners) = decoder
            .advance(
                &mut std::io::empty(),
                ImageDecodeWindow { bytes: 1, steps: 1 },
            )
            .unwrap()
        {
            return owners;
        }
    }
}

#[test]
fn field_input_consumes_native_gin_owners_without_cloning_or_inheriting_source_controls() {
    for scenario in ["valid", "label", "owner", "name", "cancel", "drop", "zero"] {
        let resources = Arc::new(AnalyzerResources::default());
        let original = binding(&resources);
        let catalog = catalog();
        let binding = original.assigned(
            "saved",
            original.index.compiled.clone(),
            AnalyzerPhase::Both,
            if scenario == "owner" {
                AnalyzerBindingOwner::Field
            } else {
                AnalyzerBindingOwner::Gin
            },
        );
        save(catalog.as_ref(), "docs", "body", &binding);
        if scenario == "label" {
            catalog
                .replace_table_field_analyzer_binding(
                    "docs",
                    "body",
                    "both",
                    "wrong",
                    &binding.to_json().unwrap(),
                )
                .unwrap();
        }
        let selected_name = if matches!(scenario, "name" | "label") {
            "different"
        } else {
            "saved"
        };
        let expected = restore(
            catalog.as_ref(),
            &resources,
            &GinAnalyzerOwners::from([(
                ("public.docs".into(), "body".into()),
                selected_name.into(),
            )]),
        );
        let source = StorageReadControl::with_limit(1 << 20);
        let gin = owners(&source, selected_name);
        let source_bytes = source.memory().used();
        let occupied = source
            .memory()
            .reserve(source.memory().available())
            .unwrap();
        source.cancellation().cancel();
        let invoking = StorageReadControl::with_limit(16 << 20);
        let mut input =
            FieldAnalyzerCatalogInput::new(catalog.as_ref(), resources, LIMITS, gin, &invoking)
                .unwrap();
        assert!(input.advance(1).unwrap().is_none());
        assert_eq!(source.memory().used(), source_bytes + occupied.bytes());
        if scenario == "cancel" {
            invoking.cancellation().cancel();
        }
        if scenario != "drop" {
            let actual = finish(&mut input, usize::from(scenario != "zero"));
            match (scenario, expected, actual) {
                ("cancel" | "zero", _, Err(_)) => (),
                (_, Err(expected), Err(actual)) => {
                    assert_eq!(actual.to_string(), expected.to_string());
                }
                ("valid", Ok(expected), Ok((mut actual, _))) => {
                    let (key, binding) = actual
                        .take_binding(&("public.docs".into(), "body".into()))
                        .unwrap();
                    assert_eq!(
                        binding.to_json().unwrap(),
                        expected.bindings[&*key].to_json().unwrap()
                    );
                    actual.finish().unwrap();
                }
                _ => panic!("unexpected field input outcome for {scenario}"),
            }
            assert!(input.advance(1).is_err());
            assert_eq!(source.memory().used(), occupied.bytes());
        }
        drop((input, occupied));
        assert_eq!(source.memory().used(), 0);
        assert_eq!(invoking.memory().used(), 0);
    }
}
