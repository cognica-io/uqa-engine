//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{default_parameter, parameter};
use crate::ast::{ColumnType, CompositeTypeReference, RoutineDefaultType};
use crate::type_resolution::{
    match_routine_candidate_with_control, routine_polymorphic_type_with_control,
    RoutineCallDescriptor, RoutineInvocationParts, RoutinePolymorphicType,
    RoutineSignatureMatchError, RoutineSignatureProductionError, RoutineVariadicMode,
};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl, ProductionVec},
    CancellationToken,
};

fn composite() -> ColumnType {
    ColumnType::Composite(CompositeTypeReference {
        schema: "owner".repeat(1024),
        name: "result".repeat(2048),
        oid: 50_001,
        array_oid: 50_002,
        relation_oid: 50_003,
    })
}

#[test]
fn candidate_and_derived_outputs_have_independent_exact_owners() {
    let parameters = [parameter("anyelement")];
    let types = [Some(composite())];
    let call = RoutineCallDescriptor {
        argument_names: &[None],
        argument_types: &types,
        explicit_variadic: false,
    };
    let budget = MemoryBudget::new(16 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    assert_eq!(
        routine_polymorphic_type_with_control("pg_catalog.anyelement", &control).unwrap(),
        Some(RoutinePolymorphicType::AnyElement)
    );
    let mut candidate = None;
    let copied = allocation_counter::measure(|| {
        candidate = match_routine_candidate_with_control(&parameters, call, &control).unwrap();
    });
    let candidate = candidate.unwrap();
    assert_eq!(candidate.argument_positions, [0]);
    assert_eq!(candidate.parameter_type_values, types);
    assert_eq!(copied.bytes_current as usize, candidate.reserved_bytes());
    assert_eq!(budget.used(), candidate.reserved_bytes());

    let mut invocation = None;
    let published = allocation_counter::measure(|| {
        invocation = Some(
            candidate
                .invocation_binding_with_control(Some("anyarray"), &control)
                .unwrap(),
        );
    });
    let invocation = invocation.unwrap();
    assert_eq!(
        published.bytes_current as usize,
        invocation.reserved_bytes()
    );
    assert_eq!(
        budget.used(),
        candidate.reserved_bytes() + invocation.reserved_bytes()
    );
    let substituted = candidate
        .substitute_type_with_control("anyarray", &control)
        .unwrap()
        .unwrap();
    assert!(
        matches!(&*substituted, ColumnType::Array(element) if Some(element.as_ref()) == types[0].as_ref())
    );
    drop(candidate);
    assert_eq!(
        budget.used(),
        invocation.reserved_bytes() + substituted.reserved_bytes()
    );
    assert_eq!(invocation.argument_positions, [0]);
    drop((invocation, substituted));
    assert_eq!(budget.used(), 0);
}

#[test]
fn named_defaults_and_variadic_candidates_keep_mapping_and_concrete_targets() {
    let mut first = parameter("int4");
    first.name = Some("first".into());
    let mut second =
        default_parameter("text", Some(RoutineDefaultType::Concrete(ColumnType::Text)));
    second.name = Some("second".into());
    let mut last = parameter("int8");
    last.name = Some("last".into());
    let parameters = [first, second, last];
    let names = [Some("last".into()), Some("first".into())];
    let types = [Some(ColumnType::Integer), Some(ColumnType::SmallInteger)];
    let budget = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let candidate = match_routine_candidate_with_control(
        &parameters,
        RoutineCallDescriptor {
            argument_names: &names,
            argument_types: &types,
            explicit_variadic: false,
        },
        &control,
    )
    .unwrap()
    .unwrap();
    assert_eq!(candidate.argument_positions, [2, 0]);
    assert_eq!(candidate.defaulted_parameters, [1]);
    assert_eq!(candidate.argument_targets, ["int8", "int4"]);
    assert_eq!(candidate.parameter_types, ["int4", "text", "int8"]);
    assert_eq!(candidate.exact_matches, 0);
    drop(candidate);
    assert_eq!(budget.used(), 0);

    let mut variadic = parameter("anycompatiblearray");
    variadic.variadic = true;
    let parameters = [variadic];
    let types = [Some(ColumnType::Integer), Some(ColumnType::BigInteger)];
    let mut candidate = None;
    let allocation = allocation_counter::measure(|| {
        candidate = match_routine_candidate_with_control(
            &parameters,
            RoutineCallDescriptor {
                argument_names: &[None, None],
                argument_types: &types,
                explicit_variadic: false,
            },
            &control,
        )
        .unwrap();
    });
    let candidate = candidate.unwrap();
    assert_eq!(candidate.variadic_mode, RoutineVariadicMode::Pack);
    assert_eq!(candidate.argument_positions, [0, 0]);
    assert_eq!(candidate.argument_targets, ["int8", "int8"]);
    assert_eq!(candidate.parameter_types, ["int8[]"]);
    assert_eq!(
        allocation.bytes_current as usize,
        candidate.reserved_bytes()
    );
    drop(candidate);
    assert_eq!(budget.used(), 0);
}

#[test]
fn retained_default_diagnostic_owns_nested_types_after_candidate_scratch_retires() {
    let parameters = [
        parameter("anyelement"),
        default_parameter(
            "anyarray",
            Some(RoutineDefaultType::Concrete(ColumnType::Array(Box::new(
                ColumnType::Integer,
            )))),
        ),
    ];
    let types = [Some(composite())];
    let budget = MemoryBudget::new(16 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut candidate = None;
    let allocation = allocation_counter::measure(|| {
        candidate = match_routine_candidate_with_control(
            &parameters,
            RoutineCallDescriptor {
                argument_names: &[None],
                argument_types: &types,
                explicit_variadic: false,
            },
            &control,
        )
        .unwrap();
    });
    let candidate = candidate.unwrap();
    assert!(matches!(
        candidate.default_error,
        Some(RoutineSignatureMatchError::InconsistentPolymorphicTypes { .. })
    ));
    assert_eq!(
        allocation.bytes_current as usize,
        candidate.reserved_bytes()
    );
    assert_eq!(budget.used(), candidate.reserved_bytes());
    drop(candidate);
    assert_eq!(budget.used(), 0);
}

#[test]
fn replacing_invocation_fields_releases_only_the_discarded_allocations() {
    let parameters = [parameter("anyelement")];
    let types = [Some(composite())];
    let budget = MemoryBudget::new(16 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let candidate = match_routine_candidate_with_control(
        &parameters,
        RoutineCallDescriptor {
            argument_names: &[None],
            argument_types: &types,
            explicit_variadic: false,
        },
        &control,
    )
    .unwrap()
    .unwrap();
    let mut parts: RoutineInvocationParts = candidate
        .invocation_binding_parts_with_control(Some("anyelement"), &control)
        .unwrap();
    drop(candidate);
    let before = budget.used();
    let old_parameter_bytes = parts.parameter_types.reserved_bytes();
    let mut names = ProductionVec::new(control);
    names
        .push_produced(control.copy_text("text").unwrap())
        .unwrap();
    let replacement = names.finish().unwrap();
    let replacement_bytes = replacement.reserved_bytes();
    let dropped = allocation_counter::measure(|| {
        parts.parameter_types = replacement;
    });
    assert_eq!(dropped.count_total, 0);
    assert_eq!(-dropped.bytes_current as usize, old_parameter_bytes);
    assert_eq!(
        budget.used(),
        before - old_parameter_bytes + replacement_bytes
    );
    let invocation = parts.finish(&control).unwrap();
    assert_eq!(invocation.parameter_types, ["text"]);
    assert_eq!(
        invocation.argument_targets[0],
        types[0].as_ref().unwrap().catalog_name()
    );
    assert_eq!(budget.used(), invocation.reserved_bytes());
    drop(invocation);
    assert_eq!(budget.used(), 0);
}

#[test]
fn quota_rejection_and_nonmatching_candidates_release_all_private_scratch() {
    let parameters = [parameter("anyelement")];
    let types = [Some(composite())];
    let budget = MemoryBudget::new(1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let allocation = allocation_counter::measure(|| {
        let error = match_routine_candidate_with_control(
            &parameters,
            RoutineCallDescriptor {
                argument_names: &[None],
                argument_types: &types,
                explicit_variadic: false,
            },
            &control,
        )
        .unwrap_err();
        assert!(
            matches!(error, RoutineSignatureProductionError::Production(ref error) if error.sqlstate() == Some("53200"))
        );
    });
    assert!(allocation.bytes_total < 4096, "{allocation:?}");
    assert_eq!(budget.used(), 0);

    let budget = MemoryBudget::new(1024 * 1024);
    let control = ProductionControl::new(&budget, &token, &token);
    let types = [Some(ColumnType::Text)];
    assert!(match_routine_candidate_with_control(
        &[parameter("int4")],
        RoutineCallDescriptor {
            argument_names: &[None],
            argument_types: &types,
            explicit_variadic: false,
        },
        &control
    )
    .unwrap()
    .is_none());
    assert_eq!(budget.used(), 0);
}

#[test]
fn cancelled_candidates_do_not_allocate_and_semantic_errors_keep_their_own_lease() {
    let parameters = [parameter("anyelement")];
    let types = [Some(composite())];
    for cancelled_original in [true, false] {
        let budget = MemoryBudget::new(1024 * 1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        if cancelled_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert!(control.check().is_err());
        let allocation = allocation_counter::measure(|| {
            let error = match_routine_candidate_with_control(
                &parameters,
                RoutineCallDescriptor {
                    argument_names: &[None],
                    argument_types: &types,
                    explicit_variadic: false,
                },
                &control,
            )
            .unwrap_err();
            assert!(
                matches!(error, RoutineSignatureProductionError::Production(ref error) if error.sqlstate() == Some("57014"))
            );
        });
        assert_eq!(allocation.count_total, 0);
        assert_eq!(budget.used(), 0);
    }
    let mut invalid = parameter("integer");
    invalid.variadic = true;
    let parameters = [invalid];
    let budget = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut result = None;
    let allocation = allocation_counter::measure(|| {
        result = Some(
            match_routine_candidate_with_control(
                &parameters,
                RoutineCallDescriptor {
                    argument_names: &[],
                    argument_types: &[],
                    explicit_variadic: false,
                },
                &control,
            )
            .unwrap_err(),
        );
    });
    let RoutineSignatureProductionError::Signature(error) = result.unwrap() else {
        panic!("signature diagnostic");
    };
    assert_eq!(error.sqlstate(), "42P13");
    assert_eq!(allocation.bytes_current as usize, error.reserved_bytes());
    assert_eq!(budget.used(), error.reserved_bytes());
    drop(error);
    assert_eq!(budget.used(), 0);
}
