//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::events::StoredTrigger;

fn trigger(id: u8) -> StoredTrigger {
    let mut trigger: StoredTrigger = serde_json::from_str(
        r#"{
        "definition": {
            "name":"guard", "table":"public.source", "function":"public.check_row",
            "arguments":[], "constraint":true, "row":true, "timing":"After",
            "events":["Insert"], "update_columns":[], "when":null, "or_replace":false
        }
    }"#,
    )
    .unwrap();
    trigger.object_id = Some(identity(id).object_id);
    trigger.catalog_oid = Some(identity(id).oid);
    trigger.constraint_catalog_oid = Some(identity(id + 10).oid);
    trigger
}

fn check(
    triggers: &[StoredTrigger],
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> Result<(), SQLError> {
    validate(
        std::iter::empty(),
        std::iter::empty(),
        std::iter::empty(),
        triggers.iter(),
        memory,
        token,
    )
}

#[test]
fn native_constraint_triggers_reject_shared_constraint_catalog_identities() {
    for (object, legacy) in [(false, false), (false, true), (true, false), (true, true)] {
        let mut triggers = [trigger(1), trigger(2)];
        let memory = MemoryBudget::new(64 << 10);
        let token = CancellationToken::new();
        check(&triggers, &memory, &token).unwrap();
        if object {
            triggers[1].object_id = triggers[0].object_id;
            if legacy {
                triggers[1].constraint_catalog_oid = None;
            }
        } else {
            triggers[1].constraint_catalog_oid = triggers[0].constraint_catalog_oid;
            if legacy {
                triggers[1].object_id = None;
            }
        }
        assert!(
            check(&triggers, &memory, &token).is_err(),
            "object={object}, legacy={legacy}"
        );
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_constraint_triggers_preserve_legacy_absence_and_separate_catalog_rows() {
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let mut triggers = [trigger(1), trigger(2)];
    triggers[1].catalog_oid = triggers[0].constraint_catalog_oid;
    check(&triggers, &memory, &token).unwrap();
    triggers[1].object_id = triggers[0].object_id;
    triggers[1].constraint_catalog_oid = triggers[0].constraint_catalog_oid;
    triggers[1].definition.constraint = false;
    check(&triggers, &memory, &token).unwrap();
    triggers[1].definition.constraint = true;
    triggers[1].object_id = Some(identity(2).object_id);
    triggers[1].constraint_catalog_oid = None;
    check(&triggers, &memory, &token).unwrap();
    assert_eq!(triggers[1].constraint_catalog_oid, None);
    triggers[1].constraint_catalog_oid = Some(identity(12).oid);
    triggers[1].object_id = None;
    check(&triggers, &memory, &token).unwrap();
    assert_eq!(triggers[1].object_id, None);
    triggers[1].constraint_catalog_oid = None;
    check(&triggers, &memory, &token).unwrap();
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_constraint_trigger_identity_admission_retires_on_failure() {
    let triggers = [trigger(1)];
    let memory = MemoryBudget::new(0);
    assert_eq!(
        check(&triggers, &memory, &CancellationToken::new())
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), 0);
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    token.cancel();
    assert_eq!(
        check(&triggers, &memory, &token).unwrap_err().sqlstate(),
        Some("57014")
    );
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_constraint_triggers_reject_invalid_partial_catalog_identities() {
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    for oid in [0, -1, i64::from(u32::MAX) + 1] {
        let mut value = trigger(1);
        value.object_id = None;
        value.constraint_catalog_oid = Some(oid);
        assert!(check(&[value], &memory, &token).is_err(), "OID {oid}");
        assert_eq!(memory.used(), 0);
    }
    let mut value = trigger(1);
    value.object_id = Some([0; 16]);
    value.constraint_catalog_oid = None;
    assert!(check(&[value], &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
}
