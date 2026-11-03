//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog identity capture preserves allocated values without deriving replacements from names.

use crate::{
    ast::RelationPersistence,
    catalog::{
        graph_oids::{EndpointIndexOids, GraphCatalogOids, IndexConstraintOids, LabelCatalogOids},
        relation_oids::RelationCatalogOids,
    },
    plan::copy::SyntaxProduction,
    SQLError,
};
use std::collections::BTreeMap;
use uqa_core::{
    memory::{Produced, ProductionControl},
    RelationIdentity,
};

pub type CapturedSequenceObjects = Vec<(RelationIdentity, [u8; 16])>;
pub type CapturedSequenceOids = Vec<([u8; 16], u32)>;

fn relations<T: Copy>(
    values: &BTreeMap<RelationIdentity, T>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<(RelationIdentity, T)>>, SQLError> {
    let output = SyntaxProduction::new(*control);
    let mut entries = output.vector(values.len())?;
    for (name, value) in values {
        entries.push((output.capture_definition(name)?, *value))?;
    }
    let entries = entries.finish()?;
    output.finish(entries)
}

pub fn sequence_objects(
    values: &BTreeMap<RelationIdentity, [u8; 16]>,
    control: &ProductionControl<'_>,
) -> Result<Produced<CapturedSequenceObjects>, SQLError> {
    relations(values, control)
}

pub fn sequence_persistence(
    values: &BTreeMap<RelationIdentity, RelationPersistence>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<(RelationIdentity, RelationPersistence)>>, SQLError> {
    relations(values, control)
}

pub fn sequence_oids(
    values: &BTreeMap<[u8; 16], u32>,
    control: &ProductionControl<'_>,
) -> Result<Produced<CapturedSequenceOids>, SQLError> {
    let output = SyntaxProduction::new(*control);
    let mut entries = output.vector(values.len())?;
    for (identity, oid) in values {
        entries.push((*identity, *oid))?;
    }
    let entries = entries.finish()?;
    output.finish(entries)
}

#[derive(Debug, PartialEq, Eq)]
pub struct CapturedGraphCatalogOids {
    pub namespace: u32,
    pub label_sequence: u32,
    pub labels: Vec<(u32, CapturedLabelCatalogOids)>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct CapturedLabelCatalogOids {
    pub sequence: u32,
    pub relation: RelationCatalogOids,
    pub id_default: u32,
    pub properties_default: u32,
    pub not_null: Vec<(String, u32)>,
    pub toast_table: u32,
    pub toast_index: u32,
    pub primary_key: Option<IndexConstraintOids>,
    pub endpoint_indexes: Option<EndpointIndexOids>,
    pub trigger: u32,
}

fn label(
    value: &LabelCatalogOids,
    output: &SyntaxProduction<'_>,
) -> Result<CapturedLabelCatalogOids, SQLError> {
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
    let mut constraints = output.vector(not_null.len())?;
    for (name, oid) in not_null {
        constraints.push((output.text(name)?, *oid))?;
    }
    Ok(CapturedLabelCatalogOids {
        sequence: *sequence,
        relation: *relation,
        id_default: *id_default,
        properties_default: *properties_default,
        not_null: constraints.finish()?,
        toast_table: *toast_table,
        toast_index: *toast_index,
        primary_key: *primary_key,
        endpoint_indexes: *endpoint_indexes,
        trigger: *trigger,
    })
}

pub fn graph_oids(
    values: &BTreeMap<String, GraphCatalogOids>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<(String, CapturedGraphCatalogOids)>>, SQLError> {
    let output = SyntaxProduction::new(*control);
    let mut entries = output.vector(values.len())?;
    for (
        name,
        GraphCatalogOids {
            namespace,
            label_sequence,
            labels,
        },
    ) in values
    {
        let name = output.text(name)?;
        let mut captured = output.vector(labels.len())?;
        for (id, value) in labels {
            captured.push((*id, label(value, &output)?))?;
        }
        entries.push((
            name,
            CapturedGraphCatalogOids {
                namespace: *namespace,
                label_sequence: *label_sequence,
                labels: captured.finish()?,
            },
        ))?;
    }
    let entries = entries.finish()?;
    output.finish(entries)
}

#[cfg(test)]
mod tests;
