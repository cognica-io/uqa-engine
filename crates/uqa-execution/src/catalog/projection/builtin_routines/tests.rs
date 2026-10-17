//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeSet;

use super::PG18_BUILTIN_ROUTINE_GROUPS;

mod call_signatures;
mod range_subtype_distance;

#[test]
fn type_introspection_metadata_matches_postgresql() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("tests/type_introspection.json")).unwrap();
    let expected = &reference["cases"][0]["results"][0]["rows"][0];
    let oid = expected[0].as_str().unwrap().parse::<i64>().unwrap();
    let routine = PG18_BUILTIN_ROUTINE_GROUPS
        .iter()
        .flat_map(|group| group.iter())
        .find(|routine| routine.oid == oid)
        .unwrap();
    assert!(routine.argument_names.is_empty());
    assert_eq!(
        serde_json::json!([
            routine.oid.to_string(),
            routine.name,
            routine.kind,
            boolean_text(routine.strict),
            routine.volatility,
            routine.parallel,
            boolean_text(routine.leakproof),
            routine.return_type.to_string(),
            routine
                .argument_types
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(" "),
            null,
            routine.default_arguments.to_string(),
            routine.argument_defaults,
            routine.source,
            routine.language().to_string(),
        ]),
        *expected,
    );
}

#[test]
fn integer_series_metadata_matches_postgresql() {
    let reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/series_binding_oracle.expected.json"
    )))
    .unwrap();
    let catalog = &reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == "catalog")
        .unwrap()["results"][0];
    let expected = catalog["rows"].as_array().unwrap();
    assert_eq!(expected.len(), 6);
    for row in expected {
        let oid = row[0].as_str().unwrap().parse::<i64>().unwrap();
        let routine = PG18_BUILTIN_ROUTINE_GROUPS
            .iter()
            .flat_map(|group| group.iter())
            .find(|routine| routine.oid == oid)
            .unwrap();
        let actual = vec![
            routine.oid.to_string(),
            routine.name.into(),
            routine.source.into(),
            routine.kind.into(),
            boolean_text(routine.strict).into(),
            routine.volatility.into(),
            routine.parallel.into(),
            boolean_text(routine.leakproof).into(),
            boolean_text(routine.returns_set()).into(),
            routine.estimated_rows().to_string(),
            routine.argument_types.len().to_string(),
            routine.default_arguments.to_string(),
            routine.support_oid().to_string(),
            routine
                .argument_types
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(" "),
            routine.return_type.to_string(),
        ];
        assert_eq!(serde_json::to_value(actual).unwrap(), *row, "OID {oid}");
    }
}

#[test]
fn clock_and_case_metadata_matches_postgresql() {
    assert_catalog_metadata(
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/parity/pg18/builtin_routine_identities_oracle.expected.json"
        )),
        7,
    );
}

#[test]
fn numeric_metadata_matches_postgresql() {
    assert_catalog_metadata(
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/parity/pg18/numeric_routine_identities_oracle.expected.json"
        )),
        11,
    );
}

#[test]
fn temporal_metadata_matches_postgresql() {
    assert_catalog_metadata(
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/parity/pg18/date_trunc_timezone_oracle.expected.json"
        )),
        7,
    );
}

#[test]
fn timestamp_zone_metadata_matches_postgresql() {
    let expected: Vec<Vec<String>> = serde_json::from_str(include_str!("timezone.json")).unwrap();
    assert_eq!(expected.len(), 6);
    for row in expected {
        let oid = row[0].parse::<i64>().unwrap();
        let routine = PG18_BUILTIN_ROUTINE_GROUPS
            .iter()
            .flat_map(|group| group.iter())
            .find(|routine| routine.oid == oid)
            .unwrap();
        assert_eq!(
            vec![
                routine.oid.to_string(),
                routine.name.into(),
                routine.source.into(),
                routine.volatility.into(),
                routine.strict.to_string(),
                routine.parallel.into(),
                routine.return_type.to_string(),
                routine
                    .argument_types
                    .iter()
                    .map(i64::to_string)
                    .collect::<Vec<_>>()
                    .join(" "),
            ],
            row
        );
    }
}

#[test]
fn extraction_metadata_and_sql_wrapper_match_postgresql() {
    let reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/finite_extraction_oracle.expected.json"
    )))
    .unwrap();
    let cases = reference["cases"].as_array().unwrap();
    let catalog = &cases.iter().find(|case| case["id"] == "catalog").unwrap()["results"][0];
    let columns = catalog["columns"].as_array().unwrap();
    let rows = catalog["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 12);
    for row in rows {
        let oid = row[0].as_str().unwrap().parse::<i64>().unwrap();
        let entry = PG18_BUILTIN_ROUTINE_GROUPS
            .iter()
            .flat_map(|group| group.iter())
            .find(|entry| entry.oid == oid)
            .unwrap();
        let actual = serde_json::json!({
            "oid": entry.oid.to_string(),
            "proname": entry.name,
            "argument_oids": entry.argument_types.iter().map(i64::to_string).collect::<Vec<_>>().join(" "),
            "prorettype": entry.return_type.to_string(),
            "prolang": entry.language().to_string(),
            "lanname": uqa_sql::catalog::languages::language_name(u32::try_from(entry.language()).unwrap()),
            "prosrc": entry.source,
            "provolatile": entry.volatility,
            "proisstrict": boolean_text(entry.strict),
            "proretset": boolean_text(entry.returns_set()),
            "proparallel": entry.parallel,
            "proleakproof": boolean_text(entry.leakproof),
            "procost": "1",
            "prorows": entry.estimated_rows().to_string(),
            "pronargdefaults": entry.default_arguments.to_string(),
            "proargdefaults": entry.argument_defaults,
            "proargnames": if entry.argument_names.is_empty() { None } else { Some(entry.argument_names) },
            "prosupport": "0",
            "provariadic": entry.variadic_type().to_string(),
            "prokind": entry.kind,
            "has_sql_body": boolean_text(entry.sql_body().is_some()),
        });
        for (column, expected) in columns.iter().zip(row.as_array().unwrap()) {
            let column = column.as_str().unwrap();
            assert_eq!(&actual[column], expected, "OID {oid} {column}");
        }
        if oid == 1384 {
            let body = &cases
                .iter()
                .find(|case| case["id"] == "date_wrapper")
                .unwrap()["results"][0]["rows"][0][0];
            assert_eq!(entry.sql_body().as_deref(), body.as_str());
        }
    }
}

#[test]
fn substring_metadata_and_sql_body_match_postgresql() {
    let transcript = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/substring_pattern_forms_oracle.expected.json"
    ));
    assert_catalog_metadata(transcript, 10);
    let reference: serde_json::Value = serde_json::from_str(transcript).unwrap();
    let rows = |id: &str| {
        reference["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["id"] == id)
            .unwrap()["results"][0]["rows"]
            .clone()
    };
    let entry = |oid: i64| {
        PG18_BUILTIN_ROUTINE_GROUPS
            .iter()
            .flat_map(|group| group.iter())
            .find(|entry| entry.oid == oid)
            .copied()
            .unwrap()
    };
    assert_eq!(
        entry(2074).sql_body().as_deref(),
        rows("catalog_body")[0][0].as_str()
    );
    let languages = rows("catalog_languages");
    for (row, oid) in languages.as_array().unwrap().iter().zip([2073, 2074]) {
        let routine = entry(oid);
        assert_eq!(row[0], serde_json::json!(oid.to_string()));
        assert_eq!(
            row[1],
            serde_json::json!(uqa_sql::catalog::languages::language_name(
                u32::try_from(routine.language()).unwrap()
            ))
        );
        assert_eq!(
            row[2],
            serde_json::json!(boolean_text(routine.sql_body().is_some()))
        );
    }
}

#[test]
fn regexp_matches_metadata_and_set_estimates_match_postgresql() {
    let transcript = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/regexp_matches_rows_oracle.expected.json"
    ));
    assert_catalog_metadata(transcript, 2);
    let reference: serde_json::Value = serde_json::from_str(transcript).unwrap();
    let rows = reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == "catalog_set_metadata")
        .unwrap()["results"][0]["rows"]
        .clone();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    for row in rows {
        let oid = row[0].as_str().unwrap().parse::<i64>().unwrap();
        let routine = PG18_BUILTIN_ROUTINE_GROUPS
            .iter()
            .flat_map(|group| group.iter())
            .find(|routine| routine.oid == oid)
            .unwrap();
        assert_eq!(
            [
                boolean_text(routine.returns_set()).to_string(),
                routine.estimated_rows().to_string(),
                format!("{{{}}}", routine.argument_names.join(",")),
            ],
            [
                row[1].as_str().unwrap(),
                row[2].as_str().unwrap(),
                row[3].as_str().unwrap()
            ],
            "OID {oid}"
        );
    }
}

fn assert_catalog_metadata(transcript: &str, expected_count: usize) {
    let reference: serde_json::Value = serde_json::from_str(transcript).unwrap();
    let catalog = reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == "catalog")
        .unwrap();
    let expected: Vec<Vec<String>> =
        serde_json::from_value(catalog["results"][0]["rows"].clone()).unwrap();
    assert_eq!(expected.len(), expected_count);
    let oids: BTreeSet<i64> = expected.iter().map(|row| row[0].parse().unwrap()).collect();
    let mut routines: Vec<_> = PG18_BUILTIN_ROUTINE_GROUPS
        .iter()
        .flat_map(|group| group.iter())
        .filter(|routine| oids.contains(&routine.oid))
        .collect();
    routines.sort_by_key(|routine| routine.oid);
    let actual: Vec<Vec<String>> = routines
        .into_iter()
        .map(|routine| {
            vec![
                routine.oid.to_string(),
                routine.name.into(),
                routine.kind.into(),
                boolean_text(routine.strict).into(),
                routine.volatility.into(),
                routine.parallel.into(),
                boolean_text(routine.leakproof).into(),
                routine.return_type.to_string(),
                routine
                    .argument_types
                    .iter()
                    .map(i64::to_string)
                    .collect::<Vec<_>>()
                    .join(" "),
                routine.source.into(),
            ]
        })
        .collect();
    assert_eq!(actual, expected);
}

fn boolean_text(value: bool) -> &'static str {
    if value {
        "t"
    } else {
        "f"
    }
}

#[test]
fn builtin_routine_oids_and_signatures_are_unique() {
    let mut oids = BTreeSet::new();
    let mut signatures = BTreeSet::new();
    for routine in PG18_BUILTIN_ROUTINE_GROUPS
        .iter()
        .flat_map(|group| group.iter())
    {
        assert!(
            oids.insert(routine.oid),
            "duplicate OID {} for {}",
            routine.oid,
            routine.name
        );
        assert!(
            signatures.insert((routine.name, routine.argument_types)),
            "duplicate signature {}({:?})",
            routine.name,
            routine.argument_types
        );
    }
}

#[test]
fn array_inspection_metadata_matches_postgresql() {
    assert_catalog_metadata(
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/parity/pg18/array_inspection_routine_identities_oracle.expected.json"
        )),
        6,
    );
}

#[test]
fn regcollation_lookup_metadata_matches_postgresql() {
    let reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/regcollation_oracle.expected.json"
    )))
    .unwrap();
    let case = reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == "lookup_metadata")
        .unwrap();
    let entry = PG18_BUILTIN_ROUTINE_GROUPS
        .iter()
        .flat_map(|group| group.iter())
        .find(|entry| entry.oid == 4195)
        .unwrap();
    let actual = vec![
        entry.oid.to_string(),
        entry.name.into(),
        entry.return_type.to_string(),
        entry
            .argument_types
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(" "),
        boolean_text(entry.strict).into(),
        entry.volatility.into(),
        entry.parallel.into(),
    ];
    assert_eq!(
        serde_json::to_value(actual).unwrap(),
        case["results"][0]["rows"][0]
    );
}
