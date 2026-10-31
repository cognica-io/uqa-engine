//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain only the catalog tuple identities selected by prepared parse analysis.

use super::CatalogReadView;
use uqa_core::{
    memory::{OwnedMap, Produced, ProductionControl, ProductionVec},
    RelationIdentity,
};
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
    acl: Option<AclRevision>,
    kind: RelationKind,
    relation: RelationIdentity,
    record: RecordRevision,
    indexes: Vec<[u8; 16]>,
}

#[derive(PartialEq, Eq)]
enum RecordRevision {
    Unavailable,
    Selected(Option<CatalogRecordRevision>),
}

#[derive(PartialEq, Eq)]
struct AclRevision {
    relation: Option<[u8; 16]>,
    columns: OwnedMap<String, [u8; 16]>,
}

#[derive(Clone, Copy)]
struct RelationDependency<'a> {
    oid: u32,
    relation: &'a RelationIdentity,
    kind: RelationKind,
    object: [u8; 16],
    acl: Option<&'a uqa_core::catalog_acl::RelationAclRevisions>,
}

pub(super) fn capture(
    catalog: &CatalogReadView,
    dependencies: &PreparedAnalysisDependencies,
) -> Result<PreparedDependencySnapshot, SQLError> {
    capture_with_control(catalog, dependencies, &ProductionControl::uncontrolled())
}

pub(super) fn capture_with_control(
    catalog: &CatalogReadView,
    dependencies: &PreparedAnalysisDependencies,
    control: &ProductionControl<'_>,
) -> Result<PreparedDependencySnapshot, SQLError> {
    control.check()?;
    let snapshot = catalog.snapshot();
    let mut result = PreparedDependencySnapshot::default();
    for oid in &dependencies.relations {
        insert(&mut result.relations, *oid, None, control)?;
    }
    for id in &dependencies.routines {
        insert(&mut result.routines, *id, None, control)?;
    }
    result.global_catalog = Some(revision(snapshot.definitions.schemas.clone(), control)?);
    for function in snapshot.definitions.sql_user_functions.values().flatten() {
        control.check()?;
        if let Some(id) = function.def.object_id {
            if let Some(slot) = result.routines.get_mut(&id) {
                *slot = function
                    .catalog_revision()
                    .map(|value| revision(value, control))
                    .transpose()?;
            }
        }
    }
    for relation in uqa_sql::catalog::SystemRelation::all() {
        control.check()?;
        if let Ok(oid) = u32::try_from(relation.oid()) {
            if let Some(slot) = result.relations.get_mut(&oid) {
                *slot = Some(revision(oid, control)?);
            }
        }
    }
    if !dependencies.relations.is_empty() {
        let selected = select_relations(catalog, dependencies, control)?;
        read_selected_relations(catalog, selected, &mut result, control)?;
    }
    Ok(result)
}

fn insert<K: Ord, V>(
    map: &mut OwnedMap<K, V>,
    key: K,
    value: V,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    match control.budget() {
        Some(budget) => {
            map.insert_budgeted(key, value, budget)?;
        }
        None => {
            map.insert(key, value);
        }
    }
    Ok(())
}

fn revision<T: Eq + Send + Sync + 'static>(
    value: T,
    control: &ProductionControl<'_>,
) -> Result<PreparedDependencyRevision, SQLError> {
    PreparedDependencyRevision::from_produced(
        control.finish(value, control.empty_reservation())?,
        control,
    )
}

struct SelectedRelations<'a, 'c> {
    objects: Produced<Vec<RelationDependency<'a>>>,
    indexes: OwnedMap<&'a str, ProductionVec<'c, [u8; 16]>>,
}

fn select_relations<'a, 'c>(
    catalog: &'a CatalogReadView,
    dependencies: &PreparedAnalysisDependencies,
    control: &ProductionControl<'c>,
) -> Result<SelectedRelations<'a, 'c>, SQLError> {
    let snapshot = catalog.snapshot();
    let mut selected = ProductionVec::new(*control);
    let mut include =
        |oid: i64, relation: &'a RelationIdentity, kind, object, acl| -> Result<(), SQLError> {
            control.check()?;
            if let Ok(oid) = u32::try_from(oid) {
                if dependencies.relations.contains(&oid) {
                    selected.push_copy(RelationDependency {
                        oid,
                        relation,
                        kind,
                        object,
                        acl,
                    })?;
                }
            }
            Ok(())
        };
    for (relation, table) in &snapshot.tables {
        include(
            i64::from(table.catalog_oids.relation),
            relation,
            RelationKind::Table,
            table.object_id,
            Some(&table.security.acl_revisions),
        )?;
    }
    for (relation, view) in snapshot.definitions.views.iter() {
        include(
            super::projection::view_relation_oid(view),
            relation,
            RelationKind::View,
            view.object_id,
            Some(&view.security.acl_revisions),
        )?;
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
                .map(|security| &security.acl_revisions),
        )?;
    }
    for (relation, object) in snapshot.definitions.sequence_object_ids.iter() {
        include(
            catalog.sequence_catalog_oid(object),
            relation,
            RelationKind::Sequence,
            *object,
            None,
        )?;
    }
    let mut indexes = OwnedMap::new();
    for (relation, row) in snapshot.definitions.catalog_indexes.iter() {
        control.check()?;
        let identity = uqa_sql::catalog::index::stored::catalog_object_identity(
            row.definition_json.as_deref(),
        )
        .map_err(|error| SQLError::Internal(error.to_string()))?;
        let Some(identity) = identity else {
            let name = relation.qualified_name_with_control(control)?;
            return Err(SQLError::Internal(
                control
                    .format(format_args!(
                        "index `{}` has no catalog identity",
                        name.as_str()
                    ))?
                    .into_parts()
                    .0,
            ));
        };
        include(
            identity.oid,
            relation,
            RelationKind::Index,
            identity.object_id,
            None,
        )?;
        let key = row.table_name.as_str();
        if !indexes.contains_key(key) {
            insert(&mut indexes, key, ProductionVec::new(*control), control)?;
        }
        indexes
            .get_mut(key)
            .expect("inserted index scope")
            .push_copy(identity.object_id)?;
    }
    Ok(SelectedRelations {
        objects: selected.finish()?,
        indexes,
    })
}

fn read_selected_relations(
    catalog: &CatalogReadView,
    selected: SelectedRelations<'_, '_>,
    output: &mut PreparedDependencySnapshot,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let keys = revision_keys(&selected.objects, control)?;
    let mut records = ProductionVec::new(*control);
    for (dependency, key) in selected.objects.iter().zip(keys.iter()) {
        records.push_copy(key.as_ref().map_or(
            CatalogRecordRef::Relation(dependency.kind, dependency.relation),
            |key| CatalogRecordRef::Metadata(key),
        ))?;
    }
    let revisions = catalog
        .prepared_catalog
        .as_ref()
        .map_or(Ok(None), |catalog| {
            catalog.record_revisions_with_control(&records, control)
        })
        .map_err(|error| {
            uqa_sql::catalog::errors::storage_error("prepared dependency snapshot", &error)
        })?;
    if revisions
        .as_ref()
        .is_some_and(|values| values.len() != records.len())
    {
        return Err(SQLError::Internal(
            "catalog returned a different number of prepared dependency revisions".into(),
        ));
    }
    for (index, dependency) in selected.objects.iter().enumerate() {
        control.check()?;
        let schema = control.copy_text(&dependency.relation.schema)?;
        let name = control.copy_text(&dependency.relation.name)?;
        let (schema, schema_memory) = schema.into_parts();
        let (name, name_memory) = name.into_parts();
        let mut memory = control.combine(schema_memory, name_memory);
        let acl = if let Some(acl) = dependency.acl {
            let mut columns = OwnedMap::new();
            for (name, identity) in &acl.columns {
                let (name, name_memory) = control.copy_text(name)?.into_parts();
                insert(&mut columns, name, *identity, control)?;
                memory = control.combine(memory, name_memory);
            }
            Some(AclRevision {
                relation: acl.relation,
                columns,
            })
        } else {
            None
        };
        let mut indexes = ProductionVec::new(*control);
        if dependency.kind == RelationKind::Table {
            let key = dependency.relation.qualified_name_with_control(control)?;
            if let Some(identities) = selected.indexes.get(key.as_str()) {
                indexes.extend_from_slice(identities)?;
            }
            uqa_core::ordering::sort_by_with_control(
                indexes.as_mut_slice(),
                &mut || control.check().map_err(SQLError::from),
                |left, right, _| Ok(left.cmp(right)),
            )?;
        }
        let (indexes, index_memory) = indexes.finish()?.into_parts();
        memory = control.combine(memory, index_memory);
        let relation = RelationRevision {
            object: dependency.object,
            acl,
            kind: dependency.kind,
            relation: RelationIdentity { schema, name },
            record: revisions
                .as_ref()
                .map_or(RecordRevision::Unavailable, |values| {
                    RecordRevision::Selected(values[index].clone())
                }),
            indexes,
        };
        let revision =
            PreparedDependencyRevision::from_produced(control.finish(relation, memory)?, control)?;
        *output
            .relations
            .get_mut(&dependency.oid)
            .expect("selected dependency slot") = Some(revision);
    }
    Ok(())
}

#[cfg(test)]
mod tests;

fn revision_keys(
    dependencies: &[RelationDependency<'_>],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Option<String>>>, SQLError> {
    let mut keys = ProductionVec::new(*control);
    for dependency in dependencies {
        let key = match dependency.kind {
            RelationKind::Table => Some(super::definition_revision::table_key_with_control(
                dependency.object,
                control,
            )?),
            RelationKind::View => Some(super::definition_revision::view_key_with_control(
                dependency.object,
                control,
            )?),
            _ => None,
        };
        let (key, memory) = key.map_or_else(
            || (None, control.empty_reservation()),
            |key| {
                let (key, memory) = key.into_parts();
                (Some(key), memory)
            },
        );
        keys.push_produced(control.finish(key, memory)?)?;
    }
    Ok(keys.finish()?)
}
