//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_analysis::{Analyzer, AnalyzerLimits, Tokenizer};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("stored binding cannot read transport bytes")
    }
}

fn binding() -> FieldAnalyzerBinding {
    let owner = AnalyzerResources::new(AnalyzerLimits::default());
    let index = owner.compile(&Analyzer::default()).unwrap();
    let search = owner
        .compile(&Analyzer::new(Tokenizer::Keyword, Vec::new(), Vec::new()))
        .unwrap();
    FieldAnalyzerBinding::unassigned(index.clone(), index).assigned(
        "exact",
        search,
        AnalyzerPhase::Search,
        AnalyzerBindingOwner::Field,
    )
}

fn decoder(
    text: &str,
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> ImageDecoder<Budgeted<FieldAnalyzerBinding>, StorageBackendError> {
    let reservation = memory.reserve(text.len()).unwrap();
    let text = Budgeted::new(text.to_owned(), reservation);
    ImageDecoder::new(memory, token, |mut channel| async move {
        FieldAnalyzerBinding::from_json_with_input(
            &text,
            &AnalyzerResources::new(AnalyzerLimits::default()),
            &mut channel.input(),
        )
        .await
    })
    .unwrap()
}

fn complete(
    input: &mut ImageDecoder<Budgeted<FieldAnalyzerBinding>, StorageBackendError>,
    steps: usize,
) -> StorageBackendResult<Budgeted<FieldAnalyzerBinding>> {
    for _ in 0..100_000 {
        if let Some(value) = input.advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps })? {
            return Ok(value);
        }
    }
    panic!("stored binding input did not finish")
}

fn compare(text: &str) {
    let ordinary =
        FieldAnalyzerBinding::from_json(text, &AnalyzerResources::new(AnalyzerLimits::default()));
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let mut input = decoder(text, &memory, &token);
    let retained = complete(&mut input, 11);
    assert_eq!(
        retained.is_ok(),
        ordinary.is_ok(),
        "source {text}; retained {:?}; ordinary {:?}",
        retained.as_ref().err(),
        ordinary.as_ref().err()
    );
    if let (Ok(retained), Ok(ordinary)) = (&retained, &ordinary) {
        assert_eq!(retained.to_json().unwrap(), ordinary.to_json().unwrap());
        assert_eq!(
            retained.reserved_bytes(),
            size_of::<FieldAnalyzerBinding>()
                + retained.index.name.as_ref().map_or(0, String::capacity)
                + retained.search.name.as_ref().map_or(0, String::capacity)
        );
    }
    drop((input, retained));
    assert_eq!(memory.used(), 0);
}

#[test]
fn stored_binding_input_preserves_closed_struct_forms_and_original_revision_validation() {
    let text = binding().to_json().unwrap();
    compare(&text);
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    for field in [
        "format",
        "version",
        "owner",
        "last_phase",
        "index",
        "search",
    ] {
        let mut absent = value.clone();
        absent.as_object_mut().unwrap().remove(field);
        compare(&absent.to_string());
        for replacement in [
            serde_json::Value::Null,
            true.into(),
            0.into(),
            "wrong".into(),
            serde_json::json!([]),
            serde_json::json!({}),
        ] {
            let mut changed = value.clone();
            changed[field] = replacement;
            compare(&changed.to_string());
        }
        let duplicate = format!("{{\"{field}\":{},{}", value[field], &text[1..]);
        compare(&duplicate);
    }
    let sequence = serde_json::json!([
        value["format"],
        value["version"],
        value["owner"],
        value["last_phase"],
        value["index"],
        value["search"]
    ]);
    compare(&sequence.to_string());
    for size in 0..6 {
        compare(
            &serde_json::Value::Array(sequence.as_array().unwrap()[..size].to_vec()).to_string(),
        );
    }
    let mut nested = value.clone();
    nested["index"] = serde_json::json!([value["index"]["name"], value["index"]["descriptor"]]);
    nested["search"] = serde_json::json!([value["search"]["name"], value["search"]["descriptor"]]);
    compare(&nested.to_string());
    for side in ["index", "search"] {
        for field in ["name", "descriptor"] {
            let mut changed = value.clone();
            changed[side].as_object_mut().unwrap().remove(field);
            compare(&changed.to_string());
            for replacement in [
                serde_json::Value::Null,
                false.into(),
                7.into(),
                "".into(),
                " bad ".into(),
                serde_json::json!({}),
                serde_json::json!([]),
            ] {
                let mut changed = value.clone();
                changed[side][field] = replacement;
                compare(&changed.to_string());
            }
        }
        let mut changed = value.clone();
        changed[side]["unknown"] = true.into();
        compare(&changed.to_string());
    }
    for suffix in ["0", " null", "{}"] {
        compare(&format!("{text}{suffix}"));
    }
    for size in (0..text.len()).step_by(37) {
        compare(&text[..size]);
    }
}

#[test]
fn stored_binding_input_keeps_envelope_errors_before_revision_validation() {
    let mut bad_format: serde_json::Value =
        serde_json::from_str(&binding().to_json().unwrap()).unwrap();
    bad_format["format"] = "wrong".into();
    bad_format["index"]["descriptor"] = false.into();
    let memory = MemoryBudget::new(8 << 20);
    let error = complete(
        &mut decoder(&bad_format.to_string(), &memory, &CancellationToken::new()),
        3,
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "unsupported analyzer binding format");
    assert_eq!(memory.used(), 0);
    // Raw descriptor strings remain opaque until envelope validation has completed.
    for raw in [
        r#""\ud800""#,
        r#"{"duplicate":0,"duplicate":1}"#,
        "null",
        "[]",
    ] {
        let source = format!(
            r#"{{"format":"wrong","version":1,"owner":"field","last_phase":"both","index":{{"name":null,"descriptor":{raw}}},"search":{{"name":null,"descriptor":null}}}}"#
        );
        let expected = FieldAnalyzerBinding::from_json(
            &source,
            &AnalyzerResources::new(AnalyzerLimits::default()),
        )
        .unwrap_err();
        let actual =
            complete(&mut decoder(&source, &memory, &CancellationToken::new()), 3).unwrap_err();
        assert_eq!(actual.to_string(), expected.to_string());
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn stored_binding_input_adopts_names_and_retires_failures_before_returning_allowance() {
    let mut binding = binding();
    binding.search.name = Some("wide한🙂".repeat(8192));
    let text = binding.to_json().unwrap();
    for steps in [1, 5, 64] {
        let memory = MemoryBudget::new(8 << 20);
        let mut input = decoder(&text, &memory, &CancellationToken::new());
        let restored = complete(&mut input, steps).unwrap();
        assert_eq!(restored.search.name, binding.search.name);
        assert_eq!(restored.index.compiled.analyze("A B").unwrap(), ["A", "B"]);
        assert_eq!(restored.search.compiled.analyze("A B").unwrap(), ["A B"]);
        assert_eq!(memory.used(), restored.reserved_bytes());
        drop((input, restored));
        assert_eq!(memory.used(), 0);
    }
    for action in 0..5 {
        let memory = MemoryBudget::new(8 << 20);
        let token = CancellationToken::new();
        let mut input = decoder(&text, &memory, &token);
        for _ in 0..32 {
            assert!(input
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                .unwrap()
                .is_none());
        }
        let mut occupied = None;
        match action {
            0 => {
                token.cancel();
                assert!(input
                    .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                    .is_err());
            }
            1 => assert!(input
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 0 })
                .is_err()),
            2 => {
                occupied = Some(memory.reserve(memory.available()).unwrap());
                assert!(complete(&mut input, 7).is_err());
            }
            3 => (),
            _ => {
                assert!(
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                        let _input = input;
                        panic!("retire unpublished binding");
                    }))
                    .is_err()
                );
                assert_eq!(memory.used(), 0);
                continue;
            }
        }
        drop((input, occupied));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn stored_binding_publication_preserves_native_buffers_both_roots_and_independent_mutation() {
    use crate::{
        analyzer_binding::registry::FieldAnalyzerCatalog, analyzer_catalog::AnalyzerCatalog,
        read_control::StorageReadControl,
    };
    let binding = binding();
    let text = binding.to_json().unwrap();
    let control = StorageReadControl::with_limit(8 << 20);
    let mut input = ImageDecoder::new(
        control.memory(),
        control.cancellation(),
        |mut channel| async move {
            let mut work = channel.input();
            let owner = AnalyzerResources::new(AnalyzerLimits::default());
            let value =
                FieldAnalyzerBinding::from_json_with_input(&text, &owner, &mut work).await?;
            let value_pointer = value.search.name.as_ref().unwrap().as_ptr() as usize;
            let name = work.copy_text("named한🙂").await?;
            let name_pointer = name.as_ptr() as usize;
            let names = AnalyzerCatalog::new(work.memory)
                .insert_restored_from(name, value.search.compiled.clone(), &mut work)
                .await?;
            let table = work.copy_text("public.saved").await?;
            let field = work.copy_text(&"body한🙂".repeat(2048)).await?;
            let field_pointer = field.as_ptr() as usize;
            let (table, mut memory) = table.into_parts();
            let (field, field_memory) = field.into_parts();
            memory.absorb(field_memory);
            let fields = FieldAnalyzerCatalog::new(work.memory)
                .insert_restored_from(Budgeted::new((table, field), memory), value, &mut work)
                .await?;
            Ok::<_, StorageBackendError>((
                names,
                fields,
                (name_pointer, field_pointer, value_pointer),
            ))
        },
    )
    .unwrap();
    let (names, fields, pointers) = loop {
        if let Some(result) = input
            .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 3 })
            .unwrap()
        {
            break result;
        }
    };
    assert_eq!(names.keys().next().unwrap().as_ptr() as usize, pointers.0);
    let (key, value) = fields.iter().next().unwrap();
    assert_eq!(key.1.as_ptr() as usize, pointers.1);
    assert_eq!(
        value.search.name.as_ref().unwrap().as_ptr() as usize,
        pointers.2
    );
    assert!(std::sync::Arc::ptr_eq(
        &names["named한🙂"],
        &value.search.compiled
    ));
    let weak = std::sync::Arc::downgrade(&value.search.compiled);
    let key = key.clone();
    let capture = StorageReadControl::with_limit(8 << 20);
    let captured_names = names.capture(&capture).unwrap();
    let named_image = captured_names.image("named한🙂").unwrap().unwrap();
    let bound = fields.capture(&capture).unwrap();
    let bound_image = bound.binding(&key).unwrap().unwrap();
    let mutations = MemoryBudget::new(1 << 20);
    let mut names = names.into_mutation_budget(&mutations);
    let mut fields = fields.into_mutation_budget(&mutations);
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    assert!(names.remove("named한🙂").unwrap());
    assert!(fields.remove(&key).unwrap());
    assert!(weak.upgrade().is_none());
    drop((names, fields, captured_names, bound, occupied, input));
    let mut output = bound_image.image_reader(&capture).unwrap();
    let mut bytes = Vec::new();
    let mut part = [0; 73];
    loop {
        let count = output.read(&mut part).unwrap();
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&part[..count]);
    }
    let decoded = FieldAnalyzerBinding::read_image(
        &mut std::io::Cursor::new(bytes),
        &AnalyzerResources::new(AnalyzerLimits::default()),
        &capture,
    )
    .unwrap();
    assert_eq!(decoded.to_json().unwrap(), binding.to_json().unwrap());
    drop(output);
    drop((bound_image, named_image, decoded));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(capture.memory().used(), 0);
    assert_eq!(mutations.used(), 0);
}
