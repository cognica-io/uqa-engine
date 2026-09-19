//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Construct historical routine catalogs without corrupting current role incarnations.

pub(super) fn remove_routine_identity_fields(value: &mut serde_json::Value) -> usize {
    if value.get("routine_catalog_format").is_some() {
        downgrade_bootstrap_authority(value);
    }
    remove_identity_fields(value)
}

fn downgrade_bootstrap_authority(value: &mut serde_json::Value) {
    use uqa_core::{catalog_acl::AclGrantee, catalog_role::RoleIdentity};
    assert_eq!(value["routine_catalog_format"], 2);
    value["routine_catalog_format"] = 1.into();
    for overloads in value["definitions"].as_object_mut().unwrap().values_mut() {
        for definition in overloads.as_array_mut().unwrap() {
            assert_eq!(
                definition["owner"],
                serde_json::to_value(RoleIdentity::BOOTSTRAP).unwrap()
            );
            definition["owner"] = "uqa".into();
            if let Some(acl) = definition["execute_acl"].as_array_mut() {
                for entry in acl {
                    let role: Option<RoleIdentity> =
                        serde_json::from_value(entry["role"].clone()).unwrap();
                    if let Some(role) = role {
                        assert_eq!(role, RoleIdentity::BOOTSTRAP);
                    }
                    entry["role"] = serde_json::to_value(if role.is_some() {
                        AclGrantee::Role("uqa".into())
                    } else {
                        AclGrantee::Public
                    })
                    .unwrap();
                    assert_eq!(
                        entry["grantor"],
                        serde_json::to_value(RoleIdentity::BOOTSTRAP).unwrap()
                    );
                    entry["grantor"] = "uqa".into();
                }
            }
        }
    }
}

fn remove_identity_fields(value: &mut serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(fields) => {
            let mut removed = usize::from(fields.remove("object_id").is_some());
            removed += usize::from(fields.remove("function_object_id").is_some());
            for value in fields.values_mut() {
                removed += remove_identity_fields(value);
            }
            removed
        }
        serde_json::Value::Array(values) => values.iter_mut().map(remove_identity_fields).sum(),
        _ => 0,
    }
}

pub(super) fn remove_function_binding_identities(value: &mut serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(fields) => {
            let is_binding = fields.contains_key("name")
                && fields.contains_key("argument_types")
                && fields.contains_key("builtin");
            let mut removed = usize::from(is_binding && fields.remove("object_id").is_some());
            for value in fields.values_mut() {
                removed += remove_function_binding_identities(value);
            }
            removed
        }
        serde_json::Value::Array(values) => values
            .iter_mut()
            .map(remove_function_binding_identities)
            .sum(),
        _ => 0,
    }
}

pub(super) fn remove_expression_function_bindings(value: &mut serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(fields) => {
            let mut removed = fields
                .get_mut("Func")
                .and_then(serde_json::Value::as_object_mut)
                .map_or(0, |function| {
                    usize::from(function.remove("binding").is_some())
                });
            for value in fields.values_mut() {
                removed += remove_expression_function_bindings(value);
            }
            removed
        }
        serde_json::Value::Array(values) => values
            .iter_mut()
            .map(remove_expression_function_bindings)
            .sum(),
        _ => 0,
    }
}
