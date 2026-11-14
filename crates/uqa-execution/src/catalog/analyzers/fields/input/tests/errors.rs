//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn alter_value(store: &Arc<dyn KeyValueStore>, value: &str, operation: &str) {
    let key = store
        .scan_prefix(&[])
        .unwrap()
        .into_iter()
        .find(|(_, bytes)| bytes.as_slice() == value.as_bytes())
        .unwrap()
        .0;
    let mut batch = store.batch();
    match operation {
        "delete" => batch.delete(&key).unwrap(),
        "duplicate_label" => {
            let mut key = key;
            key.push(255);
            batch.put(&key, value.as_bytes()).unwrap();
        }
        "corrupt" => batch.put(&key, &[255]).unwrap(),
        _ => panic!("unknown fixture mutation"),
    }
    batch.commit().unwrap();
}

fn failure(
    catalog: &dyn CatalogFacade,
    resources: &Arc<AnalyzerResources>,
    gin: &GinAnalyzerOwners,
    expected: &str,
) {
    let ordinary = match restore(catalog, resources, gin) {
        Ok(_) => panic!("ordinary input accepted corrupt records"),
        Err(error) => error,
    };
    let control = StorageReadControl::with_limit(16 << 20);
    let mut input = input(catalog, resources, gin, &control);
    let actual = match finish(&mut input, 1) {
        Ok(_) => panic!("retained input accepted corrupt records"),
        Err(error) => error,
    };
    if expected == "json" {
        let (StorageBackendError::Serde(reference), StorageBackendError::Json(observed)) =
            (&ordinary, &actual)
        else {
            panic!("original and retained JSON diagnostics differ: {ordinary}; {actual}");
        };
        assert_eq!(observed.to_string(), reference.to_string());
        assert_eq!(observed.position().line, reference.line());
        assert_eq!(observed.position().column, reference.column());
        assert_eq!(observed.is_data(), reference.is_data());
        assert_eq!(observed.is_syntax(), reference.is_syntax());
        assert_eq!(observed.is_eof(), reference.is_eof());
    } else {
        assert_eq!(actual.to_string(), ordinary.to_string());
        assert!(actual.to_string().contains(expected), "{actual}");
    }
    assert!(input.advance(1).is_err());
    drop(input);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn field_analyzer_input_preserves_complete_physical_and_semantic_error_boundaries() {
    let resources = Arc::new(AnalyzerResources::default());
    let original = binding(&resources);
    for (scenario, expected) in [
        ("json", "json"),
        ("table", "relation"),
        ("duplicate", "duplicate durable field analyzer binding"),
        (
            "missing_label",
            "field analyzer binding disagrees with its catalog label",
        ),
        (
            "repeated_label",
            "field analyzer binding disagrees with its catalog label",
        ),
        (
            "wrong_phase",
            "field analyzer binding disagrees with its catalog label",
        ),
        (
            "wrong_name",
            "field analyzer binding disagrees with its catalog label",
        ),
        (
            "gin_owner",
            "GIN definition conflicts with persisted analyzer ownership",
        ),
        (
            "gin_name",
            "GIN definition conflicts with persisted analyzer ownership",
        ),
    ] {
        let catalog = catalog();
        let mut gin = GinAnalyzerOwners::new();
        let binding = if scenario == "gin_name" {
            original.assigned(
                "saved",
                original.index.compiled.clone(),
                AnalyzerPhase::Both,
                AnalyzerBindingOwner::Gin,
            )
        } else {
            original.clone()
        };
        let table = if scenario == "table" { "a.b.c" } else { "docs" };
        save(catalog.as_ref(), table, "body", &binding);
        match scenario {
            "json" => catalog
                .replace_table_field_analyzer_binding(table, "body", "both", "", "{")
                .unwrap(),
            "duplicate" => save(catalog.as_ref(), "public.docs", "body", &binding),
            "missing_label" => alter_value(&catalog.store(), "", "delete"),
            "repeated_label" => alter_value(&catalog.store(), "", "duplicate_label"),
            "wrong_phase" | "wrong_name" => catalog
                .replace_table_field_analyzer_binding(
                    table,
                    "body",
                    if scenario == "wrong_phase" {
                        "BOTH"
                    } else {
                        "both"
                    },
                    "unexpected",
                    &binding.to_json().unwrap(),
                )
                .unwrap(),
            "gin_owner" | "gin_name" => {
                gin.insert(("public.docs".into(), "body".into()), "different".into());
            }
            _ => (),
        }
        failure(catalog.as_ref(), &resources, &gin, expected);
    }
}

#[test]
fn field_analyzer_input_keeps_binding_label_and_gin_first_error_order() {
    let resources = Arc::new(AnalyzerResources::default());
    let original = binding(&resources);
    for (scenario, expected) in [
        ("binding_physical", "invalid utf-8 value"),
        ("binding_semantic", "json"),
        ("label_physical", "invalid utf-8 value"),
        ("label_semantic", "relation"),
        (
            "first_gin",
            "GIN definition conflicts with persisted analyzer ownership",
        ),
    ] {
        let catalog = catalog();
        let mut gin = GinAnalyzerOwners::new();
        save(catalog.as_ref(), "docs", "a", &original);
        save(catalog.as_ref(), "docs", "z", &original);
        if matches!(scenario, "binding_physical" | "binding_semantic") {
            catalog
                .replace_table_field_analyzer_binding("docs", "a", "both", "", "{")
                .unwrap();
        } else {
            gin.insert(("public.docs".into(), "a".into()), "unexpected".into());
        }
        match scenario {
            "binding_physical" => {
                let later = original.to_json().unwrap() + " ";
                catalog
                    .replace_table_field_analyzer_binding("docs", "z", "both", "", &later)
                    .unwrap();
                alter_value(&catalog.store(), &later, "corrupt");
            }
            "binding_semantic" | "label_physical" => {
                catalog
                    .replace_table_field_analyzer_binding(
                        "docs",
                        "z",
                        "both",
                        "later",
                        &original.to_json().unwrap(),
                    )
                    .unwrap();
                alter_value(&catalog.store(), "later", "corrupt");
            }
            "label_semantic" => catalog
                .save_table_field_analyzer("z.z.z", "body", "both", "")
                .unwrap(),
            "first_gin" => catalog
                .replace_table_field_analyzer_binding(
                    "docs",
                    "z",
                    "wrong",
                    "",
                    &original.to_json().unwrap(),
                )
                .unwrap(),
            _ => unreachable!(),
        }
        failure(catalog.as_ref(), &resources, &gin, expected);
    }
}
