//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{
    cache::RegtypeOutputCache,
    test_support::{type_names as fixtures, CatalogServices},
    CatalogReadView,
};
use uqa_core::memory::RetainedValue;

struct Original {
    catalog: CatalogReadView,
    services: CatalogServices,
}
impl RoutineRestoreSchemas for Original {
    fn routine_schema_exists(&self, schema: &str) -> bool {
        self.catalog
            .snapshot()
            .definitions
            .schemas
            .contains_key(schema)
    }
    fn routine_user_type_identity(&self, name: &str) -> Option<String> {
        let cache = RegtypeOutputCache::default();
        crate::catalog::projection::resolve_catalog_column_type(
            &self.services.context(&self.catalog, &cache),
            name,
        )
        .and_then(|ty| ty.user_type_identity())
    }
}

fn expected(text: &str, catalog: CatalogReadView) -> Value {
    let original = Original {
        catalog,
        services: CatalogServices::default(),
    };
    let (definitions, _) = encoding::decode(Some(text), &roles(), false).unwrap();
    let (definitions, migrated) =
        canonicalize_persisted_sql_functions(&original, definitions).unwrap();
    assert!(!migrated);
    serde_json::to_value(definitions).unwrap()
}
fn selected(
    catalog: CatalogReadView,
    native: &MemoryBudget,
    control: &StorageReadControl,
) -> RoutineCatalogTypeNames {
    let resolution = CatalogServices::default()
        .resolution
        .copy_with_control(&ProductionControl::new(
            native,
            control.cancellation(),
            control.cancellation(),
        ))
        .unwrap();
    RoutineCatalogTypeNames::new(catalog, resolution)
}

#[test]
fn canonical_routine_input_uses_selected_native_catalog_types_after_source_replacement() {
    let mut definitions = serde_json::Map::new();
    for (index, ty) in [
        "d",
        "_e[][]",
        "c",
        "r",
        "m",
        "t",
        "view_array",
        "foreign_array[]",
        "integer",
        "domain#999",
        "missing",
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("f{index}");
        definitions.insert(
            name.clone(),
            json!([definition(&name, u8::try_from(index + 1).unwrap(), ty)]),
        );
    }
    let text = document(Value::Object(definitions));
    let view = fixtures::catalog();
    let expected = expected(&text, view.clone());
    let source = catalog(&text);
    for steps in [1, 7, 128] {
        source.set_metadata(FUNCTIONS_METADATA_KEY, &text).unwrap();
        let native = MemoryBudget::new(16 << 20);
        let control = StorageReadControl::with_limit(16 << 20);
        let names = selected(view.clone(), &native, &control);
        let mut input = RoutineCatalogCanonicalInput::new(
            Arc::clone(&source),
            roles(),
            names,
            LIMITS,
            &native,
            &control,
        )
        .unwrap();
        assert!(input.advance(1).unwrap().is_none());
        assert!(input.advance(1).unwrap().is_none());
        source
            .set_metadata(FUNCTIONS_METADATA_KEY, "invalid later generation")
            .unwrap();
        let mut replacement = view.snapshot().clone();
        RetainedValue::make_mut(&mut replacement.definitions.domains).clear();
        RetainedValue::make_mut(&mut replacement.definitions.schemas).clear();
        drop(replacement);
        let actual = complete(&mut input, steps).unwrap().0;
        assert_eq!(observe(&actual), expected);
        assert_eq!(control.memory().used(), 0);
        assert!(native.used() > 0);
        control.cancellation().cancel();
        drop(input);
        assert_eq!(observe(&actual), expected);
        drop(actual);
        assert_eq!(native.used(), 0);
    }
}
