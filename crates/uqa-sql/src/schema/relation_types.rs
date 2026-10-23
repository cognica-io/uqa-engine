//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Whole-row namespaces survive empty rows independently of named type identity.

use super::{Arc, ColumnIdentity, ColumnType, HashMap, RowSchema};
use crate::SQLError;

/// Absent namespaces allocate no map; copies of a retained scope share its identities.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct RelationRowTypes(Option<Arc<RelationRows>>);

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct RelationRows {
    types: HashMap<Box<str>, Option<ColumnType>>,
    /// The complete local row denotes this one source. A join or an outer-only namespace has no such identity.
    source: Option<Box<str>>,
}

impl RelationRowTypes {
    pub(super) fn get(&self, qualifier: &str) -> Option<&ColumnType> {
        self.0.as_ref()?.types.get(qualifier)?.as_ref()
    }

    pub(super) fn contains(&self, qualifier: &str) -> bool {
        self.0
            .as_ref()
            .is_some_and(|rows| rows.types.contains_key(qualifier))
    }

    pub(super) fn anonymous(qualifier: &str) -> Self {
        let mut result = Self::default();
        result.insert(qualifier, None);
        Arc::make_mut(result.0.as_mut().unwrap()).source = Some(qualifier.into());
        result
    }

    pub(super) fn qualifiers(&self) -> impl Iterator<Item = &str> {
        self.0
            .iter()
            .flat_map(|rows| rows.types.keys().map(std::convert::AsRef::as_ref))
    }

    fn insert(&mut self, qualifier: &str, ty: Option<ColumnType>) {
        let rows = self
            .0
            .get_or_insert_with(|| Arc::new(RelationRows::default()));
        Arc::make_mut(rows).types.insert(qualifier.into(), ty);
    }

    /// An alias of one retained relation keeps that relation's named type. A boundary over several relations produces an anonymous row.
    pub(super) fn requalified(&self, input: &RowSchema, qualifier: &str) -> Self {
        let anonymous = || {
            if input.is_empty() {
                Self::anonymous(qualifier)
            } else {
                Self::default()
            }
        };
        let Some(rows) = self.0.as_ref() else {
            return anonymous();
        };
        let Some(source) = rows.source.as_deref() else {
            return anonymous();
        };
        if input
            .identities()
            .iter()
            .enumerate()
            .any(|(position, column)| {
                input.wildcard_position_visible(position) && column.qualifier() != Some(source)
            })
        {
            return anonymous();
        }
        let mut result = Self::default();
        result.insert(qualifier, rows.types[source].clone());
        Arc::make_mut(result.0.as_mut().unwrap()).source = Some(qualifier.into());
        result
    }

    pub(super) fn joined(left: &RowSchema, right: &RowSchema) -> Self {
        if left.index.cold.row_types.0.is_none() && right.index.cold.row_types.0.is_none() {
            return Self::default();
        }
        let mut result = Self::default();
        for (source, other) in [(left, right), (right, left)] {
            if let Some(rows) = &source.index.cold.row_types.0 {
                for (qualifier, ty) in &rows.types {
                    if !other.has_qualifier(qualifier) {
                        result.insert(qualifier, ty.clone());
                    }
                }
            }
        }
        result
    }

    pub(super) fn with_outer(input: &RowSchema, outer: &RowSchema) -> Self {
        let mut result = input.index.cold.row_types.clone();
        if let Some(rows) = &outer.index.cold.row_types.0 {
            for (qualifier, ty) in &rows.types {
                if !input.has_qualifier(qualifier) {
                    result.insert(qualifier, ty.clone());
                }
            }
        }
        result
    }
}

impl RowSchema {
    pub(super) fn relation_qualifiers(&self) -> impl Iterator<Item = &str> {
        self.index
            .qualified
            .keys()
            .chain(self.index.aliases.keys())
            .filter_map(ColumnIdentity::qualifier)
            .chain(self.index.cold.row_types.qualifiers())
            .chain(self.whole_row_presences().map(|(qualifier, _)| qualifier))
    }

    /// Retain a relation's actual named composite identity without adding an ordinary column or a physical value slot. The existing source namespace must belong to that relation; a zero-column relation acquires its namespace here.
    pub fn with_relation_row_type(
        mut self,
        qualifier: &str,
        ty: ColumnType,
    ) -> Result<Self, SQLError> {
        if !matches!(ty, ColumnType::Composite(_))
            || (!self.is_empty() && !self.has_qualifier(qualifier))
        {
            return Err(SQLError::Internal(
                "a relation row type requires its named composite and source namespace".into(),
            ));
        }
        let rows = &mut Arc::make_mut(&mut self.index).cold.row_types;
        rows.insert(qualifier, Some(ty));
        let rows = Arc::make_mut(rows.0.as_mut().unwrap());
        rows.source = (rows.types.len() == 1).then(|| qualifier.into());
        Ok(self)
    }
}

#[cfg(test)]
mod tests;
