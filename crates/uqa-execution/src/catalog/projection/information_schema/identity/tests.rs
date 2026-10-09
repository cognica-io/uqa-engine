//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{
    projection::CatalogRequest, sequence::SequenceState, test_support::empty_catalog,
    CatalogReadView,
};
use std::sync::Arc;
use uqa_core::RelationIdentity;
use uqa_sql::ast::{AutoIncrement, AutoIncrementOwner, ColumnType, SequenceDataType};

#[test]
fn identity_attributes_borrow_only_the_owned_sequence_definition() {
    let mut snapshot = empty_catalog().snapshot().clone();
    let state = SequenceState::initial(7, 2, SequenceDataType::BigInt);
    let selected = RelationIdentity::new("public", "owned_sequence");
    let sequences = Arc::make_mut(&mut snapshot.definitions.sequences);
    sequences.insert(selected.clone(), state);
    for i in 0..128 {
        sequences.insert(
            RelationIdentity::new("public", format!("unrelated_{i}")),
            state,
        );
    }
    let catalog = CatalogReadView::new(snapshot);
    let mut column = SQLColumnDef::nullable("id", ColumnType::BigInteger);
    let mut provenance = AutoIncrement::identity_always();
    provenance.sequence = Some(selected.qualified_name());
    provenance.owner = Some(AutoIncrementOwner {
        table: "public.items".into(),
        column: "id".into(),
    });
    column.auto_increment = Some(provenance);
    let request = CatalogRequest::columns(["identity_start".into()]);
    let state = owned_identity_sequence(&catalog, &request, "public.items", &column)
        .unwrap()
        .unwrap();
    assert!(std::ptr::eq(
        state,
        &raw const catalog.snapshot().definitions.sequences[&selected]
    ));
    assert_eq!(
        IdentityAttributes::of(&column, Some(state)).start,
        Value::Str("7".into())
    );
    assert!(
        owned_identity_sequence(&catalog, &request, "public.child", &column)
            .unwrap()
            .is_none()
    );
    assert!(owned_identity_sequence(
        &catalog,
        &CatalogRequest::columns(["column_name".into()]),
        "public.items",
        &column
    )
    .unwrap()
    .is_none());
    column.auto_increment.as_mut().unwrap().sequence = Some("owned_sequence".into());
    assert!(
        owned_identity_sequence(&catalog, &request, "public.items", &column)
            .unwrap()
            .is_none()
    );
}
