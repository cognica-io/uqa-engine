//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn stored(labels: &[&str]) -> StoredEnum {
    let labels = labels
        .iter()
        .map(|label| (*label).to_owned())
        .collect::<Vec<_>>();
    let oids = (0..labels.len())
        .map(|index| 20_000 + u32::try_from(index).unwrap())
        .collect::<Vec<_>>();
    StoredEnum {
        object_id: [1; 16],
        oid: 16_384,
        array_oid: 16_385,
        array_name: "_mood".into(),
        identity: RelationIdentity::new("public", "mood"),
        owner: RoleIdentity::BOOTSTRAP,
        labels: initial_enum_labels(16_384, &labels, &oids).unwrap(),
        usage_acl: None,
    }
}

fn order(definition: &StoredEnum) -> Vec<(String, f32)> {
    definition
        .labels
        .iter()
        .map(|label| (label.label.clone(), label.sort_order))
        .collect()
}

fn neighbor(label: &str, after: bool) -> Option<EnumNeighbor> {
    Some(EnumNeighbor {
        label: label.into(),
        after,
    })
}

fn assert_keys_increase(definition: &StoredEnum) {
    for pair in definition.labels.windows(2) {
        assert!(pair[0].key < pair[1].key, "{pair:?}");
    }
}

#[test]
fn sort_positions_follow_postgresql_add_enum_label() {
    let mut definition = stored(&["sad", "ok", "happy"]);
    let mut next_oid = 30_000;
    let mut add = |definition: &mut StoredEnum, label: &str, position: Option<EnumNeighbor>| {
        next_oid += 1;
        definition
            .add_label(label, position.as_ref(), false, next_oid)
            .unwrap()
    };
    add(&mut definition, "curious", neighbor("ok", true));
    add(&mut definition, "angry", neighbor("sad", false));
    add(&mut definition, "ecstatic", None);
    add(&mut definition, "serene", neighbor("curious", false));
    definition.rename_label("ok", "neutral").unwrap();
    assert_eq!(
        order(&definition),
        [
            ("angry".to_owned(), 0.0),
            ("sad".to_owned(), 1.0),
            ("neutral".to_owned(), 2.0),
            ("serene".to_owned(), 2.25),
            ("curious".to_owned(), 2.5),
            ("happy".to_owned(), 3.0),
            ("ecstatic".to_owned(), 4.0),
        ]
    );
    assert_keys_increase(&definition);
}

#[test]
fn collapsed_float4_midpoints_renumber_catalog_positions_but_not_keys() {
    let mut definition = stored(&["a", "z"]);
    let original_z = definition.labels[1].key.clone();
    for index in 1..=25 {
        definition
            .add_label(
                &format!("v{index}"),
                neighbor("z", false).as_ref(),
                false,
                40_000 + index,
            )
            .unwrap();
    }
    definition
        .add_label("v26", neighbor("a", true).as_ref(), false, 50_026)
        .unwrap();
    definition
        .add_label("v27", neighbor("a", false).as_ref(), false, 50_027)
        .unwrap();
    definition
        .add_label("v28", neighbor("v27", false).as_ref(), false, 50_028)
        .unwrap();
    // Positions captured from PostgreSQL 18.4 for the same history.
    let mut expected = vec![
        ("v28".to_owned(), -1.0),
        ("v27".to_owned(), 0.0),
        ("a".to_owned(), 1.0),
        ("v26".to_owned(), 1.5),
    ];
    for index in 1..=23 {
        expected.push((format!("v{index}"), (index + 1) as f32));
    }
    expected.extend([
        ("v24".to_owned(), 24.5),
        ("v25".to_owned(), 24.75),
        ("z".to_owned(), 25.0),
    ]);
    assert_eq!(order(&definition), expected);
    assert_keys_increase(&definition);
    assert_eq!(definition.label_by_text("z").unwrap().key, original_z);
}

#[test]
fn declaration_errors_follow_postgresql_check_order() {
    let long = "a".repeat(64);
    let edge = "a".repeat(63);
    assert!(initial_enum_labels(1, std::slice::from_ref(&edge), &[2]).is_ok());
    let error =
        initial_enum_labels(1, &["a".into(), "a".into(), long.clone()], &[2, 3, 4]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("23505"));
    assert_eq!(
        error.to_string(),
        "duplicate key value violates unique constraint \"pg_enum_typid_label_index\""
    );
    let error =
        initial_enum_labels(1, &[long.clone(), "a".into(), "a".into()], &[2, 3, 4]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42602"));
    assert_eq!(error.to_string(), format!("invalid enum label \"{long}\""));
    assert!(initial_enum_labels(1, &[], &[]).unwrap().is_empty());
    let blank = initial_enum_labels(1, &[String::new(), " ".into()], &[2, 3]).unwrap();
    assert_eq!(blank[0].label, "");

    let mut definition = stored(&["sad", "ok", "happy"]);
    let error = definition
        .add_label(&long, neighbor("missing", true).as_ref(), true, 9)
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42602"));
    let error = definition
        .add_label("happy", neighbor("missing", true).as_ref(), false, 9)
        .unwrap_err();
    assert_eq!(
        (error.sqlstate(), error.to_string()),
        (
            Some("42710"),
            "enum label \"happy\" already exists".to_owned()
        )
    );
    assert_eq!(
        definition
            .add_label("happy", neighbor("missing", true).as_ref(), true, 9)
            .unwrap(),
        AddedEnumLabel::Skipped("enum label \"happy\" already exists, skipping".into())
    );
    let error = definition
        .add_label("x", neighbor("missing", false).as_ref(), false, 9)
        .unwrap_err();
    assert_eq!(
        (error.sqlstate(), error.to_string()),
        (
            Some("22023"),
            "\"missing\" is not an existing enum label".to_owned()
        )
    );
    assert_eq!(definition.labels.len(), 3);

    let error = definition.rename_label("sad", &long).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42602"));
    let error = definition.rename_label("missing", "y").unwrap_err();
    assert_eq!(error.sqlstate(), Some("22023"));
    let error = definition.rename_label("sad", "happy").unwrap_err();
    assert_eq!(error.sqlstate(), Some("42710"));
    let error = definition.rename_label("sad", "sad").unwrap_err();
    assert_eq!(error.sqlstate(), Some("42710"));
}
