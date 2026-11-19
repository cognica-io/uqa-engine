//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

struct Names {
    path: Vec<String>,
    role: RoleReference,
}

type Input = ImageDecoder<Produced<RelationNameResolution>, SQLError>;

fn input(
    names: Arc<Names>,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Input {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&native, &original, input.cancellation);
        RelationNameResolution::from_inputs_with_work(
            names.path.iter().map(String::as_str),
            control.copy_text("pg_temp_123")?,
            true,
            &names.role,
            RelationLookupMode::Bound,
            &control,
            &mut input,
        )
        .await
    })
    .unwrap()
}

fn advance(input: &mut Input) -> Result<Option<Produced<RelationNameResolution>>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn complete(input: &mut Input) -> (Produced<RelationNameResolution>, usize) {
    for polls in 1..100_000 {
        if let Some(value) = advance(input).unwrap() {
            return (value, polls);
        }
    }
    panic!("session namespace capture stopped progressing")
}

fn fixture(role: RoleReference) -> Arc<Names> {
    Arc::new(Names {
        path: vec![
            "public".into(),
            String::new(),
            "界".repeat(6001),
            "public".into(),
        ],
        role,
    })
}

#[test]
fn retained_namespace_capture_matches_inline_order_role_identity_and_exact_ownership() {
    let bound = Arc::new(RoleBinding::from_definition(&RoleDefinition::bootstrap()).unwrap());
    for role in [
        RoleReference::Bound(bound.clone()),
        RoleReference::Named("界".repeat(6001)),
    ] {
        let names = fixture(role);
        let native = MemoryBudget::new(1 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&native, &token, &token);
        let ordinary = RelationNameResolution::from_inputs_with_control(
            names.path.iter().map(String::as_str),
            control.copy_text("pg_temp_123").unwrap(),
            true,
            &names.role,
            RelationLookupMode::Bound,
            &control,
        )
        .unwrap();
        let (retained, polls) = complete(&mut input(names, &native, &work, &token, &token));
        assert!(polls > 8);
        assert_eq!(retained.search_path, ordinary.search_path);
        assert_eq!(retained.temporary_schema, ordinary.temporary_schema);
        assert_eq!(
            retained.temporary_namespace_allocated,
            ordinary.temporary_namespace_allocated
        );
        assert_eq!(retained.lookup_mode, ordinary.lookup_mode);
        assert_eq!(retained.current_user, ordinary.current_user);
        if let RoleReference::Bound(value) = &retained.current_user {
            assert!(Arc::ptr_eq(value, &bound));
        }
        assert_eq!(retained.reserved_bytes(), ordinary.reserved_bytes());
        assert_eq!(
            native.used(),
            retained.reserved_bytes() + ordinary.reserved_bytes()
        );
        assert_eq!(work.used(), 0);
        drop((ordinary, retained));
        assert_eq!(native.used(), 0);
    }
}

#[test]
fn retained_namespace_capture_retires_every_suspended_prefix_and_both_cancellation_owners() {
    let names = fixture(RoleReference::Named("界".repeat(6001)));
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let (value, polls) = complete(&mut input(
        names.clone(),
        &native,
        &work,
        &original,
        &invoking,
    ));
    drop(value);
    let sibling = native.reserve(17).unwrap();
    for prefix in 0..polls - 1 {
        for cancelled in [None, Some(&original), Some(&invoking)] {
            let mut selected = input(names.clone(), &native, &work, &original, &invoking);
            for _ in 0..prefix {
                assert!(advance(&mut selected).unwrap().is_none());
            }
            if let Some(token) = cancelled {
                token.cancel();
                assert_eq!(
                    advance(&mut selected).err().unwrap().sqlstate(),
                    Some("57014")
                );
                token.reset();
                assert_eq!(
                    advance(&mut selected).err().unwrap().sqlstate(),
                    Some("XX001")
                );
            }
            drop(selected);
            assert_eq!((native.used(), work.used()), (17, 0));
        }
    }
    drop(sibling);
    assert_eq!(native.used(), 0);
}

#[test]
fn retained_namespace_capture_bounds_wide_payloads_and_rejects_unadmitted_copies() {
    let names = fixture(RoleReference::Named("uqa".into()));
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for limit in [0, 10, 64, 128, 1024] {
        let native = MemoryBudget::new(limit);
        let mut selected = input(names.clone(), &native, &work, &token, &token);
        let mut error = None;
        let allocations = allocation_counter::measure(|| {
            for _ in 0..100 {
                match advance(&mut selected) {
                    Ok(None) => {}
                    Ok(Some(_)) => panic!("capture exceeded its allowance"),
                    Err(value) => {
                        error = Some(value);
                        break;
                    }
                }
            }
        });
        assert_eq!(error.unwrap().sqlstate(), Some("53200"));
        assert!(allocations.bytes_total < 4096, "{allocations:?}");
        assert_eq!((native.used(), work.used()), (0, 0));
    }
    let native = MemoryBudget::new(1 << 20);
    let names = Arc::new(Names {
        path: vec![String::new(); 1000],
        role: "uqa".into(),
    });
    let (value, polls) = complete(&mut input(names, &native, &work, &token, &token));
    assert!(polls >= 1000);
    assert_eq!(value.search_path.len(), 1000);
    drop(value);
    assert_eq!((native.used(), work.used()), (0, 0));
}
