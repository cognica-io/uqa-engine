//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected collation metadata follows logical bindings independently of value slots.

use super::{
    ColumnIdentity, ColumnType, HashMap, InternalColumnRef, RowSchema, SchemaLayoutError,
    SchemaLayoutResult,
};
use crate::catalog::type_metadata::pg_type_collation_oid;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Collations(Option<Box<Bindings>>);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Bindings {
    columns: BTreeMap<usize, u32>,
    aliases: BTreeMap<ColumnIdentity, u32>,
    internal: BTreeMap<InternalColumnRef, u32>,
    virtual_columns: BTreeMap<ColumnIdentity, u32>,
}

/// A selected collation belongs to a logical identity even when its physical value is NULL or shared with another identity.
#[derive(Clone, Copy, Debug)]
pub enum CollationBinding<'a> {
    Column(usize),
    Alias(&'a ColumnIdentity),
    Internal(InternalColumnRef),
}

fn type_default(ty: Option<&ColumnType>) -> Option<u32> {
    ty.map(|ty| u32::try_from(pg_type_collation_oid(ty)).expect("type collation identity"))
}

fn override_value(oid: Option<u32>, ty: Option<&ColumnType>) -> Option<u32> {
    oid.filter(|oid| {
        ty.is_some_and(|ty| {
            crate::schema::composites::type_is_collatable(ty)
                && type_default(Some(ty)) != Some(*oid)
        })
    })
}

impl Collations {
    pub(super) fn joined(left: &RowSchema, right: &RowSchema) -> Self {
        let mut result = left.index.cold.collations.clone();
        if let Some(target) = &mut result.0 {
            if !target.aliases.is_empty() {
                for identity in right.index.cold.aliases.keys() {
                    target.aliases.remove(identity);
                }
            }
            if !target.virtual_columns.is_empty() {
                for identity in right.index.cold.binding_only.keys() {
                    target.virtual_columns.remove(identity);
                }
            }
        }
        if let Some(source) = &right.index.cold.collations.0 {
            let target = result.0.get_or_insert_with(Default::default);
            target.columns.extend(
                source
                    .columns
                    .iter()
                    .map(|(position, oid)| (left.len() + position, *oid)),
            );
            target.aliases.extend(
                source
                    .aliases
                    .iter()
                    .map(|(identity, oid)| (identity.clone(), *oid)),
            );
            target
                .internal
                .extend(source.internal.iter().map(|(column, oid)| (*column, *oid)));
            target.virtual_columns.extend(
                source
                    .virtual_columns
                    .iter()
                    .map(|(identity, oid)| (identity.clone(), *oid)),
            );
        }
        result.compact();
        result
    }

    fn compact(&mut self) {
        if self.0.as_ref().is_some_and(|bindings| {
            bindings.columns.is_empty()
                && bindings.aliases.is_empty()
                && bindings.internal.is_empty()
                && bindings.virtual_columns.is_empty()
        }) {
            self.0 = None;
        }
    }

    pub(super) fn column(&mut self, position: usize, oid: Option<u32>, ty: Option<&ColumnType>) {
        let value = override_value(oid, ty);
        if let Some(value) = value {
            self.0
                .get_or_insert_with(Default::default)
                .columns
                .insert(position, value);
        } else if let Some(bindings) = &mut self.0 {
            bindings.columns.remove(&position);
        }
    }

    pub(super) fn alias(
        &mut self,
        identity: &ColumnIdentity,
        oid: Option<u32>,
        ty: Option<&ColumnType>,
    ) {
        let value = override_value(oid, ty);
        if let Some(value) = value {
            self.0
                .get_or_insert_with(Default::default)
                .aliases
                .insert(identity.clone(), value);
        } else if let Some(bindings) = &mut self.0 {
            bindings.aliases.remove(identity);
        }
    }

    pub(super) fn internal(
        &mut self,
        column: InternalColumnRef,
        oid: Option<u32>,
        ty: Option<&ColumnType>,
    ) {
        let value = override_value(oid, ty);
        if let Some(value) = value {
            self.0
                .get_or_insert_with(Default::default)
                .internal
                .insert(column, value);
        } else if let Some(bindings) = &mut self.0 {
            bindings.internal.remove(&column);
        }
    }

    pub(super) fn virtual_column(
        &mut self,
        identity: &ColumnIdentity,
        oid: Option<u32>,
        ty: Option<&ColumnType>,
    ) {
        let value = override_value(oid, ty);
        if let Some(value) = value {
            self.0
                .get_or_insert_with(Default::default)
                .virtual_columns
                .insert(identity.clone(), value);
        } else if let Some(bindings) = &mut self.0 {
            bindings.virtual_columns.remove(identity);
        }
    }

    pub(super) fn clear_columns(&mut self) {
        if let Some(bindings) = &mut self.0 {
            bindings.columns.clear();
        }
    }

    /// Schema construction discards metadata for removed identities and defaults. Schemas with only type-default collations have no extra allocation.
    pub(super) fn retain(
        &mut self,
        columns: &[Option<ColumnType>],
        aliases: &HashMap<ColumnIdentity, Option<ColumnType>>,
        internal: &HashMap<InternalColumnRef, Option<ColumnType>>,
        virtual_columns: &HashMap<ColumnIdentity, Option<ColumnType>>,
    ) {
        let Some(bindings) = &mut self.0 else {
            return;
        };
        bindings.columns.retain(|position, oid| {
            override_value(Some(*oid), columns.get(*position).and_then(Option::as_ref)).is_some()
        });
        bindings.aliases.retain(|identity, oid| {
            override_value(Some(*oid), aliases.get(identity).and_then(Option::as_ref)).is_some()
        });
        bindings.internal.retain(|column, oid| {
            override_value(Some(*oid), internal.get(column).and_then(Option::as_ref)).is_some()
        });
        bindings.virtual_columns.retain(|identity, oid| {
            override_value(
                Some(*oid),
                virtual_columns.get(identity).and_then(Option::as_ref),
            )
            .is_some()
        });
        if bindings.columns.is_empty()
            && bindings.aliases.is_empty()
            && bindings.internal.is_empty()
            && bindings.virtual_columns.is_empty()
        {
            self.0 = None;
        }
    }
}

impl RowSchema {
    /// Observe retained expression metadata or a direct logical reference, including selected clauses, static bindings and executor attributes without a visible output position.
    pub fn input_collation(&self, expression: &crate::ScalarExpr) -> Option<u32> {
        match expression {
            crate::ScalarExpr::TypedLiteral { collation_oid, .. } => *collation_oid,
            crate::ScalarExpr::Collated { collation, .. } => Some(collation.oid()),
            crate::ScalarExpr::Func {
                binding: Some(binding),
                ..
            } => match binding.dispatch {
                Some(crate::ast::FunctionDispatch::Collate { oid }) => oid,
                _ => None,
            },
            crate::ScalarExpr::Column(name) => self.collation_of(name),
            crate::ScalarExpr::QualifiedColumn { qualifier, column } => {
                self.qualified_collation(qualifier, column)
            }
            crate::ScalarExpr::Position(position) => self.column_collation(*position),
            crate::ScalarExpr::InternalColumn(column) => self.internal_collation(*column),
            _ => None,
        }
    }

    /// Bind a direct scalar reference without confusing its SQL identity with another logical column that shares the same physical value. Static binding-only names remain with expression evaluation.
    pub fn projection_input(&self, expression: &crate::ScalarExpr) -> Option<super::ProjectedSlot> {
        let (logical, alias) = match expression {
            crate::ScalarExpr::Column(name) => {
                if self.column_is_ambiguous(name) {
                    return None;
                }
                let logical = self.unqualified_position(name);
                (
                    logical,
                    logical.is_none().then(|| ColumnIdentity::unqualified(name)),
                )
            }
            crate::ScalarExpr::QualifiedColumn { qualifier, column } => {
                if self.qualified_column_is_ambiguous(qualifier, column) {
                    return None;
                }
                let logical = self.qualified_position(qualifier, column);
                (
                    logical,
                    logical
                        .is_none()
                        .then(|| ColumnIdentity::qualified(qualifier, column)),
                )
            }
            crate::ScalarExpr::Position(position) if *position < self.len() => {
                (Some(*position), None)
            }
            _ => return None,
        };
        let (physical, collation) = if let Some(position) = logical {
            (
                self.physical_slot(position),
                self.column_collation(position),
            )
        } else {
            let identity = alias?;
            let slot = *self.index.aliases.get(&identity)?;
            (
                (slot != super::NULL_SLOT).then_some(slot),
                self.alias_collation(&identity),
            )
        };
        Some(super::ProjectedSlot::BoundInput {
            physical,
            collation,
        })
    }

    /// Attach already-selected column collations to an owned schema. The width and collatability must match its declared types; catalog name resolution belongs to the caller's SQL analysis.
    pub fn with_column_collations(
        mut self,
        collations: impl IntoIterator<Item = Option<u32>>,
    ) -> Self {
        let mut width = 0;
        let index = std::sync::Arc::make_mut(&mut self.index);
        for (position, oid) in collations.into_iter().enumerate() {
            assert!(
                position < index.columns.len(),
                "row schema column/collation width mismatch"
            );
            let ty = index.cold.columns[position].as_ref();
            assert!(
                oid.is_none_or(
                    |oid| oid == 0 || ty.is_some_and(crate::schema::composites::type_is_collatable)
                ),
                "collation requires a collatable column type"
            );
            index.cold.collations.column(position, oid, ty);
            width += 1;
        }
        assert_eq!(
            width,
            index.columns.len(),
            "row schema column/collation width mismatch"
        );
        index.cold.collations.compact();
        self
    }

    /// Select a collation for an existing static binding without adding a physical slot or wildcard output.
    pub fn with_virtual_collation(
        mut self,
        identity: &ColumnIdentity,
        oid: u32,
    ) -> SchemaLayoutResult<Self> {
        let ty = self
            .index
            .cold
            .binding_only
            .get(identity)
            .and_then(Option::as_ref);
        if ty.is_none_or(|ty| !crate::schema::composites::type_is_collatable(ty)) {
            return Err(SchemaLayoutError(
                "collation metadata requires an existing collatable virtual binding".into(),
            ));
        }
        let index = std::sync::Arc::make_mut(&mut self.index);
        index.cold.collations.virtual_column(
            identity,
            Some(oid),
            index
                .cold
                .binding_only
                .get(identity)
                .and_then(Option::as_ref),
        );
        index.cold.collations.compact();
        Ok(self)
    }

    pub fn column_collation(&self, position: usize) -> Option<u32> {
        self.index
            .cold
            .collations
            .0
            .as_ref()
            .and_then(|bindings| bindings.columns.get(&position).copied())
            .or_else(|| type_default(self.column_type(position)))
    }

    pub fn internal_collation(&self, column: InternalColumnRef) -> Option<u32> {
        self.index
            .cold
            .collations
            .0
            .as_ref()
            .and_then(|bindings| bindings.internal.get(&column).copied())
            .or_else(|| type_default(self.internal_type(column)))
    }

    pub fn collation_of(&self, name: &str) -> Option<u32> {
        if self.column_is_ambiguous(name) {
            return None;
        }
        self.index
            .unqualified
            .get(name)
            .and_then(|position| self.column_collation(*position))
            .or_else(|| self.hidden_collation(&ColumnIdentity::unqualified(name)))
    }

    pub fn qualified_collation(&self, qualifier: &str, name: &str) -> Option<u32> {
        let identity = ColumnIdentity::qualified(qualifier, name);
        if self.index.ambiguous_qualified.contains(&identity) {
            return None;
        }
        self.index
            .qualified
            .get(&identity)
            .and_then(|position| self.column_collation(*position))
            .or_else(|| self.hidden_collation(&identity))
    }

    fn hidden_collation(&self, identity: &ColumnIdentity) -> Option<u32> {
        self.alias_collation(identity)
            .or_else(|| self.virtual_collation(identity))
    }

    pub(super) fn virtual_collation(&self, identity: &ColumnIdentity) -> Option<u32> {
        self.index
            .cold
            .collations
            .0
            .as_ref()
            .and_then(|bindings| bindings.virtual_columns.get(identity).copied())
            .or_else(|| {
                type_default(
                    self.index
                        .cold
                        .binding_only
                        .get(identity)
                        .and_then(Option::as_ref),
                )
            })
    }

    pub(super) fn alias_collation(&self, identity: &ColumnIdentity) -> Option<u32> {
        self.index
            .cold
            .collations
            .0
            .as_ref()
            .and_then(|bindings| bindings.aliases.get(identity).copied())
            .or_else(|| {
                type_default(
                    self.index
                        .cold
                        .aliases
                        .get(identity)
                        .and_then(Option::as_ref),
                )
            })
    }

    pub(super) fn inherited_column_collation(&self, position: usize) -> Option<u32> {
        self.column_type(position)
            .filter(|ty| crate::schema::composites::type_is_collatable(ty))
            .and_then(|_| self.column_collation(position))
    }

    pub(super) fn inherited_physical_collation(&self, physical: usize) -> Option<u32> {
        if physical == super::NULL_SLOT {
            return None;
        }
        self.index
            .slots
            .iter()
            .position(|slot| *slot == physical)
            .and_then(|position| self.inherited_column_collation(position))
            .or_else(|| {
                self.index.aliases.iter().find_map(|(identity, slot)| {
                    (*slot == physical
                        && self
                            .index
                            .cold
                            .aliases
                            .get(identity)
                            .and_then(Option::as_ref)
                            .is_some_and(crate::schema::composites::type_is_collatable))
                    .then(|| self.alias_collation(identity))
                    .flatten()
                })
            })
            .or_else(|| {
                self.index
                    .executor_attributes
                    .iter()
                    .find_map(|(column, slot)| {
                        (*slot == physical
                            && self
                                .internal_type(*column)
                                .is_some_and(crate::schema::composites::type_is_collatable))
                        .then(|| self.internal_collation(*column))
                        .flatten()
                    })
            })
    }

    /// Physical spill metadata excludes static name-binding-only identities, like the existing spill type layout. Type defaults need no extra record payload.
    pub fn collation_overrides(&self) -> impl Iterator<Item = (CollationBinding<'_>, u32)> {
        self.index.cold.collations.0.iter().flat_map(|bindings| {
            bindings
                .columns
                .iter()
                .map(|(column, oid)| (CollationBinding::Column(*column), *oid))
                .chain(
                    bindings
                        .aliases
                        .iter()
                        .map(|(identity, oid)| (CollationBinding::Alias(identity), *oid)),
                )
                .chain(
                    bindings
                        .internal
                        .iter()
                        .map(|(column, oid)| (CollationBinding::Internal(*column), *oid)),
                )
        })
    }

    /// Restore one selected collation only for a binding that exists in this layout. Invalid spill metadata cannot invent columns or attach a collation to a noncollatable type.
    pub fn with_collation_override(
        mut self,
        binding: CollationBinding<'_>,
        oid: u32,
    ) -> SchemaLayoutResult<Self> {
        let ty = match binding {
            CollationBinding::Column(position) => self.column_type(position),
            CollationBinding::Alias(identity) => self
                .index
                .cold
                .aliases
                .get(identity)
                .and_then(Option::as_ref),
            CollationBinding::Internal(column) => self.internal_type(column),
        };
        if ty.is_none_or(|ty| !crate::schema::composites::type_is_collatable(ty)) {
            return Err(SchemaLayoutError(
                "collation metadata requires an existing collatable binding".into(),
            ));
        }
        let value = override_value(Some(oid), ty);
        let index = std::sync::Arc::make_mut(&mut self.index);
        match binding {
            CollationBinding::Column(position) => {
                index.cold.collations.column(
                    position,
                    value,
                    index.cold.columns[position].as_ref(),
                );
            }
            CollationBinding::Alias(identity) => index.cold.collations.alias(
                identity,
                value,
                index.cold.aliases.get(identity).and_then(Option::as_ref),
            ),
            CollationBinding::Internal(column) => index.cold.collations.internal(
                column,
                value,
                index
                    .cold
                    .executor_attribute_types
                    .get(&column)
                    .and_then(Option::as_ref),
            ),
        }
        index.cold.collations.compact();
        Ok(self)
    }
}

#[cfg(test)]
mod tests;
