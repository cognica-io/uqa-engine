//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn roles() -> BTreeMap<String, RoleDefinition> {
    BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())])
}

fn custom() -> ForeignWrapperDefinition {
    ForeignWrapperDefinition {
        name: "custom".into(),
        identity: ForeignWrapperReference {
            oid: 16_384,
            object_id: [1; 16],
        },
        owner: RoleIdentity::BOOTSTRAP,
        handler: ForeignWrapperHandler::None,
        validator: None,
        options: vec![("z".into(), "first".into()), ("a".into(), "second".into())],
    }
}

#[test]
fn native_implementations_and_ordered_options_have_durable_identities() {
    let mut wrappers = native_wrappers();
    wrappers.insert("custom".into(), custom());
    validate_wrappers(&wrappers, &roles()).unwrap();
    let serialized = serde_json::to_string(&wrappers).unwrap();
    assert_eq!(
        serde_json::from_str::<ForeignWrappers>(&serialized).unwrap(),
        wrappers
    );
    for native in [
        NativeForeignWrapper::Memory,
        NativeForeignWrapper::DuckDB,
        NativeForeignWrapper::Arrow,
    ] {
        let wrapper = bound_wrapper(&wrappers, native.name(), native.reference()).unwrap();
        assert_eq!(wrapper.handler, ForeignWrapperHandler::Native(native));
    }
    assert_eq!(wrappers["custom"].options[0].0, "z");
}

#[test]
fn a_recreated_name_never_retargets_a_stored_wrapper_reference() {
    let original = custom();
    let mut replacement = original.clone();
    replacement.identity.object_id = [2; 16];
    let wrappers = BTreeMap::from([("custom".into(), replacement)]);
    assert!(bound_wrapper(&wrappers, "custom", original.identity).is_err());
    assert!(bound_wrapper(&wrappers, "missing", original.identity).is_err());
}

#[test]
fn restoration_rejects_aliases_invalid_options_and_replaced_owners() {
    let original = custom();
    let mut variants = Vec::new();
    let mut bad = original.clone();
    bad.identity.oid = 1;
    variants.push(bad);
    let mut bad = original.clone();
    bad.identity.object_id = [0; 16];
    variants.push(bad);
    let mut bad = original.clone();
    bad.owner.object_id = [7; 16];
    variants.push(bad);
    let mut bad = original.clone();
    bad.options.push(("a".into(), "duplicate".into()));
    variants.push(bad);
    let mut bad = original.clone();
    bad.options.push(("a=b".into(), "ambiguous".into()));
    variants.push(bad);
    for bad in variants {
        assert!(validate_wrappers(&BTreeMap::from([("custom".into(), bad)]), &roles()).is_err());
    }
    let mut alias = original.clone();
    alias.name = "alias".into();
    assert!(validate_wrappers(
        &BTreeMap::from([("custom".into(), original), ("alias".into(), alias)]),
        &roles()
    )
    .is_err());
    let mut native = native_wrappers();
    native.get_mut("memory_fdw").unwrap().identity.object_id[0] ^= 1;
    assert!(validate_wrappers(&native, &roles()).is_err());
}

#[test]
fn missing_functions_are_retained_but_live_oid_conflicts_are_corruption() {
    let crate::ast::Statement::CreateFunction(mut definition) = crate::compile(
        "CREATE FUNCTION callback(text[],oid) RETURNS integer LANGUAGE SQL RETURN 1",
    )
    .unwrap()
    .remove(0) else {
        panic!("function");
    };
    definition.object_id = Some([5; 16]);
    definition.catalog_oid = Some(20_000);
    let binding = crate::ast::FunctionBinding {
        name: definition.name.clone(),
        object_id: definition.object_id,
        argument_types: crate::routines::routine_signature_types(&definition),
        builtin: false,
        dispatch: None,
        invocation: None,
        composite_field: None,
        resolution_error: None,
    };
    let routines = BTreeMap::from([(
        definition.name.clone(),
        vec![std::sync::Arc::new(crate::routines::SQLUserFunction::new(
            *definition,
            crate::routines::RoutineBody::Source,
        ))],
    )]);
    let mut wrapper = custom();
    wrapper.validator = Some(ForeignWrapperFunction {
        oid: 20_000,
        binding,
    });
    let mut wrappers = BTreeMap::from([("custom".into(), wrapper)]);
    validate_functions(&wrappers, &routines).unwrap();
    validate_functions(&wrappers, &BTreeMap::new()).unwrap();
    wrappers
        .get_mut("custom")
        .unwrap()
        .validator
        .as_mut()
        .unwrap()
        .oid += 1;
    assert!(validate_functions(&wrappers, &routines).is_err());
}
