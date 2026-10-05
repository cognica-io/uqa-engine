//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain only the catalog tuple identities selected by prepared parse analysis.

use super::CatalogReadView;
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;
use uqa_sql::{
    prepared::dependencies::{
        PreparedAnalysisDependencies, PreparedDependencyRevision, PreparedDependencySnapshot,
    },
    SQLError,
};
use uqa_storage::catalog::{CatalogRecordRef, CatalogRecordRevision, RelationKind};

#[derive(PartialEq, Eq)]
struct RelationRevision {
    object: [u8; 16],
    acl: Option<uqa_core::catalog_acl::RelationAclRevisions>,
    record_names: Vec<(RelationKind, RelationIdentity)>,
    records: Option<Vec<Option<CatalogRecordRevision>>>,
}

struct RelationDependency {
    oid: u32,
    relation: RelationIdentity,
    kind: RelationKind,
    object: [u8; 16],
    acl: Option<uqa_core::catalog_acl::RelationAclRevisions>,
}

pub(super) fn capture(
    catalog: &CatalogReadView,
    dependencies: &PreparedAnalysisDependencies,
) -> Result<PreparedDependencySnapshot, SQLError> {
    let snapshot = catalog.snapshot();
    let mut result = PreparedDependencySnapshot {
        relations: dependencies
            .relations
            .iter()
            .map(|oid| (*oid, None))
            .collect(),
        routines: dependencies.routines.iter().map(|id| (*id, None)).collect(),
        global_catalog: Some(PreparedDependencyRevision::new(
            snapshot.definitions.schemas.clone(),
        )),
    };
    for function in snapshot.definitions.sql_user_functions.values().flatten() {
        if let Some(id) = function.def.object_id {
            if let Some(revision) = result.routines.get_mut(&id) {
                *revision = function
                    .catalog_revision()
                    .map(PreparedDependencyRevision::new);
            }
        }
    }
    for relation in uqa_sql::catalog::SystemRelation::all() {
        if let Ok(oid) = u32::try_from(relation.oid()) {
            if let Some(revision) = result.relations.get_mut(&oid) {
                *revision = Some(PreparedDependencyRevision::new(oid));
            }
        }
    }
    if dependencies.relations.is_empty() {
        return Ok(result);
    }
    let selected = select_relations(catalog, dependencies)?;
    result
        .relations
        .extend(read_selected_relations(catalog, selected)?);
    Ok(result)
}

struct SelectedRelations<'a> {
    objects: Vec<RelationDependency>,
    indexes: BTreeMap<String, Vec<&'a RelationIdentity>>,
}

fn select_relations<'a>(
    catalog: &'a CatalogReadView,
    dependencies: &PreparedAnalysisDependencies,
) -> Result<SelectedRelations<'a>, SQLError> {
    let snapshot = catalog.snapshot();
    let mut selected = Vec::new();
    let mut include = |oid: i64, relation: &RelationIdentity, kind, object, acl| {
        if let Ok(oid) = u32::try_from(oid) {
            if dependencies.relations.contains(&oid) {
                selected.push(RelationDependency {
                    oid,
                    relation: relation.clone(),
                    kind,
                    object,
                    acl,
                });
            }
        }
    };
    for (relation, table) in &snapshot.tables {
        include(
            i64::from(table.catalog_oids.relation),
            relation,
            RelationKind::Table,
            table.object_id,
            Some(table.security.acl_revisions.clone()),
        );
    }
    for (relation, view) in snapshot.definitions.views.iter() {
        include(
            super::projection::view_relation_oid(view),
            relation,
            RelationKind::View,
            view.object_id,
            Some(view.security.acl_revisions.clone()),
        );
    }
    for (relation, table) in snapshot.definitions.foreign_tables.iter() {
        include(
            super::projection::foreign_table_relation_oid(table),
            relation,
            RelationKind::ForeignTable,
            table.object_id,
            snapshot
                .definitions
                .foreign_table_security
                .get(relation)
                .map(|security| security.acl_revisions.clone()),
        );
    }
    for (relation, object) in snapshot.definitions.sequence_object_ids.iter() {
        include(
            catalog.sequence_catalog_oid(object),
            relation,
            RelationKind::Sequence,
            *object,
            None,
        );
    }
    let mut indexes = BTreeMap::new();
    for (relation, row) in snapshot.definitions.catalog_indexes.iter() {
        let definition = super::index::index_definition(row)
            .map_err(|error| SQLError::Internal(error.to_string()))?;
        let identity = definition.catalog.as_ref().ok_or_else(|| {
            SQLError::Internal(format!(
                "index `{}` has no catalog identity",
                relation.qualified_name()
            ))
        })?;
        include(
            identity.identity.oid,
            relation,
            RelationKind::Index,
            identity.identity.object_id,
            None,
        );
        indexes
            .entry(row.table_name.clone())
            .or_insert_with(Vec::new)
            .push(relation);
    }
    Ok(SelectedRelations {
        objects: selected,
        indexes,
    })
}

fn read_selected_relations(
    catalog: &CatalogReadView,
    selected: SelectedRelations<'_>,
) -> Result<BTreeMap<u32, Option<PreparedDependencyRevision>>, SQLError> {
    let SelectedRelations {
        objects: selected,
        indexes,
    } = selected;
    let mut records = Vec::new();
    let mut ranges = Vec::new();
    for dependency in &selected {
        let start = records.len();
        records.push(CatalogRecordRef::Relation(
            dependency.kind,
            &dependency.relation,
        ));
        if dependency.kind == RelationKind::Table {
            if let Some(indexes) = indexes.get(&dependency.relation.qualified_name()) {
                for index in indexes {
                    records.push(CatalogRecordRef::Relation(RelationKind::Index, index));
                }
            }
        }
        ranges.push(start..records.len());
    }
    let revisions = catalog
        .prepared_catalog
        .as_ref()
        .map_or(Ok(None), |catalog| catalog.record_revisions(&records))
        .map_err(|error| {
            uqa_sql::catalog::errors::storage_error("prepared dependency snapshot", &error)
        })?;
    if revisions
        .as_ref()
        .is_some_and(|revisions| revisions.len() != records.len())
    {
        return Err(SQLError::Internal(
            "catalog returned a different number of prepared dependency revisions".into(),
        ));
    }
    let mut result = BTreeMap::new();
    for (dependency, range) in selected.iter().zip(ranges) {
        result.insert(
            dependency.oid,
            Some(PreparedDependencyRevision::new(RelationRevision {
                object: dependency.object,
                acl: dependency.acl.clone(),
                record_names: records[range.clone()]
                    .iter()
                    .map(|record| match record {
                        CatalogRecordRef::Relation(kind, relation) => (*kind, (*relation).clone()),
                        CatalogRecordRef::Metadata(_) => unreachable!("relation records"),
                    })
                    .collect(),
                records: revisions
                    .as_ref()
                    .map(|revisions| revisions[range].to_vec()),
            })),
        );
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
