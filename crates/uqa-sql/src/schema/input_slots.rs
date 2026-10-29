//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow input layouts while admitting only the name index needed by a concatenated row.

use super::{identity_lookup::ColumnKey, RowSchema, NULL_SLOT};
use crate::{ast::InternalColumnRef, SQLError, ScalarExpr};
use uqa_core::memory::{ProductionControl, ProductionMap};

#[derive(Clone, Copy)]
enum Binding {
    Unique(Option<usize>),
    Ambiguous,
}

enum Layout<'s, 'c> {
    Row(&'s RowSchema),
    Join {
        left: &'s RowSchema,
        right: &'s RowSchema,
        visible: ProductionMap<'c, (Option<&'s str>, &'s str), Binding>,
    },
}

/// Positional reference observation borrows column identities, aliases and physical layouts. A joined view admits its borrowed-key index before construction; it does not produce a new row descriptor or copy type metadata.
pub struct InputSlots<'s, 'c> {
    layout: Layout<'s, 'c>,
    control: ProductionControl<'c>,
}

impl<'s, 'c> InputSlots<'s, 'c> {
    pub fn borrow(schema: &'s RowSchema, control: ProductionControl<'c>) -> Self {
        Self {
            layout: Layout::Row(schema),
            control,
        }
    }

    /// Observe the same input namespace and slots as `RowSchema::join(left, right, [])`, with no extra logical columns. Visible ambiguity is recomputed from the original identities; hidden aliases preserve right-input replacement and NULL-slot semantics.
    pub fn join(
        left: &'s RowSchema,
        right: &'s RowSchema,
        control: ProductionControl<'c>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let mut visible = ProductionMap::new(control);
        for (schema, base) in [(left, 0), (right, left.physical_width())] {
            for (position, identity) in schema.identities().iter().enumerate() {
                control.check()?;
                let slot = schema.slot(position).map(|slot| base + slot);
                let keys = [
                    Some((None, identity.column())),
                    identity.qualifier().map(|q| (Some(q), identity.column())),
                ];
                for key in keys.into_iter().flatten() {
                    let binding = if visible.get(&key).is_some() {
                        Binding::Ambiguous
                    } else {
                        Binding::Unique(slot)
                    };
                    visible.insert_copy(key, binding)?;
                }
            }
        }
        for column in right.index.executor_attributes.keys() {
            control.check()?;
            assert!(
                !left.index.executor_attributes.contains_key(column),
                "duplicate internal relation attribute in joined row"
            );
        }
        for column in right.index.cold.executor_attribute_types.keys() {
            control.check()?;
            assert!(
                !left
                    .index
                    .cold
                    .executor_attribute_types
                    .contains_key(column),
                "duplicate internal relation attribute type in joined row"
            );
        }
        Ok(Self {
            layout: Layout::Join {
                left,
                right,
                visible,
            },
            control,
        })
    }

    pub fn physical_width(&self) -> usize {
        match &self.layout {
            Layout::Row(schema) => schema.physical_width(),
            Layout::Join { left, right, .. } => left.physical_width() + right.physical_width(),
        }
    }

    /// Visit the physical inputs of one lexical reference. Wildcard aliases may repeat slots; the consuming demand owner deduplicates them. This observes names only and never evaluates SQL.
    pub fn visit_reference(
        &self,
        expression: &ScalarExpr,
        visitor: &mut impl FnMut(usize) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        self.control.check()?;
        let slot = match expression {
            ScalarExpr::Column(name) => match self.named_slot(None, name) {
                Some(slot) => Some(slot),
                None => return self.qualified_star(name, visitor),
            },
            ScalarExpr::QualifiedColumn { qualifier, column } => {
                self.named_slot(Some(qualifier), column)
            }
            ScalarExpr::InternalColumn(column) => self.internal_slot(*column),
            ScalarExpr::QualifiedStar(qualifier) => {
                return self.qualified_star(qualifier, visitor);
            }
            ScalarExpr::Star => {
                for (schema, base) in self.schemas().into_iter().flatten() {
                    for position in 0..schema.len() {
                        self.control.check()?;
                        if schema.wildcard_position_visible(position) {
                            if let Some(slot) = schema.slot(position) {
                                visitor(base + slot)?;
                            }
                        }
                    }
                }
                None
            }
            ScalarExpr::Position(position) => self.slot(*position),
            _ => None,
        };
        if let Some(slot) = slot {
            visitor(slot)?;
        }
        Ok(())
    }

    fn schemas(&self) -> [Option<(&RowSchema, usize)>; 2] {
        match &self.layout {
            Layout::Row(schema) => [Some((schema, 0)), None],
            Layout::Join { left, right, .. } => {
                [Some((left, 0)), Some((right, left.physical_width()))]
            }
        }
    }

    fn named_slot(&self, qualifier: Option<&str>, column: &str) -> Option<usize> {
        match &self.layout {
            Layout::Row(schema) => qualifier.map_or_else(
                || schema.column_slot(column),
                |q| schema.qualified_slot(q, column),
            ),
            Layout::Join {
                left,
                right,
                visible,
            } => {
                let key = (qualifier, column);
                match visible.get(&key) {
                    Some(Binding::Ambiguous) => return None,
                    Some(Binding::Unique(Some(slot))) => return Some(*slot),
                    Some(Binding::Unique(None)) | None => {}
                }
                let key = &key as &dyn ColumnKey;
                let slot = right
                    .index
                    .aliases
                    .get(key)
                    .map(|slot| shift(*slot, left.physical_width()))
                    .or_else(|| left.index.aliases.get(key).copied());
                slot.filter(|slot| *slot != NULL_SLOT)
            }
        }
    }

    fn slot(&self, position: usize) -> Option<usize> {
        match &self.layout {
            Layout::Row(schema) => schema.slot(position),
            Layout::Join { left, right, .. } => {
                if position < left.len() {
                    left.slot(position)
                } else {
                    right
                        .slot(position - left.len())
                        .map(|slot| left.physical_width() + slot)
                }
            }
        }
    }

    fn internal_slot(&self, column: InternalColumnRef) -> Option<usize> {
        match &self.layout {
            Layout::Row(schema) => schema.internal_slot(column),
            Layout::Join { left, right, .. } => left.internal_slot(column).or_else(|| {
                right
                    .internal_slot(column)
                    .map(|slot| left.physical_width() + slot)
            }),
        }
    }

    fn qualified_star(
        &self,
        qualifier: &str,
        visitor: &mut impl FnMut(usize) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        let schemas = self.schemas();
        for (index, item) in schemas.iter().enumerate() {
            let Some((schema, base)) = item else {
                continue;
            };
            for (position, identity) in schema.identities().iter().enumerate() {
                self.control.check()?;
                if identity.qualifier() == Some(qualifier) {
                    visitor(shift(schema.slot(position).unwrap_or(NULL_SLOT), *base))?;
                }
            }
            for (identity, slot) in &schema.index.aliases {
                self.control.check()?;
                let replaced = index == 0
                    && schemas[1]
                        .is_some_and(|(right, _)| right.index.aliases.contains_key(identity));
                if !replaced && identity.qualifier() == Some(qualifier) {
                    visitor(shift(*slot, *base))?;
                }
            }
        }
        Ok(())
    }
}

fn shift(slot: usize, base: usize) -> usize {
    if slot == NULL_SLOT {
        NULL_SLOT
    } else {
        base + slot
    }
}

#[cfg(test)]
mod tests;
