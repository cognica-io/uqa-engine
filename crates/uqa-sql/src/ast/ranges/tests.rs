//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{UserTypeIdentity, UserTypeKind};
use crate::catalog::type_metadata::{
    pg_type_array_oid, pg_type_by_value, pg_type_len, pg_type_oid,
};
use uqa_core::{memory::MemoryBudget, CancellationToken, RelationIdentity};

fn reference() -> RangeTypeReference {
    RangeTypeReference {
        schema: "range_values".into(),
        name: "integer_window".into(),
        oid: 27_001,
        array_oid: 27_002,
        alignment: RangeAlignment::Integer,
    }
}

#[test]
fn declared_range_identity_survives_rename_arrays_and_controlled_copy() {
    let original = ColumnType::DeclaredRange(reference());
    let mut renamed = ColumnType::Array(Box::new(original.clone()));
    assert!(renamed.rename_user_type(27_001, &RelationIdentity::new("other", "window")));
    let ColumnType::Array(element) = &renamed else {
        panic!("array")
    };
    assert_eq!(**element, original);
    assert_eq!(element.display_name(), "other.\"window\"");
    assert_eq!(renamed.catalog_name(), "range#27001[]");
    assert_eq!(
        UserTypeIdentity::parse(&renamed.catalog_name()),
        Some(UserTypeIdentity {
            kind: UserTypeKind::Range,
            oid: 27_001,
            dimensions: 1
        })
    );
    let mut distinct = reference();
    distinct.oid += 10;
    assert_ne!(original, ColumnType::DeclaredRange(distinct));
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let copy = renamed
        .clone_with_control(&ProductionControl::new(&memory, &token, &token))
        .unwrap();
    assert_eq!(*copy, renamed);
    assert_eq!(memory.used(), copy.reserved_bytes());
    drop(copy);
    assert_eq!(memory.used(), 0);
    let empty = MemoryBudget::new(0);
    assert!(renamed
        .clone_with_control(&ProductionControl::new(&empty, &token, &token))
        .is_err());
    assert_eq!(empty.used(), 0);
    token.cancel();
    assert!(renamed
        .clone_with_control(&ProductionControl::new(&memory, &token, &token))
        .is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn range_and_multirange_keep_distinct_catalog_and_array_identities() {
    let range = ColumnType::DeclaredRange(reference());
    let mut reference = reference();
    reference.oid += 2;
    reference.array_oid += 2;
    reference.name = "windows".into();
    let multirange = ColumnType::DeclaredMultirange(reference);
    assert_ne!(range, multirange);
    assert_eq!(pg_type_oid(&range), 27_001);
    assert_eq!(pg_type_oid(&multirange), 27_003);
    assert_eq!(pg_type_array_oid(&range), 27_002);
    assert_eq!(pg_type_array_oid(&multirange), 27_004);
    assert_eq!(multirange.catalog_name(), "multirange#27003");
    assert_eq!(
        UserTypeIdentity::parse(&multirange.catalog_name())
            .unwrap()
            .kind,
        UserTypeKind::Multirange
    );
    for ty in [range, multirange] {
        assert_eq!(pg_type_len(&ty), -1);
        assert!(!pg_type_by_value(&ty));
    }
}

#[test]
fn declared_range_references_require_a_current_definition_before_publication() {
    struct Catalog(Option<ColumnType>);
    impl crate::type_resolution::FunctionTypeResolver for Catalog {
        fn resolve_type_name(&self, _: &str) -> Result<Option<ColumnType>, crate::SQLError> {
            Ok(self.0.clone())
        }
        fn resolve_function_type(
            &self,
            _: &str,
            _: Option<&crate::ast::FunctionBinding>,
            _: &[Option<String>],
            _: &[Option<ColumnType>],
            _: bool,
        ) -> Result<Option<ColumnType>, crate::SQLError> {
            unreachable!()
        }
    }
    let range = ColumnType::DeclaredRange(reference());
    let array = ColumnType::Array(Box::new(range.clone()));
    let mut domain = ColumnType::Domain {
        schema: "public".into(),
        name: "positive_window".into(),
        oid: 27_005,
        array_oid: None,
        collation_oid: None,
        base: Box::new(array.clone()),
    };
    for ty in [&range, &array, &domain] {
        let error =
            crate::type_resolution::resolve_declared_column_type(&Catalog(None), ty).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42704"));
        assert_eq!(error.to_string(), "type with OID 27001 does not exist");
        assert!(crate::type_resolution::resolve_alter_column_type(&Catalog(None), ty).is_err());
    }
    let mut renamed = array.clone();
    renamed.rename_user_type(27_001, &RelationIdentity::new("other", "current_window"));
    let resolved = crate::type_resolution::resolve_declared_column_type(
        &Catalog(Some(renamed.clone())),
        &domain,
    )
    .unwrap();
    let ColumnType::Domain { base, .. } = &mut domain else {
        unreachable!()
    };
    **base = renamed;
    assert_eq!(resolved.catalog_name(), domain.catalog_name());
    let ColumnType::Domain { base, .. } = resolved else {
        unreachable!()
    };
    assert_eq!(base.display_name(), "other.current_window[]");
    assert!(crate::type_resolution::resolve_declared_column_type(
        &Catalog(Some(ColumnType::Text)),
        &range
    )
    .is_err());
}
