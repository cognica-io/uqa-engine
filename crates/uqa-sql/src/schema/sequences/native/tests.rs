//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::AutoIncrement, ColumnType};

fn column() -> ColumnDef {
    let mut column = ColumnDef::nullable("id", ColumnType::Integer);
    column.object_id = Some([2; 16]);
    column
}

fn owner(dependency: SequenceOwnerDependency) -> SequenceOwner {
    SequenceOwner {
        table_object_id: [1; 16],
        column_object_id: [2; 16],
        dependency,
    }
}

fn check(
    tables: &[([u8; 16], Vec<ColumnDef>)],
    sequences: &[(RelationIdentity, Option<SequenceOwner>)],
) -> Result<(), SQLError> {
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let result = validate(
        tables.iter().map(|(id, columns)| (*id, columns.as_slice())),
        sequences.iter().map(|(name, owner)| (name, *owner)),
        &memory,
        &token,
    );
    assert_eq!(memory.used(), 0);
    result
}

#[test]
fn native_sequence_owners_use_original_column_incarnations_without_rebinding() {
    let tables = vec![([1; 16], vec![column()])];
    let name = RelationIdentity::new("public", "s");
    let mut owned = owner(SequenceOwnerDependency::Automatic);
    check(&tables, &[(name.clone(), Some(owned))]).unwrap();
    owned.table_object_id = [3; 16];
    assert!(check(&tables, &[(name.clone(), Some(owned))]).is_err());
    owned.table_object_id = [1; 16];
    owned.column_object_id = [3; 16];
    assert!(check(&tables, &[(name, Some(owned))]).is_err());
}

#[test]
fn native_sequence_columns_preserve_inherited_provenance_and_legacy_counters() {
    let mut serial = column();
    serial.auto_increment = Some(AutoIncrement {
        sequence: Some("public.s".into()),
        ..AutoIncrement::serial()
    });
    let mut legacy = column();
    legacy.object_id = Some([3; 16]);
    legacy.auto_increment = Some(AutoIncrement::legacy());
    let tables = vec![
        ([1; 16], vec![serial.clone(), legacy]),
        ([4; 16], vec![serial]),
    ];
    let sequences = [(
        RelationIdentity::new("public", "s"),
        Some(owner(SequenceOwnerDependency::Automatic)),
    )];
    let before = serde_json::to_string(&tables).unwrap();
    check(&tables, &sequences).unwrap();
    assert_eq!(serde_json::to_string(&tables).unwrap(), before);
    assert!(check(&tables, &[]).is_err());
}

#[test]
fn native_sequence_internal_owners_require_the_original_identity_column_and_sequence() {
    let name = RelationIdentity::new("public", "s");
    let sequences = [(name.clone(), Some(owner(SequenceOwnerDependency::Internal)))];
    let mut tables = vec![([1; 16], vec![column()])];
    assert!(check(&tables, &sequences).is_err());
    tables[0].1[0].auto_increment = Some(AutoIncrement {
        sequence: Some("public.s".into()),
        ..AutoIncrement::identity_always()
    });
    check(&tables, &sequences).unwrap();
    let mut wrong = sequences.to_vec();
    wrong[0].0.name = "other".into();
    assert!(check(&tables, &wrong).is_err());
    tables[0].1[0].auto_increment.as_mut().unwrap().kind = crate::ast::AutoIncrementKind::Serial;
    assert!(check(&tables, &sequences).is_err());
}

#[test]
fn native_sequence_references_reject_unbound_names_and_unconsumed_declarations() {
    let name = RelationIdentity::new("public", "s");
    let mut tables = vec![([1; 16], vec![column()])];
    for reference in [None, Some("s".into()), Some("public.missing".into())] {
        tables[0].1[0].auto_increment = Some(AutoIncrement {
            sequence: reference,
            ..AutoIncrement::serial()
        });
        assert!(check(&tables, &[(name.clone(), None)]).is_err());
    }
    tables[0].1[0].auto_increment = Some(AutoIncrement {
        sequence: Some("public.s".into()),
        declaration: Some(Box::default()),
        ..AutoIncrement::identity_by_default()
    });
    assert!(check(&tables, &[(name, None)]).is_err());
}

#[test]
fn native_sequence_validation_rejects_duplicate_owners_and_releases_denied_workspace() {
    let tables = [([1; 16], vec![column()])];
    assert!(check(&[tables[0].clone(), tables[0].clone()], &[]).is_err());
    assert!(check(&[([1; 16], vec![column(), column()])], &[]).is_err());
    let token = CancellationToken::new();
    for limit in [0, 1] {
        let memory = MemoryBudget::new(limit);
        assert!(validate(
            tables.iter().map(|(id, c)| (*id, c.as_slice())),
            std::iter::empty(),
            &memory,
            &token
        )
        .is_err());
        assert_eq!(memory.used(), 0);
    }
    token.cancel();
    let memory = MemoryBudget::new(64 << 10);
    assert!(validate(
        tables.iter().map(|(id, c)| (*id, c.as_slice())),
        std::iter::empty(),
        &memory,
        &token
    )
    .is_err());
    assert_eq!(memory.used(), 0);
}
