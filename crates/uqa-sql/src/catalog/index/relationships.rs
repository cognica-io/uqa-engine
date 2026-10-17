//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constraint ownership and index partition ancestry are independent catalog edges.

use super::IndexDefinition;
use crate::ast::{ColumnDef, Expr, IndexKey, TableKeyConstraint, TableKeyConstraintKind};
use crate::schema::constraint_metadata::{ConstraintMetadataError, ConstraintMetadataResult};

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexRelationships {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owning_constraint: Option<[u8; 16]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_index: Option<[u8; 16]>,
}

impl IndexRelationships {
    pub fn is_empty(&self) -> bool {
        self.owning_constraint.is_none() && self.parent_index.is_none()
    }

    pub fn validate(&self, index: [u8; 16]) -> ConstraintMetadataResult<()> {
        if [self.owning_constraint, self.parent_index]
            .into_iter()
            .flatten()
            .any(|target| target == [0; 16] || target == index)
        {
            return Err(ConstraintMetadataError::Invalid(
                "invalid index ownership or parent identity".into(),
            ));
        }
        Ok(())
    }
}

pub struct IndexAttachmentShape<'a> {
    pub method: &'a str,
    pub keys: &'a [IndexKey],
    pub definition: &'a IndexDefinition,
    pub constraint_kind: Option<TableKeyConstraintKind>,
}

impl IndexDefinition {
    pub fn for_constraint(
        constraint: &TableKeyConstraint,
        columns: &[ColumnDef],
    ) -> ConstraintMetadataResult<Self> {
        let owner = constraint.catalog_identity.ok_or_else(|| {
            ConstraintMetadataError::Invalid("index owner has no constraint identity".into())
        })?;
        if !owner.is_valid() {
            return Err(ConstraintMetadataError::Invalid(
                "index owner has an invalid identity".into(),
            ));
        }
        let key_types = constraint
            .columns
            .iter()
            .map(|name| {
                columns
                    .iter()
                    .find(|column| column.name == *name)
                    .map(|column| column.ty.clone())
                    .ok_or_else(|| {
                        ConstraintMetadataError::Invalid(format!(
                            "constraint index references missing column `{name}`"
                        ))
                    })
            })
            .collect::<ConstraintMetadataResult<_>>()?;
        Ok(Self {
            relationships: IndexRelationships {
                owning_constraint: Some(owner.object_id),
                parent_index: None,
            },
            key_names: crate::schema::indexes::keys::key_names(
                &crate::schema::indexes::names::constraint_index_attributes(constraint),
            ),
            key_types,
            included_columns: constraint.included_columns.clone(),
            unique: true,
            nulls_not_distinct: constraint.nulls_not_distinct,
            ..Self::default()
        })
    }
}

/// A constraint-owned parent requires a same-kind child constraint; an independent parent can attach either an independent or constraint-owned child index. Object identities, physical namespaces and display names are not index-shape properties.
pub fn can_attach_index(
    parent: &IndexAttachmentShape<'_>,
    child: &IndexAttachmentShape<'_>,
) -> bool {
    parent
        .constraint_kind
        .is_none_or(|kind| child.constraint_kind == Some(kind))
        && parent.method.eq_ignore_ascii_case(child.method)
        && same_index_keys(parent.keys, child.keys)
        && parent.definition.unique == child.definition.unique
        && parent.definition.nulls_not_distinct == child.definition.nulls_not_distinct
        && parent.definition.included_columns == child.definition.included_columns
        && same_index_predicate(
            parent.definition.predicate.as_deref(),
            child.definition.predicate.as_deref(),
        )
        && (0..parent.keys.len()).all(|position| {
            parent
                .definition
                .column_order
                .get(position)
                .copied()
                .unwrap_or_default()
                == child
                    .definition
                    .column_order
                    .get(position)
                    .copied()
                    .unwrap_or_default()
        })
}

/// Whether two key lists index the same columns and expressions, apart from the input collations their calls record.
#[must_use]
pub fn same_index_keys(left: &[IndexKey], right: &[IndexKey]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|pair| match pair {
            (IndexKey::Column(left), IndexKey::Column(right)) => left == right,
            (IndexKey::Expression(left), IndexKey::Expression(right)) => {
                crate::catalog::stored_ast::same_stored_expression(left, right)
            }
            _ => false,
        })
}

/// Whether two partial-index predicates are the same, apart from the input collations their calls record.
#[must_use]
pub fn same_index_predicate(left: Option<&Expr>, right: Option<&Expr>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            crate::catalog::stored_ast::same_stored_expression(left, right)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constraint_ownership_controls_attachment_in_only_the_parent_direction() {
        let definition = IndexDefinition {
            unique: true,
            ..IndexDefinition::default()
        };
        let keys = [IndexKey::Column("k".into())];
        let shape = |constraint_kind| IndexAttachmentShape {
            method: "btree",
            keys: &keys,
            definition: &definition,
            constraint_kind,
        };
        use TableKeyConstraintKind::{PrimaryKey, Unique};
        for parent in [None, Some(PrimaryKey), Some(Unique)] {
            for child in [None, Some(PrimaryKey), Some(Unique)] {
                assert_eq!(
                    can_attach_index(&shape(parent), &shape(child)),
                    parent.is_none() || parent == child,
                    "parent {parent:?}, child {child:?}"
                );
            }
        }
        let other = [IndexKey::Column("other".into())];
        let mut child = shape(None);
        child.keys = &other;
        assert!(!can_attach_index(&shape(None), &child));
    }

    #[test]
    fn index_edges_reject_zero_and_self_references_and_remain_independent() {
        let valid = IndexRelationships {
            owning_constraint: Some([1; 16]),
            parent_index: Some([2; 16]),
        };
        valid.validate([3; 16]).unwrap();
        for invalid in [[0; 16], [3; 16]] {
            for relationships in [
                IndexRelationships {
                    owning_constraint: Some(invalid),
                    ..valid.clone()
                },
                IndexRelationships {
                    parent_index: Some(invalid),
                    ..valid.clone()
                },
            ] {
                assert!(relationships.validate([3; 16]).is_err());
            }
        }
        let detached = IndexRelationships {
            parent_index: None,
            ..valid
        };
        assert_eq!(detached.owning_constraint, Some([1; 16]));
        assert!(!detached.is_empty());
    }
}
