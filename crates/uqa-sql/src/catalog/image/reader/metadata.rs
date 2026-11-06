//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native child iterators resume without a catalog prewalk or output allocation.

use super::{Acl, CatalogImageReader, Columns, Entries, Fields};
use crate::{
    ast::{ColumnType, ObjectAclEntry},
    catalog::{
        composite_type::StoredCompositeAttribute, enum_type::StoredEnumLabel,
        graph_oids::LabelCatalogOids, range_type::StoredMultirange,
    },
};
use std::slice::Iter;
use uqa_core::memory::BudgetedMapIter;

pub(super) enum Tail<'a> {
    None,
    Composite(Iter<'a, StoredCompositeAttribute>),
    Multirange(Option<&'a StoredMultirange>),
    Enum(Iter<'a, StoredEnumLabel>),
    Server {
        options: BudgetedMapIter<'a, String, String>,
        order: Option<Iter<'a, String>>,
    },
    Graph(BudgetedMapIter<'a, u32, LabelCatalogOids>),
}

impl<'a> Tail<'a> {
    pub(super) fn next_fields(
        &mut self,
        fields: &mut Fields<'a>,
        columns: &mut Columns<'a>,
        column_type: &mut Option<&'a ColumnType>,
    ) -> bool {
        match self {
            Self::None => {}
            Self::Composite(attributes) => {
                if let Some(attribute) = attributes.next() {
                    fields.composite_attribute(attribute);
                    *column_type = Some(&attribute.ty);
                    return true;
                }
            }
            Self::Multirange(value) => {
                if let Some(value) = value.take() {
                    fields.multirange(value);
                    return true;
                }
            }
            Self::Enum(labels) => {
                if let Some(label) = labels.next() {
                    fields.enum_label(label);
                    return true;
                }
            }
            Self::Server { options, order } => {
                if let Some((key, value)) = options.next() {
                    fields.text(key);
                    fields.text(value);
                    return true;
                }
                if let Some(name) = order.as_mut().and_then(Iterator::next) {
                    fields.text(name);
                    return true;
                }
            }
            Self::Graph(labels) => {
                if let Some((key, label)) = labels.next() {
                    fields.graph_label(*key, label);
                    *columns = Columns::Graph(label.not_null.iter());
                    return true;
                }
            }
        }
        *self = Self::None;
        false
    }
}

impl<'a> CatalogImageReader<'a> {
    fn optional_object_acl(&mut self, acl: Option<&'a [ObjectAclEntry]>) {
        self.fields.flag(acl.is_some());
        if let Some(acl) = acl {
            self.fields.count(acl.len());
            self.acl = Acl::Object(acl.iter());
        }
    }

    pub(super) fn next_metadata(&mut self) -> bool {
        match &mut self.entries {
            Entries::Composites(entries) => {
                let Some((key, value)) = entries.next() else {
                    return false;
                };
                self.fields.composite(key, value);
                self.tail = Tail::Composite(value.attributes.iter());
                self.optional_object_acl(value.usage_acl.as_deref());
            }
            Entries::Ranges(entries) => {
                let Some((key, value)) = entries.next() else {
                    return false;
                };
                self.fields.range(key, value);
                self.tail = Tail::Multirange(Some(&value.multirange));
                self.column_type = Some(&value.subtype);
                self.optional_object_acl(value.usage_acl.as_deref());
            }
            Entries::Enums(entries) => {
                let Some((key, value)) = entries.next() else {
                    return false;
                };
                self.fields.enum_type(key, value);
                self.tail = Tail::Enum(value.labels.iter());
                self.optional_object_acl(value.usage_acl.as_deref());
            }
            Entries::Shells(entries) => {
                let Some((key, value)) = entries.next() else {
                    return false;
                };
                self.fields.shell(key, value);
                self.optional_object_acl(value.usage_acl.as_deref());
            }
            Entries::ForeignServers(entries) => {
                let Some((key, value)) = entries.next() else {
                    return false;
                };
                self.fields.foreign_server(key, value);
                self.tail = Tail::Server {
                    options: value.options.iter(),
                    order: value
                        .metadata
                        .option_order
                        .as_ref()
                        .map(|order| order.iter()),
                };
            }
            Entries::GraphOids(entries) => {
                let Some((key, value)) = entries.next() else {
                    return false;
                };
                self.fields.graph(key, value);
                self.tail = Tail::Graph(value.labels.iter());
            }
            Entries::Roles(_)
            | Entries::Memberships(_)
            | Entries::Schemas(_)
            | Entries::Database(_)
            | Entries::BuiltinRoutines(_)
            | Entries::SystemRelations(_) => {
                unreachable!("security entries are emitted by their native cursor")
            }
        }
        true
    }
}
