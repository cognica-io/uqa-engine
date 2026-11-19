//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::SQLError;
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

fn input(
    source: TypeDefinitions<StoredDomain>,
    mode: DomainValidationMode,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> ImageDecoder<(), SQLError> {
    let roles = roles();
    let budget = memory.clone();
    let original = original.clone();
    ImageDecoder::new(memory, invoking, |mut reader| async move {
        let mut work = reader.input();
        let control = ProductionControl::new(&budget, &original, work.cancellation);
        validate_from_input(&source, &roles, mode, &control, &mut work)
            .await
            .map_err(ConstraintMetadataError::into_sql_error)
    })
    .unwrap()
}

fn finish(input: &mut ImageDecoder<(), SQLError>, steps: usize) -> Result<usize, SQLError> {
    for polls in 1..65536 {
        if input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })?
            .is_some()
        {
            return Ok(polls);
        }
    }
    panic!("domain validation did not progress")
}

#[test]
fn domain_validation_input_preserves_windows_and_complete_scratch_retirement() {
    let source = TypeDefinitions::try_from_iter(registry(4, 64)).unwrap();
    let token = CancellationToken::new();
    let mut visits = Vec::new();
    for steps in [1, 7, 64] {
        let memory = MemoryBudget::new(1 << 20);
        let mut input = input(
            source.clone(),
            DomainValidationMode::Complete,
            &memory,
            &token,
            &token,
        );
        let mut polls = 0;
        loop {
            let before = memory.used();
            let result = input
                .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
                .unwrap();
            assert!(
                memory.used().saturating_sub(before)
                    <= steps * 3 * OwnedMap::<[u8; 16], ()>::entry_bytes()
            );
            polls += 1;
            if result.is_some() {
                break;
            }
            assert!(polls < 65536);
        }
        assert_eq!(memory.used(), 0);
        assert!(memory.peak() > 0);
        assert!(finish(&mut input, 1).is_err());
        visits.push(polls);
    }
    assert!(visits[0] > visits[1] && visits[1] > visits[2]);
}

#[test]
fn domain_validation_input_keeps_local_global_and_required_field_failure_order() {
    for corruption in 0..5 {
        let mut source = registry(2, 2);
        let duplicate = source["public.domain_0"].definition.checks[0].catalog_identity;
        let second = source.get_mut("public.domain_1").unwrap();
        match corruption {
            0 => {
                second.definition.checks[0].catalog_identity = duplicate;
                second.definition.checks[1].name = second.definition.checks[0].name.clone();
            }
            1 => second.definition.checks[0].catalog_identity = duplicate,
            2 => second.owner.object_id = [99; 16],
            3 => second.object_id = [0; 16],
            _ => (),
        }
        source
            .get_mut("public.domain_0")
            .unwrap()
            .definition
            .not_null
            .as_mut()
            .unwrap()
            .name = None;
        for mode in [
            DomainValidationMode::Definitions,
            DomainValidationMode::Complete,
        ] {
            let expected = match mode {
                DomainValidationMode::Definitions => {
                    crate::catalog::domain::validate_domain_definitions(&source, &roles())
                }
                DomainValidationMode::Complete => validate_domain_registry(&source, &roles()),
            }
            .map_err(|error| SQLError::Internal(error).to_string());
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let mut input = input(
                TypeDefinitions::try_from_iter(source.clone()).unwrap(),
                mode,
                &memory,
                &token,
                &token,
            );
            assert_eq!(
                finish(&mut input, 1)
                    .map(|_| ())
                    .map_err(|error| error.to_string()),
                expected
            );
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn domain_validation_input_retires_original_invoking_memory_and_abandoned_owners() {
    let source = TypeDefinitions::try_from_iter(registry(4, 64)).unwrap();
    for terminal in ["original", "invoking", "memory", "zero", "drop", "unwind"] {
        let memory = MemoryBudget::new(1 << 20);
        let sibling = memory.reserve(17).unwrap();
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let mut input = input(
            source.clone(),
            DomainValidationMode::Complete,
            &memory,
            &original,
            &invoking,
        );
        assert!(input
            .advance(
                &mut std::io::empty(),
                ImageDecodeWindow {
                    bytes: 1,
                    steps: 16
                }
            )
            .unwrap()
            .is_none());
        let occupied = (terminal == "memory").then(|| memory.reserve(memory.available()).unwrap());
        match terminal {
            "original" => original.cancel(),
            "invoking" => invoking.cancel(),
            "unwind" => {
                let _ = catch_unwind(AssertUnwindSafe(move || {
                    let _owner = input;
                    panic!("retire domain validation");
                }));
                assert_eq!(memory.used(), sibling.bytes());
                continue;
            }
            _ => (),
        }
        if terminal != "drop" {
            let error = finish(&mut input, usize::from(terminal != "zero")).unwrap_err();
            match terminal {
                "original" | "invoking" => assert_eq!(error.sqlstate(), Some("57014")),
                "memory" => assert_eq!(error.sqlstate(), Some("53200")),
                _ => (),
            }
            assert!(finish(&mut input, 1).is_err());
        }
        drop((input, occupied));
        assert_eq!(memory.used(), sibling.bytes());
    }
}
