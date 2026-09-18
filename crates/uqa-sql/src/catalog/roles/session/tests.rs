//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::roles::{identity::RoleReference, RoleDefinition};

fn binding(name: &str, oid: u32) -> Arc<RoleBinding> {
    Arc::new(RoleBinding {
        name: name.into(),
        oid,
        object_id: [u8::try_from(oid).unwrap(); 16],
    })
}

#[test]
fn independent_parameter_restore_commutes_and_keeps_selected_identity() {
    let authenticated = binding("authenticated", 1);
    let session = binding("session", 2);
    let selected = binding("selected", 3);
    for restore_role_first in [false, true] {
        let mut state = SessionAuthorization::new(authenticated.as_ref().clone());
        state.set_session(session.clone());
        state.set_role(Some(selected.clone()));
        if restore_role_first {
            state.set_role(Some(selected.clone()));
            state.restore_session(authenticated.clone());
        } else {
            state.restore_session(authenticated.clone());
            state.set_role(Some(selected.clone()));
        }
        assert!(Arc::ptr_eq(state.current(), &selected));
        assert!(Arc::ptr_eq(state.session(), &authenticated));
        state.set_role(None);
        assert!(Arc::ptr_eq(state.current(), &authenticated));
        assert_eq!(state.show_role(), "none");
    }
}

#[test]
fn routine_effective_identity_is_separate_from_role_setting_and_session() {
    let mut state = SessionAuthorization::default();
    let selected = binding("selected", 2);
    let definer = binding("definer", 3);
    state.set_role(Some(selected.clone()));
    let saved = state.clone();
    state.set_effective(definer.clone());
    assert!(Arc::ptr_eq(state.current(), &definer));
    assert_eq!(state.show_role(), "selected");
    assert_eq!(state.session().name, "uqa");
    state = saved;
    assert!(Arc::ptr_eq(state.current(), &selected));
    state.discard();
    assert!(Arc::ptr_eq(state.current(), state.authenticated()));
    assert_eq!(state.show_role(), "none");
}

#[test]
fn serialized_plan_authority_retains_the_original_incarnation() {
    assert_eq!(
        serde_json::from_str::<RoleReference>(r#""uqa""#).unwrap(),
        RoleReference::Named("uqa".into())
    );
    let original = RoleDefinition::bootstrap();
    let reference =
        RoleReference::Bound(Arc::new(RoleBinding::from_definition(&original).unwrap()));
    let encoded = serde_json::to_vec(&reference).unwrap();
    let restored: RoleReference = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(restored, reference);
    let mut replacement = original.clone();
    replacement.object_id = [42; 16];
    let roles = std::collections::BTreeMap::from([(replacement.name.clone(), replacement)]);
    assert_eq!(
        restored.require_name(&roles).unwrap_err().sqlstate(),
        Some("42704")
    );
    let roles = std::collections::BTreeMap::from([(original.name.clone(), original)]);
    assert_eq!(restored.require_name(&roles).unwrap(), "uqa");
}
