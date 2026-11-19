//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Standalone composite and range metadata retain original identities and selected support.

use super::Fields;
use crate::catalog::{
    composite_type::{StoredComposite, StoredCompositeAttribute},
    range_type::{StoredMultirange, StoredRange},
};

impl<'a> Fields<'a> {
    pub fn composite(&mut self, key: &'a str, value: &'a StoredComposite) {
        let StoredComposite {
            object_id,
            oid,
            relation_oid,
            array_oid,
            array_name,
            identity,
            owner,
            attributes,
            usage_acl: _,
        } = value;
        self.text(key);
        self.scalar(*object_id);
        self.scalar(oid.to_le_bytes());
        self.scalar(relation_oid.to_le_bytes());
        self.scalar(array_oid.to_le_bytes());
        self.text(array_name);
        self.relation_identity(identity);
        self.identity(*owner);
        self.count(attributes.len());
    }
    pub fn composite_attribute(&mut self, value: &'a StoredCompositeAttribute) {
        let StoredCompositeAttribute {
            name,
            ty: _,
            collation,
            collation_oid,
            number,
            dropped,
        } = value;
        self.text(name);
        self.optional_text(collation.as_deref());
        self.optional_u32(*collation_oid);
        self.scalar(number.to_le_bytes());
        self.flag(*dropped);
    }
    pub fn range(&mut self, key: &'a str, value: &'a StoredRange) {
        let StoredRange {
            object_id,
            oid,
            array_oid,
            array_name,
            identity,
            owner,
            subtype: _,
            subtype_operator_class,
            collation_oid,
            canonical_oid,
            subtype_difference_oid,
            multirange: _,
            usage_acl: _,
        } = value;
        self.text(key);
        self.scalar(*object_id);
        self.scalar(oid.to_le_bytes());
        self.scalar(array_oid.to_le_bytes());
        self.text(array_name);
        self.relation_identity(identity);
        self.identity(*owner);
        self.scalar(subtype_operator_class.to_le_bytes());
        self.scalar(collation_oid.to_le_bytes());
        self.optional_u32(*canonical_oid);
        self.optional_u32(*subtype_difference_oid);
    }
    pub fn multirange(&mut self, value: &'a StoredMultirange) {
        let StoredMultirange {
            object_id,
            oid,
            array_oid,
            array_name,
            identity,
        } = value;
        self.scalar(*object_id);
        self.scalar(oid.to_le_bytes());
        self.scalar(array_oid.to_le_bytes());
        self.text(array_name);
        self.relation_identity(identity);
    }
}
