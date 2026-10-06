//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn dropped_slots_survive_empty_relations_and_names_can_be_reused() {
    let mut columns = vec![
        ColumnDef::nullable("a", ColumnType::Integer),
        ColumnDef::nullable("b", ColumnType::Text),
    ];
    assert!(materialize(&mut columns, &[]).unwrap());
    let dropped = columns
        .iter()
        .enumerate()
        .map(|(index, column)| DroppedAttribute::from_column(column, index, true, 0).unwrap())
        .collect::<Vec<_>>();
    columns.clear();
    validate(&columns, &dropped).unwrap();
    columns.push(ColumnDef::nullable("a", ColumnType::BigInteger));
    materialize(&mut columns, &dropped).unwrap();
    assert_eq!(columns[0].attribute_number, Some(3));
    assert_eq!(dropped[0].name(), "........pg.dropped.1........");
    columns[0].name = "renamed".into();
    assert!(!materialize(&mut columns, &dropped).unwrap());
}

#[test]
fn invalid_attribute_layouts_do_not_publish_partial_numbering() {
    let mut columns = vec![
        ColumnDef::nullable("a", ColumnType::Integer),
        ColumnDef::nullable("b", ColumnType::Integer),
    ];
    columns[0].attribute_number = Some(2);
    assert!(materialize(&mut columns, &[]).is_err());
    assert_eq!(columns[1].attribute_number, None);
    columns[1].attribute_number = Some(2);
    assert!(validate(&columns, &[]).is_err());
    columns[0].attribute_number = Some(0);
    assert!(validate(&columns, &[]).is_err());
}

#[test]
fn dropped_attributes_count_towards_the_postgresql_column_limit() {
    let mut columns = (0..MAX_ATTRIBUTES)
        .map(|index| ColumnDef::nullable(index.to_string(), ColumnType::Integer))
        .collect::<Vec<_>>();
    materialize(&mut columns, &[]).unwrap();
    let dropped = DroppedAttribute::from_column(&columns[0], 0, true, 0).unwrap();
    columns.remove(0);
    columns.push(ColumnDef::nullable("new", ColumnType::Text));
    let error = materialize(&mut columns, &[dropped]).unwrap_err();
    assert!(
        matches!(error, SQLError::Routine { ref sqlstate, ref message } if sqlstate == "54011" && message == "tables can have at most 1600 columns")
    );
    assert_eq!(columns.last().unwrap().attribute_number, None);
}

#[test]
fn dropped_attributes_keep_physical_metadata_without_type_references() {
    let mut column = ColumnDef::nullable(
        "a",
        ColumnType::Array(Box::new(ColumnType::Varchar(Some(8)))),
    );
    column.attribute_number = Some(4);
    let dropped = DroppedAttribute::from_column(&column, 0, false, 1).unwrap();
    assert_eq!(
        (
            dropped.number,
            dropped.type_length,
            dropped.type_modifier,
            dropped.dimensions
        ),
        (4, -1, 12, 1)
    );
    assert_eq!(
        (
            dropped.alignment.as_str(),
            dropped.storage.as_str(),
            dropped.collation
        ),
        ("i", "x", 100)
    );
    assert!(!dropped.is_local);
    let encoded = serde_json::to_string(&dropped).unwrap();
    assert_eq!(
        serde_json::from_str::<DroppedAttribute>(&encoded).unwrap(),
        dropped
    );
    assert!(!encoded.contains("object_id"));
}

#[test]
fn inherited_merge_keeps_the_child_attribute_identity() {
    let mut parent = ColumnDef::nullable("merged", ColumnType::Integer);
    parent.object_id = Some([1; 16]);
    parent.attribute_number = Some(5);
    let mut child = parent.clone();
    child.object_id = Some([2; 16]);
    child.attribute_number = Some(2);
    crate::schema::inheritance::merge_same_column(&mut parent, child.clone()).unwrap();
    assert_eq!(parent.object_id, child.object_id);
    assert_eq!(parent.attribute_number, child.attribute_number);
}

#[test]
fn identity_sequence_removal_preserves_the_dropped_attribute_flag() {
    for (mut provenance, flag) in [
        (crate::ast::AutoIncrement::identity_always(), "a"),
        (crate::ast::AutoIncrement::identity_by_default(), "d"),
        (crate::ast::AutoIncrement::serial(), ""),
    ] {
        provenance.sequence = Some("public.sequence".into());
        let mut columns = vec![ColumnDef::nullable("identity", ColumnType::BigInteger)];
        columns[0].auto_increment = Some(provenance);
        crate::schema::sequences::dependencies::detach_sequence_provenance(
            &mut columns,
            "public.sequence",
        );
        let dropped = DroppedAttribute::from_column(&columns[0], 0, true, 0).unwrap();
        assert_eq!(dropped.identity, flag);
    }
}
