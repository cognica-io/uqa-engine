//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

const TEXT: &str = r#"{"f":[{"owner":"owner","execute_acl":[{"role":"delegate","grant_option":true},{"role":"PUBLIC","grantor":"delegate","grant_option":false}]}],"empty":[]}"#;

#[test]
fn routine_catalog_authority_retains_selected_roles_and_retires_interrupted_work() {
    let token = CancellationToken::new();
    let mut baseline = 0;
    for steps in [1, 7, 64] {
        let source = MemoryBudget::new(1 << 20);
        let memory = MemoryBudget::new(1 << 20);
        let mut current = roles();
        let selected = current.clone();
        let expected = reference(TEXT, &selected).unwrap();
        let mut input = decoder(
            document(TEXT, &source, &token),
            selected,
            &memory,
            &token,
            &token,
        )
        .unwrap();
        assert!(advance(&mut input, 1).unwrap().is_none());
        let mut replacement = current["owner"].clone();
        replacement.oid += 100;
        replacement.object_id = [9; 16];
        current.insert("owner".into(), replacement).unwrap();
        current.remove("delegate").unwrap();
        drop(current);
        let (output, polls) = complete(&mut input, steps).unwrap();
        equal(&output, &expected);
        assert_eq!(output.catalog().definitions().next().unwrap().0, "empty");
        assert!(advance(&mut input, 1).is_err());
        if steps == 1 {
            baseline = polls + 1;
        }
        drop(input);
        assert!(source.used() > 0 && memory.used() > 0);
        drop(output);
        assert_eq!(source.used(), 0);
        assert_eq!(memory.used(), 0);
    }
    assert!(baseline > 10);
    for boundary in [0, 1, baseline / 2, baseline - 1] {
        for action in 0..4 {
            let source = MemoryBudget::new(1 << 20);
            let memory = MemoryBudget::new(1 << 20);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut input = decoder(
                document(TEXT, &source, &original),
                roles(),
                &memory,
                &original,
                &invoking,
            )
            .unwrap();
            for _ in 0..boundary {
                assert!(advance(&mut input, 1).unwrap().is_none());
            }
            match action {
                0 => (),
                1 | 2 => {
                    if action == 1 {
                        original.cancel();
                    } else {
                        invoking.cancel();
                    }
                    assert!(matches!(
                        advance(&mut input, 1),
                        Err(StorageBackendError::Cancelled(_))
                    ));
                    assert!(advance(&mut input, 1).is_err());
                }
                3 => assert!(advance(&mut input, 0).is_err()),
                _ => unreachable!(),
            }
            drop(input);
            assert_eq!(source.used(), 0);
            assert_eq!(memory.used(), 0);
        }
    }
    check_admission(&token);
}

fn check_admission(token: &CancellationToken) {
    let mut rejected = 0;
    let mut accepted = 0;
    for limit in [0, 128, 512, 1024, 4096, 16_384, 1 << 20] {
        let source = MemoryBudget::new(1 << 20);
        let memory = MemoryBudget::new(limit);
        let result = decoder(
            document(TEXT, &source, token),
            roles(),
            &memory,
            token,
            token,
        )
        .and_then(|mut input| complete(&mut input, 1).map(|(output, _)| output));
        match &result {
            Ok(output) => {
                equal(output, &reference(TEXT, &roles()).unwrap());
                accepted += 1;
            }
            Err(StorageBackendError::Memory(_)) => rejected += 1,
            Err(error) => panic!("limit {limit}: {error}"),
        }
        drop(result);
        assert!(memory.peak() <= limit);
        assert_eq!(source.used(), 0);
        assert_eq!(memory.used(), 0);
    }
    assert!(rejected > 0 && accepted > 0);
}

#[test]
fn routine_catalog_authority_retains_only_the_owned_missing_role_diagnostic() {
    let owner = "missing한🙂".repeat(8192);
    let text = format!(r#"{{"f":[{{"owner":"{owner}","execute_acl":null}}]}}"#);
    let expected = reference(&text, &roles()).err().unwrap().to_string();
    let source = MemoryBudget::new(8 << 20);
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let mut input = decoder(
        document(&text, &source, &token),
        roles(),
        &memory,
        &token,
        &token,
    )
    .unwrap();
    drop(text);
    let error = complete(&mut input, 1).err().unwrap();
    drop(input);
    assert_eq!(source.used(), 0);
    assert_eq!(error.to_string(), expected);
    assert!(memory.used() >= owner.len());
    assert!(matches!(error, StorageBackendError::RetainedDiagnostic(_)));
    let error = uqa_storage::backup::BackupError::from(error);
    assert!(matches!(
        error,
        uqa_storage::backup::BackupError::Invalid("stored catalog authority")
    ));
    assert!(!error.to_string().contains(&owner));
    assert_eq!(memory.used(), 0);
}
