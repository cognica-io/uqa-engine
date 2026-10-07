//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{AlterTypeObjectAction, CompositeAttributeAddition, FunctionBinding, Statement};
use crate::catalog::composite_type::StoredComposite;
use crate::expr::composites::{CompositeTypeCatalog, CompositeTypeDescriptor};
use std::sync::Arc;

struct Types;
impl FunctionTypeResolver for Types {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}
impl CompositeTypeCatalog for Types {
    fn composite_type(&self, _: u32) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        Ok(None)
    }
}

fn definition() -> StoredComposite {
    StoredComposite {
        object_id: [1; 16],
        oid: 20_001,
        relation_oid: 20_002,
        array_oid: 20_003,
        array_name: "_ca_pair".into(),
        identity: uqa_core::RelationIdentity::new("public", "ca_pair"),
        owner: uqa_core::catalog_role::RoleIdentity::BOOTSTRAP,
        usage_acl: None,
        attributes: vec![StoredCompositeAttribute {
            name: "a".into(),
            ty: ColumnType::Integer,
            collation: None,
            number: 1,
            dropped: false,
        }],
    }
}
fn addition(sql: &str) -> CompositeAttributeAddition {
    let Statement::AlterTypeObject(statement) = crate::compile(sql).unwrap().remove(0) else {
        panic!("ALTER TYPE")
    };
    let AlterTypeObjectAction::AddAttributes(mut attributes) = statement.action else {
        panic!("ADD ATTRIBUTE")
    };
    attributes.remove(0)
}

#[test]
fn duplicate_name_precedes_type_and_serial_declaration_errors() {
    // Independently captured in composite_attribute_add_oracle.expected.json.
    for ty in ["ca_missing", "numeric(0)", "serial", "serial[]"] {
        let attribute = addition(&format!("ALTER TYPE ca_pair ADD ATTRIBUTE a {ty}"));
        let error = prepare_added_attribute(&Types, &Types, &definition(), &attribute).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42701"));
        assert_eq!(
            error.to_string(),
            "column \"a\" of relation \"ca_pair\" already exists"
        );
    }
}

#[test]
fn additions_keep_dropped_attribute_numbers_and_reject_recursive_shapes() {
    let mut definition = definition();
    definition.attributes.push(StoredCompositeAttribute {
        name: StoredCompositeAttribute::dropped_name(3),
        ty: ColumnType::Text,
        collation: None,
        number: 3,
        dropped: true,
    });
    let mut attribute = addition("ALTER TYPE ca_pair ADD ATTRIBUTE b integer");
    assert_eq!(
        prepare_added_attribute(&Types, &Types, &definition, &attribute)
            .unwrap()
            .number,
        4
    );
    attribute.attribute.ty = ColumnType::Array(Box::new(definition.column_type()));
    let error = prepare_added_attribute(&Types, &Types, &definition, &attribute).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42P16"));
    assert_eq!(
        error.to_string(),
        "composite type ca_pair cannot be made a member of itself"
    );
}

#[test]
fn serial_lowering_is_exclusive_to_attribute_addition() {
    let added = addition("ALTER TYPE ca_pair ADD ATTRIBUTE b serial");
    assert!(added.declaration.serial);
    assert_eq!(
        prepare_added_attribute(&Types, &Types, &definition(), &added)
            .unwrap()
            .ty,
        ColumnType::Integer
    );
    let qualified = addition("ALTER TYPE ca_pair ADD ATTRIBUTE b pg_catalog.serial");
    assert!(!qualified.declaration.serial);
    assert!(matches!(qualified.attribute.ty, ColumnType::Named(_)));
    let array = addition("ALTER TYPE ca_pair ADD ATTRIBUTE b serial[]");
    assert_eq!(
        prepare_added_attribute(&Types, &Types, &definition(), &array)
            .unwrap_err()
            .sqlstate(),
        Some("0A000")
    );
    let Statement::CreateCompositeType(created) =
        crate::compile("CREATE TYPE ca_pair AS (a serial)")
            .unwrap()
            .remove(0)
    else {
        panic!("CREATE TYPE")
    };
    assert!(matches!(created.attributes[0].ty, ColumnType::Named(_)));
}
