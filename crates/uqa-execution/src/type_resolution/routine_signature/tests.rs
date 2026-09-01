//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_sql::ast::{ColumnType, RangeSubtype};

use super::*;
use crate::type_resolution::rank_function_matches;

fn parameter(type_name: &str) -> RoutineParameterDescriptor {
    RoutineParameterDescriptor {
        name: None,
        type_name: type_name.into(),
        has_default: false,
        variadic: false,
    }
}

fn match_types(
    parameters: &[RoutineParameterDescriptor],
    argument_types: &[Option<ColumnType>],
) -> Result<Option<MatchedRoutineSignature>, RoutineSignatureMatchError> {
    let argument_names = vec![None; argument_types.len()];
    match_routine_signature(
        parameters,
        RoutineCallDescriptor {
            argument_names: &argument_names,
            argument_types,
            explicit_variadic: false,
        },
    )
}

#[test]
fn parses_every_polymorphic_family_and_marks_available_actual_carriers() {
    let cases = [
        ("anyelement", true),
        ("anyarray", true),
        ("anynonarray", true),
        ("anyenum", false),
        ("anyrange", true),
        ("anymultirange", true),
        ("anycompatible", true),
        ("anycompatiblearray", true),
        ("anycompatiblenonarray", true),
        ("anycompatiblerange", true),
        ("anycompatiblemultirange", true),
    ];
    for (type_name, has_actual_carrier) in cases {
        assert_eq!(
            routine_polymorphic_type(type_name)
                .expect("known polymorphic spelling")
                .has_actual_carrier(),
            has_actual_carrier,
            "{type_name}"
        );
    }
}

#[test]
fn simple_family_requires_known_consistent_types_and_substitutes_outputs() {
    let matched = match_types(&[parameter("anyelement")], &[Some(ColumnType::Integer)])
        .unwrap()
        .unwrap();
    assert_eq!(matched.argument_positions, [0]);
    assert_eq!(matched.argument_targets, ["int4"]);
    assert_eq!(matched.parameter_types, ["int4"]);
    assert_eq!(
        matched.substitute_type("anyelement"),
        Some(ColumnType::Integer)
    );
    assert_eq!(
        matched.substitute_type("anyarray"),
        Some(ColumnType::Array(Box::new(ColumnType::Integer)))
    );

    let error = match_types(&[parameter("anyelement")], &[None]).unwrap_err();
    assert_eq!(error.sqlstate(), "42804");

    assert!(match_types(
        &[parameter("anyarray"), parameter("anyelement")],
        &[
            Some(ColumnType::Array(Box::new(ColumnType::Integer))),
            Some(ColumnType::BigInteger),
        ],
    )
    .unwrap()
    .is_none());
}

#[test]
fn simple_anyarray_flattens_array_domains() {
    let domain = ColumnType::Domain {
        schema: "public".into(),
        name: "ints".into(),
        oid: 42,
        base: Box::new(ColumnType::Array(Box::new(ColumnType::Integer))),
    };
    let matched = match_types(&[parameter("anyarray")], &[Some(domain)])
        .unwrap()
        .unwrap();
    assert_eq!(matched.argument_targets, ["int4[]"]);
    assert_eq!(
        matched.substitute_type("anyelement"),
        Some(ColumnType::Integer)
    );
}

#[test]
fn compatible_family_uses_common_type_and_unknown_text_fallback() {
    let numeric = ColumnType::Numeric {
        precision: None,
        scale: None,
    };
    let cases = [
        (ColumnType::SmallInteger, ColumnType::Integer, "int4"),
        (ColumnType::Integer, ColumnType::BigInteger, "int8"),
        (ColumnType::Integer, numeric, "numeric"),
    ];
    for (left, right, expected) in cases {
        let matched = match_types(
            &[parameter("anycompatible"), parameter("anycompatible")],
            &[Some(left), Some(right)],
        )
        .unwrap()
        .unwrap();
        assert_eq!(matched.argument_targets, [expected, expected]);
    }
    let matched = match_types(
        &[parameter("anycompatible"), parameter("anycompatible")],
        &[None, None],
    )
    .unwrap()
    .unwrap();
    assert_eq!(matched.argument_targets, ["text", "text"]);

    assert!(match_types(
        &[parameter("anycompatiblenonarray")],
        &[Some(ColumnType::Array(Box::new(ColumnType::Integer)))],
    )
    .unwrap()
    .is_none());

    let array = match_types(
        &[parameter("anycompatiblearray"), parameter("anycompatible")],
        &[
            Some(ColumnType::Array(Box::new(ColumnType::Integer))),
            Some(ColumnType::BigInteger),
        ],
    )
    .unwrap()
    .unwrap();
    assert_eq!(array.argument_targets, ["int8[]", "int8"]);
}

#[test]
fn unavailable_enum_carrier_does_not_claim_concrete_actuals() {
    assert!(
        match_types(&[parameter("anyenum")], &[Some(ColumnType::Integer)])
            .unwrap()
            .is_none()
    );
    assert!(match_types(&[parameter("anyenum")], &[None])
        .unwrap()
        .is_none());
}

#[test]
fn simple_range_family_links_range_multirange_and_subtype() {
    let matched = match_types(
        &[parameter("anyrange"), parameter("anyelement")],
        &[
            Some(ColumnType::Range(RangeSubtype::Integer)),
            Some(ColumnType::Integer),
        ],
    )
    .unwrap()
    .unwrap();
    assert_eq!(matched.argument_targets, ["int4range", "int4"]);
    assert_eq!(
        matched.substitute_type("anyrange"),
        Some(ColumnType::Range(RangeSubtype::Integer))
    );
    assert_eq!(
        matched.substitute_type("anymultirange"),
        Some(ColumnType::Multirange(RangeSubtype::Integer))
    );
    assert_eq!(
        matched.substitute_type("anyelement"),
        Some(ColumnType::Integer)
    );

    let multirange = match_types(
        &[parameter("anymultirange")],
        &[Some(ColumnType::Multirange(RangeSubtype::Date))],
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        multirange.substitute_type("anyrange"),
        Some(ColumnType::Range(RangeSubtype::Date))
    );

    assert!(match_types(
        &[parameter("anyrange"), parameter("anyelement")],
        &[
            Some(ColumnType::Range(RangeSubtype::Integer)),
            Some(ColumnType::BigInteger),
        ],
    )
    .unwrap()
    .is_none());
    for type_name in [
        "anyrange",
        "anymultirange",
        "anycompatiblerange",
        "anycompatiblemultirange",
    ] {
        assert_eq!(
            match_types(&[parameter(type_name)], &[None])
                .unwrap_err()
                .sqlstate(),
            "42804"
        );
    }
}

#[test]
fn compatible_range_family_promotes_the_subtype_without_inventing_range_casts() {
    let matched = match_types(
        &[parameter("anycompatiblerange"), parameter("anycompatible")],
        &[
            Some(ColumnType::Range(RangeSubtype::BigInteger)),
            Some(ColumnType::Integer),
        ],
    )
    .unwrap()
    .unwrap();
    assert_eq!(matched.argument_targets, ["int8range", "int8"]);
    assert_eq!(
        matched.substitute_type("anycompatiblemultirange"),
        Some(ColumnType::Multirange(RangeSubtype::BigInteger))
    );

    assert!(match_types(
        &[parameter("anycompatiblerange"), parameter("anycompatible")],
        &[
            Some(ColumnType::Range(RangeSubtype::Integer)),
            Some(ColumnType::BigInteger),
        ],
    )
    .unwrap()
    .is_none());
    assert_eq!(
        match_types(
            &[parameter("anycompatiblerange"), parameter("anycompatible")],
            &[None, Some(ColumnType::Integer)],
        )
        .unwrap_err()
        .sqlstate(),
        "42804"
    );
    assert!(match_types(
        &[parameter("anycompatiblerange")],
        &[Some(ColumnType::Integer)],
    )
    .unwrap()
    .is_none());
}

#[test]
fn compatible_family_preserves_equal_domains_and_flattens_mixed_domains() {
    let domain = ColumnType::Domain {
        schema: "public".into(),
        name: "positive_int".into(),
        oid: 43,
        base: Box::new(ColumnType::Integer),
    };
    let same = match_types(
        &[parameter("anycompatible"), parameter("anycompatible")],
        &[Some(domain.clone()), Some(domain.clone())],
    )
    .unwrap()
    .unwrap();
    assert_eq!(same.argument_targets, ["public.positive_int"; 2]);

    let mixed = match_types(
        &[parameter("anycompatible"), parameter("anycompatible")],
        &[Some(domain), Some(ColumnType::Integer)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(mixed.argument_targets, ["int4", "int4"]);
}

#[test]
fn variadic_mapping_distinguishes_pack_default_and_explicit_array() {
    let mut variadic = parameter("int4[]");
    variadic.name = Some("xs".into());
    variadic.variadic = true;

    assert!(match_types(&[variadic.clone()], &[]).unwrap().is_none());
    let packed = match_types(
        &[variadic.clone()],
        &[Some(ColumnType::Integer), Some(ColumnType::SmallInteger)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(packed.argument_positions, [0, 0]);
    assert_eq!(packed.argument_targets, ["int4", "int4"]);
    assert_eq!(packed.variadic_mode, RoutineVariadicMode::Pack);

    variadic.has_default = true;
    let defaulted = match_types(&[variadic.clone()], &[]).unwrap().unwrap();
    assert_eq!(defaulted.variadic_mode, RoutineVariadicMode::Default);
    assert_eq!(defaulted.defaulted_parameters, [0]);

    let argument_names = [None];
    let argument_types = [Some(ColumnType::Array(Box::new(ColumnType::Integer)))];
    let passed = match_routine_signature(
        &[variadic.clone()],
        RoutineCallDescriptor {
            argument_names: &argument_names,
            argument_types: &argument_types,
            explicit_variadic: true,
        },
    )
    .unwrap()
    .unwrap();
    assert_eq!(passed.argument_targets, ["int4[]"]);
    assert_eq!(passed.variadic_mode, RoutineVariadicMode::PassThrough);

    let named = [Some("xs".into())];
    assert!(match_routine_signature(
        &[variadic.clone()],
        RoutineCallDescriptor {
            argument_names: &named,
            argument_types: &argument_types,
            explicit_variadic: false,
        },
    )
    .unwrap()
    .is_none());
    let explicit_named = match_routine_signature(
        &[variadic],
        RoutineCallDescriptor {
            argument_names: &named,
            argument_types: &argument_types,
            explicit_variadic: true,
        },
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        explicit_named.variadic_mode,
        RoutineVariadicMode::PassThrough
    );
}

#[test]
fn variadic_candidates_reject_named_notation_but_keep_positional_defaults() {
    let mut first = parameter("int4");
    first.name = Some("a".into());
    let mut second = parameter("int4");
    second.name = Some("b".into());
    second.has_default = true;
    let mut variadic = parameter("int4[]");
    variadic.name = Some("xs".into());
    variadic.has_default = true;
    variadic.variadic = true;
    let parameters = [first, second, variadic];

    let positional_names = [None];
    let positional_types = [Some(ColumnType::Integer)];
    let positional = match_routine_signature(
        &parameters,
        RoutineCallDescriptor {
            argument_names: &positional_names,
            argument_types: &positional_types,
            explicit_variadic: false,
        },
    )
    .unwrap()
    .unwrap();
    assert_eq!(positional.defaulted_parameters, [1, 2]);
    assert_eq!(positional.variadic_mode, RoutineVariadicMode::Default);

    let named = [Some("a".into())];
    assert!(match_routine_signature(
        &parameters,
        RoutineCallDescriptor {
            argument_names: &named,
            argument_types: &positional_types,
            explicit_variadic: false,
        },
    )
    .unwrap()
    .is_none());
}

#[test]
fn generic_variadic_infers_element_and_concrete_parameter_array() {
    let mut variadic = parameter("anycompatiblearray");
    variadic.variadic = true;
    let matched = match_types(&[variadic], &[Some(ColumnType::Integer), None])
        .unwrap()
        .unwrap();
    assert_eq!(matched.argument_targets, ["int4", "int4"]);
    assert_eq!(matched.parameter_types, ["int4[]"]);
    assert_eq!(
        matched.substitute_type_name("anycompatiblearray"),
        Some("int4[]".into())
    );
    let invocation = matched.invocation_binding(Some("anycompatible"));
    assert_eq!(invocation.argument_positions, [0, 0]);
    assert_eq!(invocation.argument_targets, ["int4", "int4"]);
    assert_eq!(invocation.parameter_types, ["int4[]"]);
    assert_eq!(invocation.return_type, Some("int4".into()));
    assert_eq!(
        invocation.variadic_mode,
        InvocationVariadicMode::Expanded { parameter_index: 0 }
    );
}

#[test]
fn ranking_keeps_oracle_ambiguities_and_prefers_fixed_equivalent_signature() {
    let array_actual = [Some(ColumnType::Array(Box::new(ColumnType::Integer)))];
    let anyelement = match_types(&[parameter("anyelement")], &array_actual)
        .unwrap()
        .unwrap();
    let anyarray = match_types(&[parameter("anyarray")], &array_actual)
        .unwrap()
        .unwrap();
    let mut ambiguous = vec![anyelement, anyarray];
    assert!(rank_function_matches(&mut ambiguous, &array_actual));
    assert_eq!(ambiguous.len(), 2);

    let fixed = match_types(&[parameter("int4")], &[Some(ColumnType::Integer)])
        .unwrap()
        .unwrap();
    let mut variadic_parameter = parameter("int4[]");
    variadic_parameter.variadic = true;
    let expanded = match_types(&[variadic_parameter], &[Some(ColumnType::Integer)])
        .unwrap()
        .unwrap();
    let mut fixed_over_variadic = vec![expanded, fixed];
    assert!(rank_function_matches(
        &mut fixed_over_variadic,
        &[Some(ColumnType::Integer)]
    ));
    assert_eq!(fixed_over_variadic.len(), 1);
    assert_eq!(
        fixed_over_variadic[0].variadic_mode,
        RoutineVariadicMode::None
    );
}

#[test]
fn fixed_array_candidate_survives_explicit_variadic_marker() {
    let argument_names = [None];
    let argument_types = [Some(ColumnType::Array(Box::new(ColumnType::Integer)))];
    let fixed = match_routine_signature(
        &[parameter("int4[]")],
        RoutineCallDescriptor {
            argument_names: &argument_names,
            argument_types: &argument_types,
            explicit_variadic: true,
        },
    )
    .unwrap()
    .unwrap();
    assert_eq!(fixed.variadic_mode, RoutineVariadicMode::None);

    let named = [Some("value".into())];
    assert!(match_routine_signature(
        &[parameter("int4[]")],
        RoutineCallDescriptor {
            argument_names: &named,
            argument_types: &argument_types,
            explicit_variadic: true,
        },
    )
    .unwrap()
    .is_none());

    let mut generic_variadic = parameter("anyarray");
    generic_variadic.variadic = true;
    let passed_through = match_routine_signature(
        &[generic_variadic],
        RoutineCallDescriptor {
            argument_names: &argument_names,
            argument_types: &argument_types,
            explicit_variadic: true,
        },
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        passed_through.variadic_mode,
        RoutineVariadicMode::PassThrough
    );

    let mut candidates = vec![passed_through, fixed];
    assert!(rank_function_matches(&mut candidates, &argument_types));
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].declared_identity, ["int4[]"]);
}

#[test]
fn fixed_zero_and_defaulted_variadic_zero_remain_ambiguous() {
    let fixed = match_types(&[], &[]).unwrap().unwrap();
    let mut variadic = parameter("int4[]");
    variadic.has_default = true;
    variadic.variadic = true;
    let defaulted = match_types(&[variadic], &[]).unwrap().unwrap();
    assert_eq!(defaulted.variadic_mode, RoutineVariadicMode::Default);

    let mut candidates = vec![defaulted, fixed];
    assert!(rank_function_matches(&mut candidates, &[]));
    assert_eq!(candidates.len(), 2);
}

#[test]
fn concrete_and_generic_ranking_matches_postgresql_oracle() {
    let concrete = match_types(&[parameter("int4")], &[Some(ColumnType::Integer)])
        .unwrap()
        .unwrap();
    let generic = match_types(&[parameter("anyelement")], &[Some(ColumnType::Integer)])
        .unwrap()
        .unwrap();
    let mut exact = vec![generic, concrete];
    assert!(rank_function_matches(
        &mut exact,
        &[Some(ColumnType::Integer)]
    ));
    assert_eq!(exact.len(), 1);
    assert_eq!(exact[0].declared_identity, ["int4"]);

    let concrete = match_types(&[parameter("int4")], &[Some(ColumnType::SmallInteger)])
        .unwrap()
        .unwrap();
    let generic = match_types(
        &[parameter("anyelement")],
        &[Some(ColumnType::SmallInteger)],
    )
    .unwrap()
    .unwrap();
    let mut ambiguous = vec![generic, concrete];
    assert!(rank_function_matches(
        &mut ambiguous,
        &[Some(ColumnType::SmallInteger)]
    ));
    assert_eq!(ambiguous.len(), 2);
}
