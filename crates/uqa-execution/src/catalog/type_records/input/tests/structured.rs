//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::{
    catalog::{
        composite_type::{StoredComposite, StoredCompositeAttribute},
        range_type::{StoredMultirange, StoredRange},
    },
    ColumnType,
};

impl Fixture for StoredRange {
    const FORMAT: TypeRecordFormat = TypeRecordFormat {
        label: "range",
        definition_context: "range definition",
        oid_context: "range OID",
        format_key: "sql_range_types_json",
        format_field: "range_catalog_format",
        version: 1,
        prefix: "uqa.sql.range.v1:",
        oid_prefix: "uqa.sql.range_oid.v1:",
    };
    fn sample(index: u32) -> (String, Self) {
        let oid = 20000 + index * 16;
        let identity = RelationIdentity::new("public", format!("range_{index:04}_한글"));
        let name = identity.qualified_name();
        let definition = Self {
            object_id: [u8::try_from(index * 2 + 1).unwrap(); 16],
            oid,
            array_oid: oid + 1,
            array_name: format!("_{}", identity.name),
            identity,
            owner: RoleIdentity::BOOTSTRAP,
            subtype: ColumnType::Array(Box::new(ColumnType::Integer)),
            subtype_operator_class: 1978,
            collation_oid: 0,
            canonical_oid: None,
            subtype_difference_oid: None,
            multirange: StoredMultirange {
                object_id: [u8::try_from(index * 2 + 2).unwrap(); 16],
                oid: oid + 2,
                array_oid: oid + 3,
                array_name: format!("_multi_{index}"),
                identity: RelationIdentity::new("other", format!("multi_{index}")),
            },
            usage_acl: None,
        };
        (name, definition)
    }
    fn restore(
        catalog: &dyn CatalogFacade,
        roles: &RoleDefinitions,
    ) -> StorageBackendResult<TypeDefinitions<Self>> {
        crate::catalog::range_type::restore(catalog, roles)
    }
}
impl Fixture for StoredComposite {
    const FORMAT: TypeRecordFormat = TypeRecordFormat {
        label: "composite",
        definition_context: "composite definition",
        oid_context: "composite OID",
        format_key: "sql_composites_json",
        format_field: "composite_catalog_format",
        version: 1,
        prefix: "uqa.sql.composite.v1:",
        oid_prefix: "uqa.sql.composite_oid.v1:",
    };
    fn sample(index: u32) -> (String, Self) {
        let oid = 20000 + index * 16;
        let identity = RelationIdentity::new("public", format!("composite_{index:04}_한글"));
        let name = identity.qualified_name();
        (
            name,
            Self {
                object_id: [u8::try_from(index + 1).unwrap(); 16],
                oid,
                array_oid: oid + 1,
                relation_oid: oid + 2,
                array_name: format!("_{}", identity.name),
                identity,
                owner: RoleIdentity::BOOTSTRAP,
                attributes: (1..=8)
                    .map(|number| StoredCompositeAttribute {
                        name: format!("a_{number}"),
                        ty: ColumnType::Array(Box::new(ColumnType::Integer)),
                        collation: None,
                        collation_oid: None,
                        number,
                        dropped: number % 2 == 0,
                    })
                    .collect(),
                usage_acl: None,
            },
        )
    }
    fn restore(
        catalog: &dyn CatalogFacade,
        roles: &RoleDefinitions,
    ) -> StorageBackendResult<TypeDefinitions<Self>> {
        crate::catalog::composite_type::restore(catalog, roles)
    }
}

#[test]
fn persistent_range_input_preserves_native_roots_claims_and_future_mutations() {
    native_roots::<StoredRange>();
}
#[test]
fn persistent_composite_input_preserves_native_roots_claims_and_future_mutations() {
    native_roots::<StoredComposite>();
}
#[test]
fn persistent_structured_type_input_retires_physical_and_native_prefixes() {
    terminal_owners::<StoredRange>();
    terminal_owners::<StoredComposite>();
}
#[test]
fn persistent_structured_type_input_preserves_marker_claim_and_semantic_error_order() {
    ordering::malformed::<StoredRange>();
    ordering::malformed::<StoredComposite>();
}
