//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::routines::{
    catalog::{encoding, FUNCTIONS_METADATA_KEY},
    restoration::{canonicalize_persisted_sql_functions, RoutineRestoreSchemas},
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};
use uqa_core::{image_io::AsyncImageInput, memory::ProductionControl};
use uqa_sql::{
    ast::{FunctionBody, UserTypeIdentity, UserTypeKind},
    catalog::{
        roles::{RoleDefinition, RoleIdentity},
        security::BoundSchemaSecurity,
    },
};

const LIMITS: CatalogInputLimits = CatalogInputLimits {
    maximum_key_bytes: 4096,
    maximum_value_bytes: 1 << 20,
};

#[derive(Clone)]
struct Names {
    schemas: SchemaRegistry,
    calls: Arc<Mutex<Vec<String>>>,
    entered: Arc<AtomicBool>,
    fail: bool,
}
impl Names {
    fn new() -> Self {
        Self {
            schemas: BoundSchemaSecurity::initial_catalog(),
            calls: Arc::default(),
            entered: Arc::default(),
            fail: false,
        }
    }
    fn identity(name: &str) -> Option<UserTypeIdentity> {
        (name == "domain_alias").then_some(UserTypeIdentity {
            kind: UserTypeKind::Domain,
            oid: 42,
            dimensions: 0,
        })
    }
}
impl RoutineRestoreSchemas for Names {
    fn routine_schema_exists(&self, schema: &str) -> bool {
        self.schemas.contains_key(schema)
    }
    fn routine_user_type_identity(&self, name: &str) -> Option<String> {
        Self::identity(name).map(|identity| {
            identity
                .render_with_control(&ProductionControl::uncontrolled())
                .unwrap()
                .into_uncontrolled()
                .unwrap()
        })
    }
}
impl RoutineTypeIdentityInput for Names {
    type Error = StorageBackendError;
    async fn resolve_user_type_identity(
        &mut self,
        name: &str,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<Option<UserTypeIdentity>> {
        control.check()?;
        input.checkpoint().await?;
        control.check()?;
        self.calls.lock().unwrap().push(name.into());
        if self.fail {
            return Err(StorageBackendError::Other(
                "selected type lookup failed".into(),
            ));
        }
        Ok(Self::identity(name))
    }
}
impl RoutineRestoreSchemaInput for Names {
    fn schemas(&self) -> &SchemaRegistry {
        self.entered.store(true, Ordering::Relaxed);
        &self.schemas
    }
}

fn roles() -> RoleDefinitions {
    RoleDefinitions::try_from_iter([("uqa".into(), RoleDefinition::bootstrap())]).unwrap()
}
fn definition(name: &str, id: u8, ty: &str) -> Value {
    let uqa_sql::Statement::CreateFunction(mut value) = uqa_sql::compile(
        "CREATE FUNCTION f(a integer) RETURNS integer AS 'this is uncompiled source' LANGUAGE SQL",
    )
    .unwrap()
    .remove(0) else {
        unreachable!()
    };
    value.name = name.into();
    value.object_id = Some([id; 16]);
    value.owner = Some(RoleIdentity::BOOTSTRAP);
    value.params[0].type_name = ty.into();
    serde_json::to_value(value).unwrap()
}
fn document(definitions: Value) -> String {
    json!({"routine_catalog_format":3,"definitions":definitions}).to_string()
}
fn catalog(text: &str) -> Arc<dyn CatalogFacade> {
    let storage = uqa_storage_sqlite::SQLiteKeyValueStorage::open_in_memory().unwrap();
    let catalog = Arc::new(storage.catalog());
    catalog.set_metadata(FUNCTIONS_METADATA_KEY, text).unwrap();
    catalog
}
fn complete(
    input: &mut RoutineCatalogCanonicalInput,
    steps: usize,
) -> StorageBackendResult<(CanonicalRoutineDefinitions, usize)> {
    for polls in 1..1_000_000 {
        if let Some(value) = input.advance(steps)? {
            return Ok((value, polls));
        }
    }
    panic!("canonical routine input stopped progressing")
}
fn observe(value: &CanonicalRoutineDefinitions) -> Value {
    serde_json::to_value(
        value
            .definitions()
            .map(|(name, values)| {
                (
                    name,
                    values.iter().map(|value| &**value).collect::<Vec<_>>(),
                )
            })
            .collect::<BTreeMap<_, _>>(),
    )
    .unwrap()
}
fn original(text: &str, names: &Names) -> StorageBackendResult<Value> {
    let (definitions, _) = encoding::decode(Some(text), &roles(), false)?;
    let (definitions, migrated) = canonicalize_persisted_sql_functions(names, definitions)?;
    if migrated {
        return Err(StorageBackendError::Other("routine catalog requires an initial-open expression, object-identity or ACL format migration".into()));
    }
    Ok(serde_json::to_value(definitions).unwrap())
}
fn compare(text: &str, names: Names) {
    let expected = original(text, &names);
    for steps in [1, 7, 128] {
        let native = MemoryBudget::new(16 << 20);
        let control = StorageReadControl::with_limit(16 << 20);
        let mut input = RoutineCatalogCanonicalInput::new(
            catalog(text),
            roles(),
            names.clone(),
            LIMITS,
            &native,
            &control,
        )
        .unwrap();
        let result = complete(&mut input, steps);
        match (&expected, &result) {
            (Ok(expected), Ok((actual, _))) => assert_eq!(*expected, observe(actual)),
            (Err(expected), Err(actual)) => {
                assert_eq!(expected.to_string(), actual.to_string(), "{text}");
            }
            _ => panic!(
                "original success {} != retained success {}: {text}",
                expected.is_ok(),
                result.is_ok()
            ),
        }
        drop(result);
        assert!(input.advance(1).is_err());
        drop(input);
        assert_eq!((native.used(), control.memory().used()), (0, 0));
    }
}

#[test]
fn canonical_routine_catalog_input_preserves_alias_groups_type_binding_and_original_validation_order(
) {
    compare(
        &document(
            json!({"f":[definition("public.f",1,"domain_alias")],"public.f":[definition("f",2,"integer")],"public.empty":[]}),
        ),
        Names::new(),
    );
    for definitions in [
        json!({"f":[definition("f",1,"domain_alias")],"public.f":[definition("f",2,"domain#42")]}),
        json!({"f":[definition("f",1,"integer"),definition("f",1,"domain_alias")]}),
        json!({"missing.empty":[],"z":[definition("z",1,"integer")]}),
        json!({"\"unterminated":[]}),
        json!({"f":[definition("other",1,"integer")]}),
        json!({"f":[definition("\"unterminated",1,"integer")]}),
    ] {
        compare(&document(definitions), Names::new());
    }
    let mut first = definition("f", 1, "integer");
    let statements = uqa_sql::compile("SELECT __to_hex_int4(1)").unwrap();
    first["body"] = serde_json::to_value(FunctionBody::Statements(statements)).unwrap();
    compare(&document(json!({"f":[first.clone()]})), Names::new());
    compare(
        &document(json!({"f":[first],"z":[definition("wrong",2,"integer")]})),
        Names::new(),
    );
    let mut invalid = definition("f", 1, "integer");
    invalid["object_id"] = Value::Null;
    compare(&document(json!({"f":[invalid],"z":[{}]})), Names::new());
    compare(
        r#"{"routine_catalog_format":2,"definitions":{"f":[{}]}}"#,
        Names::new(),
    );
}

#[test]
fn canonical_routine_catalog_input_finishes_all_authority_and_typed_fields_before_name_callbacks() {
    let first = definition("f", 1, "domain_alias");
    let mut bad_authority = definition("z", 2, "integer");
    bad_authority.as_object_mut().unwrap().remove("owner");
    for last in [
        bad_authority,
        json!({"owner":RoleIdentity::BOOTSTRAP,"execute_acl":null}),
        definition("z", 0, "integer"),
    ] {
        let text = document(json!({"f":[first.clone()],"z":[last]}));
        let names = Names::new();
        compare(&text, names.clone());
        assert!(names.calls.lock().unwrap().is_empty());
    }
    let text = document(json!({"f":[definition("f",1,"domain_alias")]}));
    let native = MemoryBudget::new(1 << 20);
    let control = StorageReadControl::with_limit(1 << 20);
    let mut names = Names::new();
    names.fail = true;
    let error = complete(
        &mut RoutineCatalogCanonicalInput::new(
            catalog(&text),
            roles(),
            names,
            LIMITS,
            &native,
            &control,
        )
        .unwrap(),
        1,
    )
    .err()
    .unwrap();
    assert_eq!(error.to_string(), "selected type lookup failed");
    assert_eq!((native.used(), control.memory().used()), (0, 0));
}

#[test]
fn canonical_routine_catalog_input_retains_selected_source_and_independent_native_output() {
    let text = document(json!({"f":[definition("f",1,"domain_alias")]}));
    let native = MemoryBudget::new(8 << 20);
    let control = StorageReadControl::with_limit(8 << 20);
    let catalog = catalog(&text);
    let expected = original(&text, &Names::new()).unwrap();
    let mut input = RoutineCatalogCanonicalInput::new(
        Arc::clone(&catalog),
        roles(),
        Names::new(),
        LIMITS,
        &native,
        &control,
    )
    .unwrap();
    assert!(input.advance(1).unwrap().is_none());
    assert!(input.advance(1).unwrap().is_none());
    catalog
        .set_metadata(FUNCTIONS_METADATA_KEY, "invalid later text")
        .unwrap();
    drop(catalog);
    let output = complete(&mut input, 1).unwrap().0;
    assert_eq!(observe(&output), expected);
    assert!(native.used() > 0);
    assert_eq!(control.memory().used(), 0);
    control.cancellation().cancel();
    drop(input);
    assert_eq!(observe(&output), expected);
    drop(output);
    assert_eq!(native.used(), 0);
}

#[test]
fn canonical_routine_catalog_input_retires_candidates_at_each_pending_schema_and_type_transition() {
    let text = document(
        json!({"f":[definition("f",1,"domain_alias")],"public.f":[definition("f",2,"integer")]}),
    );
    let catalog = catalog(&text);
    let native = MemoryBudget::new(8 << 20);
    let baseline = StorageReadControl::with_limit(8 << 20);
    let names = Names::new();
    let entered = Arc::clone(&names.entered);
    let mut input = RoutineCatalogCanonicalInput::new(
        Arc::clone(&catalog),
        roles(),
        names,
        LIMITS,
        &native,
        &baseline,
    )
    .unwrap();
    let mut first_schema = None;
    let mut polls = 0;
    let value = loop {
        polls += 1;
        let value = input.advance(1).unwrap();
        if entered.load(Ordering::Relaxed) && first_schema.is_none() {
            first_schema = Some(polls);
        }
        if let Some(value) = value {
            break value;
        }
    };
    drop((value, input));
    let first_schema = first_schema.unwrap();
    let mut boundaries = vec![0, 1, 2, first_schema - 1];
    boundaries.extend(first_schema..polls - 1);
    let mut native_denials = 0;
    let mut work_denials = 0;
    for pending in boundaries {
        for fail in ["cancel", "drop", "native memory", "work memory", "window"] {
            let control = StorageReadControl::with_limit(8 << 20);
            let mut input = RoutineCatalogCanonicalInput::new(
                Arc::clone(&catalog),
                roles(),
                Names::new(),
                LIMITS,
                &native,
                &control,
            )
            .unwrap();
            for _ in 0..pending {
                assert!(input.advance(1).unwrap().is_none());
            }
            let hold = match fail {
                "native memory" => Some(native.reserve(native.available()).unwrap()),
                "work memory" => Some(
                    control
                        .memory()
                        .reserve(control.memory().available())
                        .unwrap(),
                ),
                _ => None,
            };
            if fail == "cancel" {
                control.cancellation().cancel();
            }
            if fail != "drop" {
                let result = complete(&mut input, usize::from(fail != "window"));
                if matches!(fail, "cancel" | "window") {
                    assert!(result.is_err());
                }
                native_denials += usize::from(fail == "native memory" && result.is_err());
                work_denials += usize::from(fail == "work memory" && result.is_err());
                drop(result);
            }
            drop((input, hold));
            assert_eq!((native.used(), control.memory().used()), (0, 0));
        }
    }
    assert!(native_denials > 0 && work_denials > 0);
    assert_eq!(
        catalog
            .get_metadata(FUNCTIONS_METADATA_KEY)
            .unwrap()
            .as_deref(),
        Some(text.as_str())
    );
}

#[test]
fn canonical_routine_catalog_input_bounds_schema_comparison_for_empty_overloads() {
    let schema = format!("{}selected", "가".repeat(8193));
    let text = document(json!({format!("\"{schema}\".empty"):[]}));
    for present in [false, true] {
        let mut names = Names::new();
        let registered = if present {
            schema.clone()
        } else {
            format!("{schema}_other")
        };
        names
            .schemas
            .insert(
                registered.clone(),
                BoundSchemaSecurity::bootstrap(&registered),
            )
            .unwrap();
        let expected = original(&text, &names);
        let native = MemoryBudget::new(16 << 20);
        let control = StorageReadControl::with_limit(16 << 20);
        let entered = Arc::clone(&names.entered);
        let mut input = RoutineCatalogCanonicalInput::new(
            catalog(&text),
            roles(),
            names.clone(),
            LIMITS,
            &native,
            &control,
        )
        .unwrap();
        names.schemas.clear();
        drop(names);
        let mut schema_polls = 0;
        let result = loop {
            if entered.load(Ordering::Relaxed) {
                schema_polls += 1;
            }
            match input.advance(1) {
                Ok(None) => {}
                result => break result,
            }
        };
        assert!(schema_polls >= schema.len() / 4096);
        match (expected, &result) {
            (Ok(expected), Ok(Some(actual))) => {
                assert_eq!(observe(actual), expected);
                assert_eq!(actual.definitions().count(), 0);
            }
            (Err(expected), Err(actual)) => assert_eq!(expected.to_string(), actual.to_string()),
            _ => panic!("empty overload schema validation changed"),
        }
        assert!(input.advance(1).is_err());
        drop((result, input));
        assert_eq!((native.used(), control.memory().used()), (0, 0));
    }
}
