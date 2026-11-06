//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exhaustive projections preserve native type, foreign-server and graph metadata.

use super::Fields;
use crate::catalog::{
    enum_type::{StoredEnum, StoredEnumLabel},
    foreign_server::{ForeignServerDefinition, ForeignServerMetadata},
    foreign_wrapper::ForeignWrapperReference,
    graph_oids::{EndpointIndexOids, GraphCatalogOids, IndexConstraintOids, LabelCatalogOids},
    relation_oids::RelationCatalogOids,
    shell_type::StoredShellType,
};
use uqa_core::RelationIdentity;

impl<'a> Fields<'a> {
    fn relation_identity(&mut self, identity: &'a RelationIdentity) {
        let RelationIdentity { schema, name } = identity;
        self.text(schema);
        self.text(name);
    }

    pub fn enum_type(&mut self, key: &'a str, value: &'a StoredEnum) {
        let StoredEnum {
            object_id,
            oid,
            array_oid,
            array_name,
            identity,
            owner,
            labels,
            usage_acl: _,
        } = value;
        self.text(key);
        self.scalar(*object_id);
        self.scalar(oid.to_le_bytes());
        self.scalar(array_oid.to_le_bytes());
        self.text(array_name);
        self.relation_identity(identity);
        self.identity(*owner);
        self.count(labels.len());
    }

    pub fn enum_label(&mut self, value: &'a StoredEnumLabel) {
        let StoredEnumLabel {
            oid,
            key,
            label,
            sort_order,
        } = value;
        self.scalar(oid.to_le_bytes());
        self.blob(key.as_bytes());
        self.text(label);
        self.scalar(sort_order.to_bits().to_le_bytes());
    }

    pub fn shell(&mut self, key: &'a str, value: &'a StoredShellType) {
        let StoredShellType {
            object_id,
            oid,
            identity,
            owner,
            usage_acl: _,
        } = value;
        self.text(key);
        self.scalar(*object_id);
        self.scalar(oid.to_le_bytes());
        self.relation_identity(identity);
        self.identity(*owner);
    }

    pub fn foreign_server(&mut self, key: &'a str, value: &'a ForeignServerDefinition) {
        let ForeignServerDefinition {
            name,
            fdw_type,
            options,
            metadata,
        } = value;
        let ForeignServerMetadata {
            oid,
            object_id,
            owner,
            server_type,
            version,
            option_order,
            wrapper_reference,
        } = metadata;
        self.text(key);
        self.text(name);
        self.text(fdw_type);
        self.scalar(oid.to_le_bytes());
        self.scalar(*object_id);
        self.identity(*owner);
        self.optional_text(server_type.as_deref());
        self.optional_text(version.as_deref());
        self.flag(wrapper_reference.is_some());
        if let Some(ForeignWrapperReference { oid, object_id }) = wrapper_reference {
            self.scalar(oid.to_le_bytes());
            self.scalar(*object_id);
        }
        self.count(options.len());
        self.flag(option_order.is_some());
        if let Some(order) = option_order {
            self.count(order.len());
        }
    }

    pub fn graph(&mut self, key: &'a str, value: &'a GraphCatalogOids) {
        let GraphCatalogOids {
            namespace,
            label_sequence,
            labels,
        } = value;
        self.text(key);
        self.scalar(namespace.to_le_bytes());
        self.scalar(label_sequence.to_le_bytes());
        self.count(labels.len());
    }

    pub fn graph_label(&mut self, key: u32, value: &'a LabelCatalogOids) {
        let LabelCatalogOids {
            sequence,
            relation,
            id_default,
            properties_default,
            not_null,
            toast_table,
            toast_index,
            primary_key,
            endpoint_indexes,
            trigger,
        } = value;
        let RelationCatalogOids {
            relation,
            row_type,
            array_type,
            rule,
        } = relation;
        self.scalar(key.to_le_bytes());
        self.scalar(sequence.to_le_bytes());
        self.scalar(relation.to_le_bytes());
        self.optional_u32(*row_type);
        self.optional_u32(*array_type);
        self.optional_u32(*rule);
        self.scalar(id_default.to_le_bytes());
        self.scalar(properties_default.to_le_bytes());
        self.scalar(toast_table.to_le_bytes());
        self.scalar(toast_index.to_le_bytes());
        self.flag(primary_key.is_some());
        if let Some(IndexConstraintOids { index, constraint }) = primary_key {
            self.scalar(index.to_le_bytes());
            self.scalar(constraint.to_le_bytes());
        }
        self.flag(endpoint_indexes.is_some());
        if let Some(EndpointIndexOids { start_id, end_id }) = endpoint_indexes {
            self.scalar(start_id.to_le_bytes());
            self.scalar(end_id.to_le_bytes());
        }
        self.scalar(trigger.to_le_bytes());
        self.count(not_null.len());
    }
}
