//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::routines::{RoutineBody, SQLUserFunction};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

const ROUTINES: u32 = 64;
const WRAPPERS: u32 = 48;

fn catalogs() -> (ForeignWrappers, RoutineRegistry) {
    let (mut wrapper, template) = super::super::tests::callback_fixture();
    let mut routines = RoutineRegistry::new();
    for index in 1..=ROUTINES {
        let mut routine = template.def.clone();
        routine.name = format!("public.r{index:03}");
        routine.object_id = Some(u128::from(index).to_be_bytes());
        routine.catalog_oid = Some(20_000 + index);
        routines
            .insert(
                routine.name.clone(),
                vec![SQLUserFunction::new(routine, RoutineBody::Source).shared()],
            )
            .unwrap();
    }
    let last = &routines["public.r064"][0].def;
    let callback = wrapper.validator.as_mut().unwrap();
    callback.oid = last.catalog_oid.unwrap();
    callback.binding.object_id = last.object_id;
    callback.binding.name.clone_from(&last.name);
    let mut wrappers = ForeignWrappers::new();
    for index in 1..=WRAPPERS {
        wrapper.name = format!("w{index:03}");
        wrapper.identity.oid = 30_000 + index;
        wrapper.identity.object_id = u128::from(1000 + index).to_be_bytes();
        wrappers
            .insert(wrapper.name.clone(), wrapper.clone())
            .unwrap();
    }
    (wrappers, routines)
}

fn reader(
    wrappers: ForeignWrappers,
    routines: RoutineRegistry,
    visits: &Arc<AtomicUsize>,
    memory: &MemoryBudget,
    cancel: &CancellationToken,
) -> ImageDecoder<(), SQLError> {
    let visits = visits.clone();
    ImageDecoder::new(memory, cancel, move |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(input.memory, input.cancellation, input.cancellation);
        let groups = routines.values().map(|group| {
            group.iter().inspect(|_| {
                visits.fetch_add(1, Ordering::Relaxed);
            })
        });
        validate_groups(&wrappers, groups, &control, &mut input).await
    })
    .unwrap()
}

#[test]
fn each_window_visits_at_most_one_routine_and_multiple_callbacks_scan_the_registry_once() {
    let (mut wrappers, routines) = catalogs();
    for corrupt in [false, true] {
        if corrupt {
            wrappers
                .edit("w048", |wrapper| {
                    wrapper.validator.as_mut().unwrap().oid += 1;
                    Ok(())
                })
                .unwrap();
        }
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let visits = Arc::new(AtomicUsize::new(0));
        let mut input = reader(wrappers.clone(), routines.clone(), &visits, &memory, &token);
        let mut windows = 0;
        loop {
            let before = visits.load(Ordering::Relaxed);
            let result = input.advance(
                &mut std::io::empty(),
                ImageDecodeWindow { bytes: 1, steps: 1 },
            );
            assert!(visits.load(Ordering::Relaxed) - before <= 1);
            windows += 1;
            match result {
                Ok(None) => continue,
                Ok(Some(())) => assert!(!corrupt),
                Err(error) => {
                    assert!(corrupt);
                    assert!(error.to_string().contains("w048"));
                }
            }
            break;
        }
        assert_eq!(visits.load(Ordering::Relaxed), ROUTINES as usize);
        assert!(windows > (ROUTINES + WRAPPERS) as usize);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn a_later_bad_target_cannot_displace_the_first_callback_and_cancellation_releases_its_index() {
    let (mut wrappers, routines) = catalogs();
    wrappers
        .edit("w001", |wrapper| {
            let mut handler = wrapper.validator.clone().unwrap();
            handler.oid = 1;
            wrapper.handler = ForeignWrapperHandler::Function(handler);
            wrapper.validator.as_mut().unwrap().binding.object_id = Some(1u128.to_be_bytes());
            Ok(())
        })
        .unwrap();
    wrappers
        .edit("w002", |wrapper| {
            wrapper.validator.as_mut().unwrap().binding.object_id = Some(1u128.to_be_bytes());
            Ok(())
        })
        .unwrap();
    for cancelled in [false, true] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let visits = Arc::new(AtomicUsize::new(0));
        let mut input = reader(wrappers.clone(), routines.clone(), &visits, &memory, &token);
        loop {
            if cancelled && visits.load(Ordering::Relaxed) == 7 {
                token.cancel();
            }
            match input.advance(
                &mut std::io::empty(),
                ImageDecodeWindow { bytes: 1, steps: 1 },
            ) {
                Ok(None) => (),
                Ok(Some(())) => panic!("corrupt callback accepted"),
                Err(error) => {
                    if cancelled {
                        assert_eq!(error.sqlstate(), Some("57014"));
                        assert_eq!(visits.load(Ordering::Relaxed), 7);
                    } else {
                        assert!(error.to_string().contains("w001"));
                        assert_eq!(visits.load(Ordering::Relaxed), ROUTINES as usize);
                    }
                    break;
                }
            }
        }
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn wrappers_without_stored_callbacks_need_no_index_and_visit_no_routines() {
    let (_, routines) = catalogs();
    let wrappers = super::super::native_wrappers();
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let groups = routines
        .values()
        .map(|_| -> std::iter::Empty<&SQLRoutine> { panic!("unselected routine group visited") });
    inline(validate_groups(&wrappers, groups, &control, &mut ())).unwrap();
    assert_eq!(memory.used(), 0);
}
