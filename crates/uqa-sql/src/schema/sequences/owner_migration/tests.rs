//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{AutoIncrement, AutoIncrementOwner, ColumnDef, Statement};
use std::collections::{BTreeMap, BTreeSet};

fn column() -> ColumnDef {
    let Statement::CreateTable(mut table) = crate::compile("CREATE TABLE items(id integer)")
        .unwrap()
        .remove(0)
    else {
        panic!("table declaration")
    };
    let mut column = table.columns.remove(0);
    column.object_id = Some([2; 16]);
    column
}
fn owned_column(mut provenance: AutoIncrement) -> ColumnDef {
    let mut column = column();
    provenance.sequence = Some("public.ids".into());
    provenance.owner = Some(AutoIncrementOwner {
        table: "public.items".into(),
        column: "id".into(),
    });
    column.auto_increment = Some(provenance);
    column
}

#[test]
fn migration_sequence_names_require_one_exact_qualified_or_unqualified_candidate() {
    let sequences = [
        RelationIdentity::new("public", "ids"),
        RelationIdentity::new("tenant", "ids"),
        RelationIdentity::new("Mixed.Schema", "Mixed.Sequence"),
    ];
    assert_eq!(
        resolve_migrated_sequence_reference("tenant.ids", &sequences).unwrap(),
        sequences[1]
    );
    assert_eq!(
        resolve_migrated_sequence_reference(r#""Mixed.Schema"."Mixed.Sequence""#, &sequences)
            .unwrap(),
        sequences[2]
    );
    assert_eq!(
        resolve_migrated_sequence_reference("ids", &sequences).unwrap_err(),
        "implicit sequence owner reference `ids` is ambiguous"
    );
    assert_eq!(
        resolve_migrated_sequence_reference("missing", &sequences).unwrap_err(),
        "implicit sequence owner references missing sequence `missing`"
    );
    assert_eq!(
        resolve_migrated_sequence_reference("ids", &sequences[..1]).unwrap(),
        sequences[0]
    );
}

#[test]
fn migration_requires_column_identity_even_without_sequence_provenance() {
    let mut column = column();
    column.object_id = None;
    let mut valid = BTreeSet::new();
    let mut inferred = BTreeMap::new();
    let error = collect_migrated_sequence_owner(
        &RelationIdentity::new("public", "items"),
        [1; 16],
        &column,
        &[],
        &mut valid,
        &mut inferred,
    )
    .unwrap_err();
    assert_eq!(
        error,
        "column `public.items`.`id` has no object identity during sequence-owner migration"
    );
    assert!(valid.is_empty() && inferred.is_empty());
}

#[test]
fn nonowning_and_incomplete_provenance_still_registers_the_real_column_identity() {
    let plain = column();
    let mut unnamed = owned_column(AutoIncrement::serial());
    unnamed.auto_increment.as_mut().unwrap().owner = None;
    let mut inherited = owned_column(AutoIncrement::serial());
    inherited
        .auto_increment
        .as_mut()
        .unwrap()
        .owner
        .as_mut()
        .unwrap()
        .table = "public.parent".into();
    let mut unbound = owned_column(AutoIncrement::serial());
    unbound.auto_increment.as_mut().unwrap().sequence = None;
    for column in [plain, unnamed, inherited, unbound] {
        let mut valid = BTreeSet::new();
        let mut inferred = BTreeMap::new();
        collect_migrated_sequence_owner(
            &RelationIdentity::new("public", "items"),
            [1; 16],
            &column,
            &[],
            &mut valid,
            &mut inferred,
        )
        .unwrap();
        assert_eq!(valid, BTreeSet::from([([1; 16], [2; 16])]));
        assert!(inferred.is_empty());
    }
}

#[test]
fn serial_and_identity_migrations_preserve_distinct_dependency_strengths() {
    let sequence = RelationIdentity::new("public", "ids");
    for (provenance, dependency) in [
        (AutoIncrement::serial(), SequenceOwnerDependency::Automatic),
        (
            AutoIncrement::identity_always(),
            SequenceOwnerDependency::Internal,
        ),
        (
            AutoIncrement::identity_by_default(),
            SequenceOwnerDependency::Internal,
        ),
    ] {
        let mut valid = BTreeSet::new();
        let mut inferred = BTreeMap::new();
        collect_migrated_sequence_owner(
            &RelationIdentity::new("public", "items"),
            [1; 16],
            &owned_column(provenance),
            std::slice::from_ref(&sequence),
            &mut valid,
            &mut inferred,
        )
        .unwrap();
        assert_eq!(
            inferred[&sequence],
            SequenceOwner {
                table_object_id: [1; 16],
                column_object_id: [2; 16],
                dependency
            }
        );
    }
}

#[test]
fn conflicting_migrated_owners_fail_after_registering_the_candidate_identities() {
    let sequence = RelationIdentity::new("public", "ids");
    let relation = RelationIdentity::new("public", "items");
    let mut valid = BTreeSet::new();
    let mut inferred = BTreeMap::new();
    let column = owned_column(AutoIncrement::serial());
    collect_migrated_sequence_owner(
        &relation,
        [1; 16],
        &column,
        std::slice::from_ref(&sequence),
        &mut valid,
        &mut inferred,
    )
    .unwrap();
    let error = collect_migrated_sequence_owner(
        &relation,
        [3; 16],
        &column,
        std::slice::from_ref(&sequence),
        &mut valid,
        &mut inferred,
    )
    .unwrap_err();
    assert_eq!(
        error,
        "sequence `public.ids` has conflicting implicit owners"
    );
    assert_eq!(
        valid,
        BTreeSet::from([([1; 16], [2; 16]), ([3; 16], [2; 16])])
    );
    assert_eq!(inferred[&sequence].table_object_id, [3; 16]);
}

#[test]
fn repeated_identical_migration_owners_are_idempotent() {
    let sequence = RelationIdentity::new("public", "ids");
    let mut valid = BTreeSet::new();
    let mut inferred = BTreeMap::new();
    for _ in 0..2 {
        collect_migrated_sequence_owner(
            &RelationIdentity::new("public", "items"),
            [1; 16],
            &owned_column(AutoIncrement::serial()),
            std::slice::from_ref(&sequence),
            &mut valid,
            &mut inferred,
        )
        .unwrap();
    }
    assert_eq!(valid.len(), 1);
    assert_eq!(inferred.len(), 1);
}
