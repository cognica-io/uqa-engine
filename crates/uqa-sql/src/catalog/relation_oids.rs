//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The public OIDs of one relation incarnation: its `pg_class` row, the row type and array type `heap_create_with_catalog` creates with it, and a view's `_RETURN` rule.

use serde::{Deserialize, Serialize};

/// The kinds of relation whose OIDs differ in which companion objects exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationOidKind {
    /// Tables and partitioned tables: a row type and its array type.
    Table,
    /// Views and materialized views: a row type, its array type and the `_RETURN` rule.
    View,
    /// Foreign tables: a row type and its array type.
    ForeignTable,
    /// Sequences have no row type.
    Sequence,
}

/// Relation OIDs as the database's counter allocated them, in `PostgreSQL`'s order: the relation, the array type, the row type and then a view's rule. Relations created before OIDs were recorded derive them from their identity instead, and never change them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelationCatalogOids {
    pub relation: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_type: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub array_type: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<u32>,
}

impl RelationCatalogOids {
    /// The OIDs a relation created before OIDs were recorded derives from its identity. Such relations have no recorded array type.
    #[must_use]
    pub fn legacy(kind: RelationOidKind, object_id: &[u8; 16]) -> Self {
        let derive = |label| {
            u32::try_from(super::oids::stable_object_oid(label, object_id))
                .expect("derived catalog OIDs fit in u32")
        };
        Self {
            relation: derive("relation"),
            row_type: (kind != RelationOidKind::Sequence).then(|| derive("rowtype")),
            array_type: None,
            rule: (kind == RelationOidKind::View).then(|| derive("view-rule")),
        }
    }

    /// The row type OID `pg_class.reltype` reports: zero for relations without one.
    #[must_use]
    pub fn reltype(&self) -> u32 {
        self.row_type.unwrap_or(0)
    }

    /// Every OID the relation claims, for occupancy checks.
    pub fn claimed(&self) -> impl Iterator<Item = u32> {
        [
            Some(self.relation),
            self.row_type,
            self.array_type,
            self.rule,
        ]
        .into_iter()
        .flatten()
    }

    /// Whether the OIDs are well formed for the kind: every recorded OID is a user OID, and exactly the companions the kind has are present.
    #[must_use]
    pub fn is_valid_for(&self, kind: RelationOidKind) -> bool {
        let user = |oid: u32| oid >= super::oids::FIRST_NORMAL_OBJECT_ID;
        let has_row_type = kind != RelationOidKind::Sequence;
        user(self.relation)
            && self.row_type.is_some() == has_row_type
            && self.array_type.is_some() == has_row_type
            && self.rule.is_some() == (kind == RelationOidKind::View)
            && self.claimed().all(user)
    }
}

#[cfg(test)]
mod tests;
