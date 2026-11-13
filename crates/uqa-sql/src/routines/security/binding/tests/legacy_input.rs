//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::work::inline;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{MemoryBudget, Produced, ProductionControl},
    CancellationToken,
};

fn acl() -> Vec<LegacyRoutineAclEntry> {
    serde_json::from_str(r#"[{"role":"delegate","grant_option":true},{"role":"recipient","grantor":"delegate","grant_option":false},{"role":"PUBLIC","grant_option":false}]"#).unwrap()
}

#[test]
fn native_legacy_routine_binding_preserves_identity_grant_order_and_owner_formats() {
    let roles = roles();
    for implicit in [false, true] {
        for acl in [None, Some(Vec::new()), Some(acl())] {
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&memory, &token, &token);
            let value = inline(super::super::legacy::bind_from(
                "owner",
                acl.as_deref(),
                &roles,
                implicit,
                &control,
                &mut (),
            ))
            .unwrap();
            assert_eq!(value.owner, roles["owner"].identity());
            match (&acl, &value.execute_acl) {
                (None, None) => (),
                (Some(source), Some(actual)) => {
                    assert_eq!(actual.len(), source.len() + usize::from(implicit));
                    for (entry, source) in actual.iter().zip(source) {
                        assert_eq!(
                            entry.role,
                            source.role.role_name().map(|name| roles[name].identity())
                        );
                        assert_eq!(
                            entry.grantor,
                            roles[source.grantor.as_deref().unwrap_or("owner")].identity()
                        );
                        assert_eq!(entry.grant_option, source.grant_option);
                    }
                    if implicit {
                        let last = actual.last().unwrap();
                        assert_eq!(last.role, Some(value.owner));
                        assert_eq!(last.grantor, value.owner);
                        assert!(!last.grant_option);
                    }
                }
                _ => panic!("absent ACL changed"),
            }
            drop(value);
            assert_eq!(memory.used(), 0);
        }
    }
    for (owner, entry, expected) in [
        (
            "missing_owner",
            r#"{"role":"missing_recipient","grantor":"missing_grantor","grant_option":false}"#,
            "role \"missing_owner\" does not exist",
        ),
        (
            "owner",
            r#"{"role":"missing_recipient","grantor":"missing_grantor","grant_option":false}"#,
            "role \"missing_recipient\" does not exist",
        ),
        (
            "owner",
            r#"{"role":"PUBLIC","grantor":"missing_grantor","grant_option":true}"#,
            "role \"missing_grantor\" does not exist",
        ),
        (
            "owner",
            r#"{"role":"PUBLIC","grant_option":true}"#,
            "internal error: invalid routine authority: PUBLIC cannot retain a grant option",
        ),
    ] {
        let acl = serde_json::from_str::<LegacyRoutineAclEntry>(entry).unwrap();
        let error = bind_legacy_authority(owner, Some(vec![acl]), &roles, false)
            .err()
            .unwrap();
        assert_eq!(error.to_string(), expected);
    }
}

type Decoder = ImageDecoder<Produced<BoundRoutineAuthority>, SQLError>;

#[test]
fn native_legacy_routine_binding_admits_complete_grants_and_retires_denied_validation() {
    let roles = roles();
    let acl = acl();
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(1 << 20);
    let value = inline(super::super::legacy::bind_from(
        "owner",
        Some(&acl),
        &roles,
        true,
        &ProductionControl::new(&memory, &token, &token),
        &mut (),
    ))
    .unwrap();
    let output_bytes = memory.used();
    let peak = memory.peak();
    assert!(peak > output_bytes);
    let expected = value.execute_acl.clone();
    drop(value);
    assert_eq!(memory.used(), 0);
    for limit in [0, 1, output_bytes, peak - 1, peak] {
        let memory = MemoryBudget::new(limit);
        let result = inline(super::super::legacy::bind_from(
            "owner",
            Some(&acl),
            &roles,
            true,
            &ProductionControl::new(&memory, &token, &token),
            &mut (),
        ));
        if limit < peak {
            assert_eq!(result.err().unwrap().sqlstate(), Some("53200"));
        } else {
            assert_eq!(result.as_ref().unwrap().execute_acl, expected);
            drop(result);
        }
        assert!(memory.peak() <= limit);
        assert_eq!(memory.used(), 0);
    }
}

fn input(
    owner: String,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Decoder {
    let roles = roles();
    let acl = acl();
    let retained = memory.clone();
    let original = original.clone();
    ImageDecoder::new(memory, invoking, |mut reader| async move {
        let mut work = reader.input();
        let control = ProductionControl::new(&retained, &original, work.cancellation);
        bind_legacy_authority_from_input(&owner, Some(&acl), &roles, false, &control, &mut work)
            .await
    })
    .unwrap()
}

fn advance(
    input: &mut Decoder,
    steps: usize,
) -> Result<Option<Produced<BoundRoutineAuthority>>, SQLError> {
    input.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
}

#[test]
fn native_legacy_routine_binding_retains_cancelled_work_and_owned_missing_role_diagnostics() {
    let token = CancellationToken::new();
    let mut baseline = 0;
    for steps in [1, 7, 64] {
        let memory = MemoryBudget::new(1 << 20);
        let mut decoder = input("owner".into(), &memory, &token, &token);
        for poll in 1..1000 {
            if let Some(output) = advance(&mut decoder, steps).unwrap() {
                assert_eq!(output.execute_acl.as_ref().unwrap().len(), 3);
                drop(output);
                if steps == 1 {
                    baseline = poll;
                }
                break;
            }
        }
        assert!(advance(&mut decoder, 1).is_err());
        drop(decoder);
        assert_eq!(memory.used(), 0);
    }
    assert!(baseline > 10);
    for boundary in [0, 1, baseline / 2, baseline - 1] {
        for original_cancel in [false, true] {
            let memory = MemoryBudget::new(1 << 20);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut decoder = input("owner".into(), &memory, &original, &invoking);
            for _ in 0..boundary {
                assert!(advance(&mut decoder, 1).unwrap().is_none());
            }
            if original_cancel {
                original.cancel();
            } else {
                invoking.cancel();
            }
            assert!(matches!(
                advance(&mut decoder, 1),
                Err(SQLError::Cancelled(_))
            ));
            drop(decoder);
            assert_eq!(memory.used(), 0);
        }
    }
    let owner = "missing한🙂".repeat(8192);
    let memory = MemoryBudget::new(1 << 20);
    let mut decoder = input(owner.clone(), &memory, &token, &token);
    let mut failure = None;
    for _ in 0..1000 {
        match advance(&mut decoder, 1) {
            Ok(None) => (),
            Err(error) => {
                failure = Some(error);
                break;
            }
            Ok(Some(_)) => panic!("missing role became bound"),
        }
    }
    drop(decoder);
    let error = failure.unwrap();
    assert_eq!(error.sqlstate(), Some("42704"));
    assert_eq!(
        error.to_string(),
        format!("role \"{owner}\" does not exist")
    );
    assert!(memory.used() >= owner.len());
    let cloned = error.clone();
    drop(error);
    assert!(memory.used() >= owner.len());
    drop(cloned);
    assert_eq!(memory.used(), 0);
}
