//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable statement catalog snapshots and relation-name resolution.

#[cfg(test)]
use std::collections::BTreeMap;
use std::collections::BTreeSet;
#[cfg(test)]
use std::sync::Arc;

use uqa_graph::GraphStore;
use uqa_sql::SQLError;

#[cfg(test)]
use crate::engine_state::DurableCatalogState;

#[cfg(test)]
use super::CatalogReadSnapshot;
use super::{CatalogReadView, CatalogTableSnapshot, RelationNameResolution};

#[cfg(test)]
impl CatalogTableSnapshot {
    pub(crate) fn fixture(columns: Vec<uqa_sql::ast::ColumnDef>) -> Self {
        Self {
            object_id: [1; 16],
            columns,
            checks: Vec::new(),
            foreign_keys: Vec::new(),
            keys: Vec::new(),
            hierarchy: uqa_sql::ast::TableHierarchy::default(),
            persistence: uqa_sql::ast::RelationPersistence::Permanent,
        }
    }
}

impl RelationNameResolution {
    pub(crate) fn search_path(&self) -> &[String] {
        &self.search_path
    }

    pub(crate) fn search_path_contains(&self, schema: &str) -> bool {
        self.search_path.iter().any(|candidate| candidate == schema)
    }

    fn relation_lookup_candidates(
        &self,
        name: &str,
    ) -> Result<Vec<crate::RelationIdentity>, SQLError> {
        let (schema, relation) =
            crate::RelationIdentity::parse_reference(name).map_err(|error| {
                SQLError::Internal(format!("resolve catalog relation `{name}`: {error}"))
            })?;
        if let Some(schema) = schema {
            let schema = if schema == "pg_temp" {
                self.temporary_schema.clone()
            } else {
                schema
            };
            return Ok(vec![crate::RelationIdentity::new(schema, relation)]);
        }
        let mut candidates = vec![crate::RelationIdentity::new(
            &self.temporary_schema,
            &relation,
        )];
        candidates.extend(
            self.search_path
                .iter()
                .filter(|schema| *schema != "pg_catalog" && *schema != "information_schema")
                .map(|schema| crate::RelationIdentity::new(schema, &relation)),
        );
        Ok(candidates)
    }

    #[cfg(test)]
    pub(crate) fn fixture(search_path: Vec<String>, temporary_schema: String) -> Self {
        Self {
            search_path,
            temporary_schema,
        }
    }
}

impl CatalogReadView {
    pub(crate) fn all_schema_names(&self, resolution: &RelationNameResolution) -> Vec<String> {
        let mut schemas = vec![
            "pg_catalog".to_string(),
            "information_schema".to_string(),
            "ag_catalog".to_string(),
        ];
        schemas.extend(self.snapshot.durable.schemas.iter().cloned());
        schemas.extend(self.snapshot.durable.graphs.keys().cloned());
        let temporary_schema = resolution.temporary_schema.clone();
        let has_temporary_relation =
            self.snapshot
                .tables
                .iter()
                .any(|(relation, _)| relation.schema == temporary_schema)
                || self
                    .snapshot
                    .durable
                    .views
                    .keys()
                    .any(|relation| relation.schema == temporary_schema)
                || self.snapshot.durable.sequence_persistence.iter().any(
                    |(relation, persistence)| {
                        relation.schema == temporary_schema
                            && *persistence == uqa_sql::ast::RelationPersistence::Temporary
                    },
                );
        if has_temporary_relation {
            schemas.push(temporary_schema);
        }
        schemas.sort();
        schemas.dedup();
        schemas
    }

    #[cfg(test)]
    pub(crate) fn has_schema(&self, name: &str) -> bool {
        self.snapshot.durable.schemas.contains(name)
    }

    pub(crate) fn table_names(&self) -> Vec<String> {
        self.snapshot
            .tables
            .keys()
            .map(crate::RelationIdentity::qualified_name)
            .collect()
    }

    pub(crate) fn roles(&self) -> impl Iterator<Item = &crate::engine_roles::RoleDefinition> {
        self.snapshot.durable.roles.values()
    }

    pub(crate) fn role_memberships(
        &self,
    ) -> impl Iterator<Item = &crate::engine_roles::RoleMembership> {
        self.snapshot.durable.role_memberships.values()
    }

    pub(crate) fn sequences(
        &self,
    ) -> Vec<(
        String,
        uqa_sql::ast::RelationPersistence,
        [u8; 16],
        crate::engine_state::SequenceSecurity,
    )> {
        self.snapshot
            .durable
            .sequences
            .keys()
            .map(|identity| {
                (
                    identity.qualified_name(),
                    self.snapshot
                        .durable
                        .sequence_persistence
                        .get(identity)
                        .copied()
                        .unwrap_or_default(),
                    self.snapshot
                        .durable
                        .sequence_object_ids
                        .get(identity)
                        .copied()
                        .unwrap_or_default(),
                    self.snapshot
                        .durable
                        .sequence_security
                        .get(identity)
                        .cloned()
                        .unwrap_or_else(|| crate::engine_state::SequenceSecurity {
                            role_owner: "uqa".into(),
                            acl: None,
                        }),
                )
            })
            .collect()
    }

    pub(crate) fn sequence_states(&self) -> Vec<(String, crate::SequenceState, String)> {
        self.snapshot
            .durable
            .sequences
            .iter()
            .map(|(identity, state)| {
                (
                    identity.qualified_name(),
                    *state,
                    self.snapshot
                        .durable
                        .sequence_security
                        .get(identity)
                        .map_or_else(|| "uqa".into(), |security| security.role_owner.clone()),
                )
            })
            .collect()
    }

    pub(crate) fn views_of_kind(
        &self,
        kind: crate::StoredViewKind,
    ) -> Vec<(String, crate::StoredView)> {
        self.snapshot
            .durable
            .views
            .iter()
            .filter(|(_, view)| view.kind == kind)
            .map(|(identity, view)| (identity.qualified_name(), view.clone()))
            .collect()
    }

    pub(crate) fn foreign_table_names(&self) -> Vec<String> {
        self.snapshot
            .durable
            .foreign_tables
            .keys()
            .map(crate::RelationIdentity::qualified_name)
            .collect()
    }

    pub(crate) fn foreign_tables(&self) -> Vec<(String, uqa_fdw::ForeignTable)> {
        self.snapshot
            .durable
            .foreign_tables
            .iter()
            .map(|(identity, table)| (identity.qualified_name(), table.clone()))
            .collect()
    }

    pub(crate) fn catalog_indexes(&self) -> impl Iterator<Item = &uqa_storage::CatalogIndexRow> {
        self.snapshot.durable.catalog_indexes.values()
    }

    pub(crate) fn triggers(&self) -> Vec<crate::engine_events::StoredTrigger> {
        self.snapshot
            .durable
            .triggers
            .values()
            .flat_map(|triggers| triggers.values().cloned())
            .collect()
    }

    pub(crate) fn rules(&self) -> Vec<crate::engine_events::StoredRule> {
        self.snapshot
            .durable
            .rules
            .values()
            .flat_map(|rules| rules.values().cloned())
            .collect()
    }

    pub(crate) fn sql_functions(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<Vec<std::sync::Arc<crate::engine_user_functions::SQLUserFunction>>>, SQLError>
    {
        let (schema, local_name) =
            crate::RelationIdentity::parse_reference(name).map_err(|error| SQLError::Routine {
                sqlstate: "42602".into(),
                message: format!("invalid routine name `{name}`: {error}"),
            })?;
        let keys = schema.map_or_else(
            || {
                resolution
                    .search_path
                    .iter()
                    .map(|schema| {
                        crate::RelationIdentity::new(schema, &local_name).qualified_name()
                    })
                    .collect::<Vec<_>>()
            },
            |schema| vec![crate::RelationIdentity::new(schema, &local_name).qualified_name()],
        );
        let mut visible = Vec::new();
        let mut seen = BTreeSet::new();
        for key in keys {
            let Some(overloads) = self.snapshot.durable.sql_user_functions.get(&key) else {
                continue;
            };
            for function in overloads {
                let identity = (
                    crate::engine_user_functions::routine_signature_types(&function.def),
                    function.def.is_procedure,
                );
                if seen.insert(identity) {
                    visible.push(function.clone());
                }
            }
        }
        Ok((!visible.is_empty()).then_some(visible))
    }

    pub(crate) fn all_sql_functions(
        &self,
    ) -> Vec<std::sync::Arc<crate::engine_user_functions::SQLUserFunction>> {
        self.snapshot
            .durable
            .sql_user_functions
            .values()
            .flat_map(|functions| functions.iter().cloned())
            .collect()
    }

    pub(crate) fn graph_labels(
        &self,
        graph: &str,
    ) -> Result<Option<Vec<uqa_graph::GraphLabelInfo>>, SQLError> {
        let Some(store) = self.snapshot.durable.graphs.get(graph) else {
            return Ok(None);
        };
        store.graph_labels(graph).map(Some).map_err(|error| {
            SQLError::Internal(format!("read graph `{graph}` catalog labels: {error}"))
        })
    }

    pub(crate) fn graph_names(&self) -> Vec<String> {
        self.snapshot.durable.graphs.keys().cloned().collect()
    }

    pub(crate) fn graph_next_label_id(&self, graph: &str) -> Option<u32> {
        self.snapshot
            .durable
            .graphs
            .get(graph)
            .map(|store| store.label_registry(graph).next_label_id)
    }

    pub(crate) fn graph_label_count(
        &self,
        graph: &str,
        label: &str,
        kind: uqa_graph::LabelKind,
    ) -> Result<Option<usize>, SQLError> {
        let Some(store) = self.snapshot.durable.graphs.get(graph) else {
            return Ok(None);
        };
        let count = match kind {
            uqa_graph::LabelKind::Vertex => store
                .vertex_ids_by_label(label, graph)
                .map(|identities| identities.len()),
            uqa_graph::LabelKind::Edge => store
                .edge_ids_by_label(label, graph)
                .map(|identities| identities.len()),
        }
        .map_err(|error| {
            SQLError::Internal(format!("read graph `{graph}` label `{label}`: {error}"))
        })?;
        Ok(Some(count))
    }

    pub(crate) fn graph_vertices(
        &self,
        graph: &str,
    ) -> Result<Option<Vec<uqa_core::Vertex>>, SQLError> {
        let Some(store) = self.snapshot.durable.graphs.get(graph) else {
            return Ok(None);
        };
        store
            .vertices_in_graph(graph)
            .map(Some)
            .map_err(|error| SQLError::Internal(format!("read graph `{graph}` vertices: {error}")))
    }

    pub(crate) fn graph_edges(&self, graph: &str) -> Result<Option<Vec<uqa_core::Edge>>, SQLError> {
        let Some(store) = self.snapshot.durable.graphs.get(graph) else {
            return Ok(None);
        };
        store
            .edges_in_graph(graph)
            .map(Some)
            .map_err(|error| SQLError::Internal(format!("read graph `{graph}` edges: {error}")))
    }

    pub(crate) fn table(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<&CatalogTableSnapshot>, SQLError> {
        self.table_resolved(resolution, name)
    }

    pub(crate) fn table_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<&CatalogTableSnapshot>, SQLError> {
        Ok(resolution
            .relation_lookup_candidates(name)?
            .into_iter()
            .find_map(|relation| self.snapshot.tables.get(&relation)))
    }

    pub(crate) fn table_name(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<String>, SQLError> {
        self.table_name_resolved(resolution, name)
    }

    pub(crate) fn table_name_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<String>, SQLError> {
        Ok(resolution
            .relation_lookup_candidates(name)?
            .into_iter()
            .find(|relation| self.snapshot.tables.contains_key(relation))
            .map(|relation| relation.qualified_name()))
    }

    pub(crate) fn hierarchy_scan_tables(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        include_descendants: bool,
    ) -> Result<Vec<String>, SQLError> {
        let root = resolution
            .relation_lookup_candidates(name)?
            .into_iter()
            .find(|relation| self.snapshot.tables.contains_key(relation))
            .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
        if !include_descendants {
            return Ok(vec![root.qualified_name()]);
        }
        let mut output = Vec::new();
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        self.collect_hierarchy_descendants(&root, &mut visiting, &mut visited, &mut output)?;
        Ok(output)
    }

    pub(crate) fn direct_hierarchy_children(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Vec<String>, SQLError> {
        let parent = self
            .table_name(resolution, name)?
            .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
        Ok(self
            .snapshot
            .tables
            .iter()
            .filter(|(_, table)| table.hierarchy.parents.iter().any(|item| item == &parent))
            .map(|(identity, _)| identity.qualified_name())
            .collect())
    }

    pub(crate) fn table_has_rules(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<bool, SQLError> {
        let relation = resolution
            .relation_lookup_candidates(name)?
            .into_iter()
            .find(|relation| self.snapshot.tables.contains_key(relation))
            .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
        Ok(self
            .snapshot
            .durable
            .rules
            .get(&relation)
            .is_some_and(|rules| !rules.is_empty()))
    }

    pub(crate) fn relation_has_triggers(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<bool, SQLError> {
        let relation = resolution
            .relation_lookup_candidates(name)?
            .into_iter()
            .find(|relation| {
                self.snapshot.tables.contains_key(relation)
                    || self.snapshot.durable.views.contains_key(relation)
                    || self.snapshot.durable.foreign_tables.contains_key(relation)
            })
            .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
        if !self.snapshot.tables.contains_key(&relation) {
            return Ok(self
                .snapshot
                .durable
                .triggers
                .get(&relation)
                .is_some_and(|triggers| !triggers.is_empty()));
        }
        let sources = self.partition_trigger_sources(resolution, &relation.qualified_name())?;
        Ok(sources.iter().enumerate().any(|(index, source)| {
            self.snapshot
                .durable
                .triggers
                .get(source)
                .is_some_and(|entries| {
                    entries
                        .values()
                        .any(|trigger| index == 0 || trigger.definition.row)
                })
        }))
    }

    pub(crate) fn partition_trigger_sources(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Vec<crate::RelationIdentity>, SQLError> {
        let mut current = resolution
            .relation_lookup_candidates(name)?
            .into_iter()
            .find(|relation| self.snapshot.tables.contains_key(relation))
            .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
        let mut sources = Vec::new();
        let mut visited = BTreeSet::new();
        loop {
            if !visited.insert(current.clone()) {
                return Err(SQLError::Internal(format!(
                    "trigger partition hierarchy contains a cycle at `{}`",
                    current.qualified_name()
                )));
            }
            sources.push(current.clone());
            let hierarchy = &self
                .snapshot
                .tables
                .get(&current)
                .ok_or_else(|| SQLError::UnknownTable(current.qualified_name()))?
                .hierarchy;
            if hierarchy.partition_bound.is_none() {
                break;
            }
            let Some(parent) = hierarchy.parents.first() else {
                return Err(SQLError::Internal(format!(
                    "partition `{}` has no parent",
                    current.qualified_name()
                )));
            };
            current = crate::RelationIdentity::from_legacy_name(parent).map_err(|error| {
                SQLError::Internal(format!(
                    "decode query trigger partition parent `{parent}`: {error}"
                ))
            })?;
        }
        Ok(sources)
    }

    pub(crate) fn view_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<&crate::StoredView>, SQLError> {
        Ok(resolution
            .relation_lookup_candidates(name)?
            .into_iter()
            .find_map(|relation| self.snapshot.durable.views.get(&relation)))
    }

    pub(crate) fn view_name_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<String>, SQLError> {
        Ok(resolution
            .relation_lookup_candidates(name)?
            .into_iter()
            .find(|relation| self.snapshot.durable.views.contains_key(relation))
            .map(|relation| relation.qualified_name()))
    }

    pub(crate) fn foreign_table_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<&uqa_fdw::ForeignTable>, SQLError> {
        Ok(resolution
            .relation_lookup_candidates(name)?
            .into_iter()
            .find_map(|relation| self.snapshot.durable.foreign_tables.get(&relation)))
    }

    pub(crate) fn foreign_table_entry_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<(String, uqa_fdw::ForeignTable)>, SQLError> {
        Ok(resolution
            .relation_lookup_candidates(name)?
            .into_iter()
            .find_map(|relation| {
                self.snapshot
                    .durable
                    .foreign_tables
                    .get(&relation)
                    .cloned()
                    .map(|table| (relation.qualified_name(), table))
            }))
    }

    #[cfg(test)]
    pub(crate) fn fixture(tables: BTreeMap<crate::RelationIdentity, CatalogTableSnapshot>) -> Self {
        Self {
            snapshot: Arc::new(CatalogReadSnapshot {
                tables,
                durable: Arc::new(DurableCatalogState::new().snapshot()),
            }),
        }
    }

    fn collect_hierarchy_descendants(
        &self,
        parent: &crate::RelationIdentity,
        visiting: &mut BTreeSet<crate::RelationIdentity>,
        visited: &mut BTreeSet<crate::RelationIdentity>,
        output: &mut Vec<String>,
    ) -> Result<(), SQLError> {
        if visiting.contains(parent) {
            return Err(SQLError::Internal(format!(
                "table inheritance cycle reaches `{}`",
                parent.qualified_name()
            )));
        }
        if !visited.insert(parent.clone()) {
            return Ok(());
        }
        visiting.insert(parent.clone());
        output.push(parent.qualified_name());
        let parent_name = parent.qualified_name();
        let children = self
            .snapshot
            .tables
            .iter()
            .filter(|(_, table)| {
                table
                    .hierarchy
                    .parents
                    .iter()
                    .any(|candidate| candidate == &parent_name)
            })
            .map(|(identity, _)| identity.clone())
            .collect::<Vec<_>>();
        for child in children {
            self.collect_hierarchy_descendants(&child, visiting, visited, output)?;
        }
        visiting.remove(parent);
        Ok(())
    }
}
