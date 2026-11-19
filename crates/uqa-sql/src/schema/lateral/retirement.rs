//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Arc, Denial, DiagnosticNamespace, LateralRestrictions};
use crate::{
    ir::ownership::Retirement,
    plan::retirement::{scalar_fields, Pending, Retire},
    schema::{
        production::lease::SchemaMemory,
        retirement::{
            fields::{Input, Sequence},
            maps::Map,
        },
    },
};

impl Retire for Denial {
    fn retire_step(&mut self, _: &mut [usize], _: &mut Option<Retirement>) -> bool {
        true
    }
}

pub(in crate::schema) struct NamespaceFields {
    name: Box<str>,
    columns: Vec<Box<str>>,
    _memory: SchemaMemory,
}
impl NamespaceFields {
    fn take(namespace: &mut DiagnosticNamespace) -> Self {
        Self {
            name: std::mem::take(&mut namespace.name),
            columns: std::mem::take(&mut namespace.columns),
            _memory: std::mem::take(&mut namespace.memory),
        }
    }
}
impl Retire for NamespaceFields {
    fn retire_step(&mut self, positions: &mut [usize], scalar: &mut Option<Retirement>) -> bool {
        scalar_fields!(positions, scalar; self.name, self.columns)
    }
}
impl Input for Arc<DiagnosticNamespace> {
    type Retired = Option<Pending<NamespaceFields>>;
    fn into_retirement(self) -> Self::Retired {
        Arc::into_inner(self).map(|mut value| Pending::new(NamespaceFields::take(&mut value)))
    }
}
impl Drop for DiagnosticNamespace {
    fn drop(&mut self) {
        drop(Pending::new(NamespaceFields::take(self)));
    }
}

pub(in crate::schema) struct Fields {
    qualifiers: Map<Box<str>, Denial>,
    columns: Map<Box<str>, (Box<str>, Denial)>,
    ambiguous_qualifiers: Map<Box<str>, ()>,
    namespaces: Sequence<std::vec::IntoIter<Arc<DiagnosticNamespace>>>,
    column_namespaces: Sequence<std::vec::IntoIter<Arc<DiagnosticNamespace>>>,
    unavailable_ambiguous_columns: Map<Box<str>, ()>,
    unavailable_qualifiers: Map<Box<str>, bool>,
    unavailable_columns: Map<Box<str>, Vec<(Box<str>, bool)>>,
    _memory: SchemaMemory,
}
impl Fields {
    fn take(value: &mut LateralRestrictions) -> Self {
        Self {
            qualifiers: Sequence::new(std::mem::take(&mut value.qualifiers).into()),
            columns: Sequence::new(std::mem::take(&mut value.columns).into()),
            ambiguous_qualifiers: Sequence::new(
                std::mem::take(&mut value.ambiguous_qualifiers)
                    .into_map()
                    .into(),
            ),
            namespaces: Sequence::new(std::mem::take(&mut value.namespaces).into_iter()),
            column_namespaces: Sequence::new(
                std::mem::take(&mut value.column_namespaces).into_iter(),
            ),
            unavailable_ambiguous_columns: Sequence::new(
                std::mem::take(&mut value.unavailable_ambiguous_columns)
                    .into_map()
                    .into(),
            ),
            unavailable_qualifiers: Sequence::new(
                std::mem::take(&mut value.unavailable_qualifiers).into(),
            ),
            unavailable_columns: Sequence::new(
                std::mem::take(&mut value.unavailable_columns).into(),
            ),
            _memory: std::mem::take(&mut value.memory),
        }
    }
}
impl Retire for Fields {
    fn retire_step(&mut self, positions: &mut [usize], scalar: &mut Option<Retirement>) -> bool {
        scalar_fields!(positions, scalar; self.qualifiers, self.columns, self.ambiguous_qualifiers,
            self.namespaces, self.column_namespaces, self.unavailable_ambiguous_columns,
            self.unavailable_qualifiers, self.unavailable_columns)
    }
}
impl Input for Arc<LateralRestrictions> {
    type Retired = Option<Pending<Fields>>;
    fn into_retirement(self) -> Self::Retired {
        Arc::into_inner(self).map(|mut value| Pending::new(Fields::take(&mut value)))
    }
}
impl Drop for LateralRestrictions {
    fn drop(&mut self) {
        drop(Pending::new(Fields::take(self)));
    }
}
