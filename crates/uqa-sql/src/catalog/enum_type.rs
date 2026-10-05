//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable enum definitions: label identity, immutable order keys and `PostgreSQL` float4 sort positions.
//!
//! Each label owns an immutable [`EnumLabelKey`] that orders stored values, and a separate
//! `pg_enum.enumsortorder` that reproduces `PostgreSQL`'s float4 midpoint and renumbering
//! algorithm. Renumbering changes only the catalog column; stored values keep their keys.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use uqa_core::{catalog_role::RoleIdentity, EnumLabelKey, RelationIdentity};

use super::roles::{identity::RoleSubject, RoleDefinition};
use crate::ast::{ColumnType, EnumNeighbor, EnumTypeReference};
use crate::SQLError;

/// `PostgreSQL` stores labels in a `name` column of `NAMEDATALEN - 1` bytes.
pub const MAX_ENUM_LABEL_BYTES: usize = 63;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredEnumLabel {
    pub oid: u32,
    pub key: EnumLabelKey,
    pub label: String,
    pub sort_order: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredEnum<Owner = RoleIdentity> {
    pub object_id: [u8; 16],
    pub oid: u32,
    pub array_oid: u32,
    /// Name of the generated array type in the enum's schema, which a later type can displace.
    pub array_name: String,
    pub identity: RelationIdentity,
    pub owner: Owner,
    /// Labels in declaration order, which is ascending key and sort-order order.
    pub labels: Vec<StoredEnumLabel>,
    /// Explicit `USAGE` privileges; `None` is the default ACL (PUBLIC and the owner). The array type has no ACL of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_acl: Option<Vec<crate::ast::ObjectAclEntry>>,
}

impl<Owner> StoredEnum<Owner> {
    pub fn reference(&self) -> EnumTypeReference {
        EnumTypeReference {
            schema: self.identity.schema.clone(),
            name: self.identity.name.clone(),
            oid: self.oid,
            array_oid: self.array_oid,
        }
    }

    pub fn column_type(&self) -> ColumnType {
        ColumnType::Enum(self.reference())
    }

    pub fn label_by_key(&self, key: &EnumLabelKey) -> Option<&StoredEnumLabel> {
        self.labels
            .binary_search_by(|label| label.key.cmp(key))
            .ok()
            .map(|index| &self.labels[index])
    }

    pub fn label_by_text(&self, text: &str) -> Option<&StoredEnumLabel> {
        self.labels.iter().find(|label| label.label == text)
    }

    pub fn label_oids(&self) -> impl Iterator<Item = u32> + '_ {
        self.labels.iter().map(|label| label.oid)
    }
}

/// Result of `ALTER TYPE ... ADD VALUE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddedEnumLabel {
    /// The new label's OID and key.
    Added { oid: u32, key: EnumLabelKey },
    /// `IF NOT EXISTS` found the label; `PostgreSQL` reports this notice and changes nothing.
    Skipped(String),
}

fn invalid_label(label: &str) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "42602".into(),
        message: format!("invalid enum label \"{label}\""),
        detail: Some(format!(
            "Labels must be {MAX_ENUM_LABEL_BYTES} bytes or less."
        )),
        hint: None,
    }
}

fn not_a_label(label: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "22023".into(),
        message: format!("\"{label}\" is not an existing enum label"),
    }
}

fn label_exists(label: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42710".into(),
        message: format!("enum label \"{label}\" already exists"),
    }
}

pub fn validate_enum_label(label: &str) -> Result<(), SQLError> {
    if label.len() > MAX_ENUM_LABEL_BYTES {
        return Err(invalid_label(label));
    }
    Ok(())
}

fn key_error(error: uqa_core::EnumLabelKeyError) -> SQLError {
    SQLError::Routine {
        sqlstate: "54000".into(),
        message: format!("cannot allocate an enum label position: {error}"),
    }
}

/// Build the labels of `CREATE TYPE ... AS ENUM` in declaration order. Labels are checked in list order: a label longer than 63 bytes fails with 42602, and a repeated label fails like `PostgreSQL`'s `pg_enum` unique-index insertion. `label_oids` supplies one OID per label.
pub fn initial_enum_labels(
    type_oid: u32,
    labels: &[String],
    label_oids: &[u32],
) -> Result<Vec<StoredEnumLabel>, SQLError> {
    if labels.len() != label_oids.len() {
        return Err(SQLError::Internal(
            "enum label OID allocation does not match its labels".into(),
        ));
    }
    let keys = EnumLabelKey::initial(labels.len()).map_err(key_error)?;
    let mut stored: Vec<StoredEnumLabel> = Vec::with_capacity(labels.len());
    for (position, ((label, oid), key)) in labels.iter().zip(label_oids).zip(keys).enumerate() {
        validate_enum_label(label)?;
        if stored.iter().any(|existing| existing.label == *label) {
            return Err(SQLError::Diagnostic {
                sqlstate: "23505".into(),
                message:
                    "duplicate key value violates unique constraint \"pg_enum_typid_label_index\""
                        .into(),
                detail: Some(format!(
                    "Key (enumtypid, enumlabel)=({type_oid}, {label}) already exists."
                )),
                hint: None,
            });
        }
        stored.push(StoredEnumLabel {
            oid: *oid,
            key,
            label: label.clone(),
            sort_order: (position + 1) as f32,
        });
    }
    Ok(stored)
}

impl<Owner> StoredEnum<Owner> {
    /// Apply `ALTER TYPE ... ADD VALUE` with `PostgreSQL`'s check order: label length, existing label (or the `IF NOT EXISTS` notice), then the neighbor. The float4 sort position follows `AddEnumLabel`, renumbering every label to `1..n` when a midpoint collapses onto a neighbor; keys never change. Only then does `allocate` draw the label's OID, which must satisfy the predicate it receives: an even OID when the new OID orders correctly against every even-numbered label and an odd one otherwise, as `AddEnumLabel` chooses it.
    pub fn add_label(
        &mut self,
        label: &str,
        neighbor: Option<&EnumNeighbor>,
        if_not_exists: bool,
        allocate: impl FnOnce(&dyn Fn(u32) -> bool) -> Result<u32, SQLError>,
    ) -> Result<AddedEnumLabel, SQLError> {
        validate_enum_label(label)?;
        if self.label_by_text(label).is_some() {
            if if_not_exists {
                return Ok(AddedEnumLabel::Skipped(format!(
                    "enum label \"{label}\" already exists, skipping"
                )));
            }
            return Err(label_exists(label));
        }
        let (position, key, sort_order) = match neighbor {
            None => {
                let last = self.labels.last();
                let key =
                    EnumLabelKey::between(last.map(|label| &label.key), None).map_err(key_error)?;
                let order = last.map_or(1.0, |label| label.sort_order + 1.0);
                (self.labels.len(), key, order)
            }
            Some(neighbor) => {
                let index = self
                    .labels
                    .iter()
                    .position(|existing| existing.label == neighbor.label)
                    .ok_or_else(|| not_a_label(&neighbor.label))?;
                let (lower, upper, position) = if neighbor.after {
                    (
                        Some(index),
                        (index + 1 < self.labels.len()).then_some(index + 1),
                        index + 1,
                    )
                } else {
                    (index.checked_sub(1), Some(index), index)
                };
                let key = EnumLabelKey::between(
                    lower.map(|index| &self.labels[index].key),
                    upper.map(|index| &self.labels[index].key),
                )
                .map_err(key_error)?;
                let order = self.neighbor_sort_order(index, neighbor.after);
                (position, key, order)
            }
        };
        let labels = &self.labels;
        let oid = allocate(&|candidate| {
            let sorts = labels
                .iter()
                .filter(|existing| existing.oid % 2 == 0)
                .all(|existing| {
                    if existing.sort_order < sort_order {
                        existing.oid < candidate
                    } else {
                        existing.oid > candidate
                    }
                });
            sorts == (candidate % 2 == 0)
        })?;
        self.labels.insert(
            position,
            StoredEnumLabel {
                oid,
                key: key.clone(),
                label: label.to_owned(),
                sort_order,
            },
        );
        Ok(AddedEnumLabel::Added { oid, key })
    }

    /// `PostgreSQL` computes the neighbor midpoint in float4 and renumbers existing labels to `1..n` before retrying when the midpoint equals either neighbor.
    fn neighbor_sort_order(&mut self, index: usize, after: bool) -> f32 {
        let other = if after {
            index
                .checked_add(1)
                .filter(|other| *other < self.labels.len())
        } else {
            index.checked_sub(1)
        };
        let Some(other) = other else {
            let base = self.labels[index].sort_order;
            return if after { base + 1.0 } else { base - 1.0 };
        };
        let midpoint = f32::midpoint(self.labels[index].sort_order, self.labels[other].sort_order);
        if midpoint == self.labels[index].sort_order || midpoint == self.labels[other].sort_order {
            for (position, label) in self.labels.iter_mut().enumerate() {
                label.sort_order = (position + 1) as f32;
            }
            return self.neighbor_sort_order(index, after);
        }
        midpoint
    }

    /// Apply `ALTER TYPE ... RENAME VALUE` with `PostgreSQL`'s check order: new label length, missing old label, then an existing new label. The key and OID are unchanged, so stored values follow the rename.
    pub fn rename_label(&mut self, old: &str, new: &str) -> Result<(), SQLError> {
        validate_enum_label(new)?;
        let index = self
            .labels
            .iter()
            .position(|label| label.label == old)
            .ok_or_else(|| not_a_label(old))?;
        if self.labels.iter().any(|label| label.label == new) {
            return Err(label_exists(new));
        }
        new.clone_into(&mut self.labels[index].label);
        Ok(())
    }
}

/// Validate a complete enum registry before restoration exposes it or publication persists it.
pub fn validate_enum_registry(
    registry: &BTreeMap<String, StoredEnum>,
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<(), String> {
    let mut identities = BTreeSet::new();
    let mut oids = BTreeSet::new();
    for (name, definition) in registry {
        if definition.object_id == [0; 16] || !identities.insert(definition.object_id) {
            return Err(format!("invalid or duplicate enum identity for `{name}`"));
        }
        for oid in [definition.oid, definition.array_oid]
            .into_iter()
            .chain(definition.label_oids())
        {
            if oid < super::oids::FIRST_NORMAL_OBJECT_ID || !oids.insert(oid) {
                return Err(format!("invalid or duplicate enum OID {oid} for `{name}`"));
            }
        }
        if definition.identity.schema.is_empty()
            || definition.identity.name.is_empty()
            || definition.identity.qualified_name() != *name
            || !super::array_type_names::valid_type_name(&definition.array_name)
        {
            return Err(format!("inconsistent enum name for `{name}`"));
        }
        let mut labels = BTreeSet::new();
        for label in &definition.labels {
            if label.label.len() > MAX_ENUM_LABEL_BYTES
                || !labels.insert(label.label.as_str())
                || !label.sort_order.is_finite()
            {
                return Err(format!("invalid enum label for `{name}`"));
            }
        }
        if definition
            .labels
            .windows(2)
            .any(|pair| pair[0].key >= pair[1].key || pair[0].sort_order >= pair[1].sort_order)
        {
            return Err(format!("enum labels for `{name}` are out of order"));
        }
        if !definition.owner.is_valid() || definition.owner.role_definition(roles).is_none() {
            return Err(format!(
                "enum `{name}` references missing role incarnation {}",
                definition.owner.oid
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
