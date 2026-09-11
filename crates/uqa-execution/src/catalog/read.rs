//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable statement catalog snapshots and relation-name resolution.

mod indexes;
mod privileges;

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use uqa_graph::GraphStore;
use uqa_sql::SQLError;

use super::{
    CatalogReadView, CatalogSequenceSnapshot, CatalogTableSnapshot, RelationLookupMode,
    RelationNameResolution, RelationResolution,
};

impl CatalogReadView {
    /// Produce the only relation candidate set exposed to SQL binding and execution. Dynamic names are filtered by namespace `USAGE`; stored bindings must already be canonical and therefore bypass name lookup entirely.
    fn relation_lookup_candidates(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Vec<uqa_core::RelationIdentity>, SQLError> {
        if resolution.lookup_mode() == RelationLookupMode::Bound {
            let (schema, relation) =
                uqa_core::RelationIdentity::parse_reference(name).map_err(|error| {
                    SQLError::Internal(format!("decode bound relation `{name}`: {error}"))
                })?;
            let schema = schema.ok_or_else(|| {
                SQLError::Internal(format!(
                    "bound query contains non-canonical relation reference `{name}`"
                ))
            })?;
            return Ok(vec![uqa_core::RelationIdentity::new(schema, relation)]);
        }

        let qualified = uqa_core::RelationIdentity::parse_reference(name)
            .map_err(SQLError::Internal)?
            .0
            .is_some();
        let mut visible = Vec::new();
        for relation in resolution.raw_relation_lookup_candidates(name)? {
            if self
                .snapshot
                .definitions
                .schemas
                .contains_key(&relation.schema)
                && !self.schema_has_privilege_to(
                    &relation.schema,
                    &resolution.current_user,
                    crate::catalog::security::schema::SchemaAclPrivilege::Usage,
                )
            {
                if qualified {
                    return Err(SQLError::Routine {
                        sqlstate: "42501".into(),
                        message: format!("permission denied for schema {}", relation.schema),
                    });
                }
                continue;
            }
            visible.push(relation);
        }
        Ok(visible)
    }

    fn relation_exists(&self, relation: &uqa_core::RelationIdentity) -> bool {
        self.snapshot.tables.contains_key(relation)
            || self.snapshot.definitions.views.contains_key(relation)
            || self.snapshot.definitions.sequences.contains_key(relation)
            || self
                .snapshot
                .definitions
                .foreign_tables
                .contains_key(relation)
            || self
                .snapshot
                .definitions
                .catalog_indexes
                .contains_key(relation)
            || self.has_constraint_index(relation)
    }

    fn namespace_exists(&self, resolution: &RelationNameResolution, schema: &str) -> bool {
        if schema == resolution.temporary_schema {
            return resolution.temporary_namespace_allocated;
        }
        uqa_sql::catalog::is_virtual_system_schema(schema)
            || self.snapshot.definitions.schemas.contains_key(schema)
            || self.snapshot.definitions.graphs.contains_key(schema)
    }

    pub fn relation_kind_resolution(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<RelationResolution, SQLError> {
        if resolution.lookup_mode() == RelationLookupMode::Dynamic {
            if let Some((requested, resolved)) = resolution.qualified_schema(name)? {
                if !self.namespace_exists(resolution, &resolved) {
                    return Ok(RelationResolution::MissingSchema(requested));
                }
            }
        }
        for relation in self.relation_lookup_candidates(resolution, name)? {
            let kind = if self.snapshot.tables.contains_key(&relation) {
                Some("table")
            } else if let Some(view) = self.snapshot.definitions.views.get(&relation) {
                Some(match view.kind {
                    crate::catalog::view::StoredViewKind::View => "view",
                    crate::catalog::view::StoredViewKind::Materialized => "materialized view",
                })
            } else if self.snapshot.definitions.sequences.contains_key(&relation) {
                Some("sequence")
            } else if self
                .snapshot
                .definitions
                .foreign_tables
                .contains_key(&relation)
            {
                Some("foreign table")
            } else if self
                .snapshot
                .definitions
                .catalog_indexes
                .contains_key(&relation)
                || self.has_constraint_index(&relation)
            {
                Some("index")
            } else {
                None
            };
            if let Some(kind) = kind {
                return Ok(RelationResolution::Found(relation.qualified_name(), kind));
            }
        }
        Ok(RelationResolution::MissingRelation)
    }

    pub fn all_schema_names(&self, resolution: &RelationNameResolution) -> Vec<String> {
        let mut schemas = vec![
            "pg_catalog".to_string(),
            "information_schema".to_string(),
            "ag_catalog".to_string(),
        ];
        schemas.extend(self.snapshot.definitions.schemas.keys().cloned());
        schemas.extend(self.snapshot.definitions.graphs.keys().cloned());
        let temporary_schema = resolution.temporary_schema.clone();
        let has_temporary_relation = self
            .snapshot
            .tables
            .iter()
            .any(|(relation, _)| relation.schema == temporary_schema)
            || self
                .snapshot
                .definitions
                .views
                .keys()
                .any(|relation| relation.schema == temporary_schema)
            || self.snapshot.definitions.sequence_persistence.iter().any(
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

    pub fn schema_security(&self, name: &str) -> Option<&crate::catalog::security::SchemaSecurity> {
        self.snapshot.definitions.schemas.get(name)
    }

    pub fn database_security(&self) -> &crate::catalog::security::DatabaseSecurity {
        &self.snapshot.definitions.database_security
    }

    pub fn has_schema(&self, name: &str) -> bool {
        self.snapshot.definitions.schemas.contains_key(name)
    }

    pub fn table_names(&self) -> Vec<String> {
        self.snapshot
            .tables
            .keys()
            .map(uqa_core::RelationIdentity::qualified_name)
            .collect()
    }

    pub fn roles(&self) -> impl Iterator<Item = &uqa_sql::catalog::roles::RoleDefinition> {
        self.snapshot.definitions.roles.values()
    }

    pub fn role_memberships(
        &self,
    ) -> impl Iterator<Item = &uqa_sql::catalog::roles::RoleMembership> {
        self.snapshot.definitions.role_memberships.values()
    }

    pub fn sequences(
        &self,
    ) -> Vec<(
        String,
        uqa_sql::ast::RelationPersistence,
        [u8; 16],
        crate::catalog::security::SequenceSecurity,
    )> {
        self.snapshot
            .definitions
            .sequences
            .keys()
            .map(|identity| {
                (
                    identity.qualified_name(),
                    self.snapshot
                        .definitions
                        .sequence_persistence
                        .get(identity)
                        .copied()
                        .unwrap_or_default(),
                    self.snapshot
                        .definitions
                        .sequence_object_ids
                        .get(identity)
                        .copied()
                        .unwrap_or_default(),
                    self.snapshot
                        .definitions
                        .sequence_security
                        .get(identity)
                        .cloned()
                        .unwrap_or_else(|| crate::catalog::security::SequenceSecurity {
                            role_owner: "uqa".into(),
                            acl: None,
                        }),
                )
            })
            .collect()
    }

    pub fn sequence_states(
        &self,
    ) -> Vec<(
        uqa_core::RelationIdentity,
        crate::catalog::sequence::SequenceState,
        uqa_sql::ast::RelationPersistence,
        crate::catalog::security::SequenceSecurity,
    )> {
        self.snapshot
            .definitions
            .sequences
            .iter()
            .map(|(identity, state)| {
                (
                    identity.clone(),
                    *state,
                    self.snapshot
                        .definitions
                        .sequence_persistence
                        .get(identity)
                        .copied()
                        .unwrap_or_default(),
                    self.snapshot
                        .definitions
                        .sequence_security
                        .get(identity)
                        .cloned()
                        .unwrap_or_else(|| crate::catalog::security::SequenceSecurity {
                            role_owner: "uqa".into(),
                            acl: None,
                        }),
                )
            })
            .collect()
    }

    pub fn sequence_is_visible_to(
        &self,
        security: &crate::catalog::security::SequenceSecurity,
        role: &str,
    ) -> bool {
        crate::catalog::security::sequence::role_can_view_sequence(
            security,
            role,
            &self.snapshot.definitions.roles,
            &self.snapshot.definitions.role_memberships,
        )
    }

    pub fn sequence_is_selectable_to(
        &self,
        security: &crate::catalog::security::SequenceSecurity,
        role: &str,
    ) -> bool {
        crate::catalog::security::sequence::role_can_select_sequence(
            security,
            role,
            &self.snapshot.definitions.roles,
            &self.snapshot.definitions.role_memberships,
        )
    }

    pub fn sequence_value_is_readable_to(
        &self,
        security: &crate::catalog::security::SequenceSecurity,
        role: &str,
    ) -> bool {
        crate::catalog::security::sequence::role_can_read_sequence_value(
            security,
            role,
            &self.snapshot.definitions.roles,
            &self.snapshot.definitions.role_memberships,
        )
    }

    pub fn sequence_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<CatalogSequenceSnapshot>, SQLError> {
        for relation in self.relation_lookup_candidates(resolution, name)? {
            if let Some(state) = self.snapshot.definitions.sequences.get(&relation) {
                return Ok(Some(CatalogSequenceSnapshot {
                    relation: relation.clone(),
                    state: *state,
                    security: self
                        .snapshot
                        .definitions
                        .sequence_security
                        .get(&relation)
                        .cloned()
                        .unwrap_or_else(|| crate::catalog::security::SequenceSecurity {
                            role_owner: "uqa".into(),
                            acl: None,
                        }),
                }));
            }
            if self.snapshot.tables.contains_key(&relation)
                || self.snapshot.definitions.views.contains_key(&relation)
                || self
                    .snapshot
                    .definitions
                    .foreign_tables
                    .contains_key(&relation)
                || self
                    .snapshot
                    .definitions
                    .catalog_indexes
                    .contains_key(&relation)
            {
                return Ok(None);
            }
        }
        Ok(None)
    }

    pub fn schema_has_privilege_to(
        &self,
        schema: &str,
        role: &str,
        privilege: crate::catalog::security::schema::SchemaAclPrivilege,
    ) -> bool {
        let Some(security) = self.snapshot.definitions.schemas.get(schema) else {
            return true;
        };
        crate::catalog::security::schema::role_has_schema_privilege(
            security,
            role,
            privilege,
            &self.snapshot.definitions.roles,
            &self.snapshot.definitions.role_memberships,
        )
    }

    pub fn views_of_kind(
        &self,
        kind: crate::catalog::view::StoredViewKind,
    ) -> Vec<(String, crate::catalog::view::StoredView)> {
        self.snapshot
            .definitions
            .views
            .iter()
            .filter(|(_, view)| view.kind == kind)
            .map(|(identity, view)| (identity.qualified_name(), view.clone()))
            .collect()
    }

    pub fn foreign_table_names(&self) -> Vec<String> {
        self.snapshot
            .definitions
            .foreign_tables
            .keys()
            .map(uqa_core::RelationIdentity::qualified_name)
            .collect()
    }

    pub fn foreign_tables(&self) -> Vec<(String, crate::catalog::foreign::StoredForeignTable)> {
        self.snapshot
            .definitions
            .foreign_tables
            .iter()
            .map(|(identity, table)| (identity.qualified_name(), table.clone()))
            .collect()
    }

    pub fn foreign_table_security(
        &self,
        name: &str,
    ) -> Result<&crate::catalog::security::TableSecurity, SQLError> {
        let relation = uqa_core::RelationIdentity::from_legacy_name(name).map_err(|error| {
            SQLError::Internal(format!("resolve catalog foreign table `{name}`: {error}"))
        })?;
        self.snapshot
            .definitions
            .foreign_table_security
            .get(&relation)
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "catalog foreign table `{name}` has no security metadata"
                ))
            })
    }

    pub fn catalog_indexes(&self) -> impl Iterator<Item = &uqa_storage::CatalogIndexRow> {
        self.snapshot.definitions.catalog_indexes.values()
    }

    pub fn triggers(&self) -> Vec<uqa_sql::catalog::events::StoredTrigger> {
        self.snapshot
            .definitions
            .triggers
            .values()
            .flat_map(|triggers| triggers.values().cloned())
            .collect()
    }

    pub fn rules(&self) -> Vec<uqa_sql::catalog::events::StoredRule> {
        self.snapshot
            .definitions
            .rules
            .values()
            .flat_map(|rules| rules.values().cloned())
            .collect()
    }

    /// Share the captured domain allocation with static signature analysis.
    pub fn domain_snapshot(&self) -> Arc<BTreeMap<String, uqa_sql::catalog::domain::StoredDomain>> {
        self.snapshot.definitions.domains.clone()
    }

    pub fn domains(&self) -> impl Iterator<Item = &uqa_sql::catalog::domain::StoredDomain> {
        self.snapshot.definitions.domains.values()
    }

    pub fn sql_functions(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<Vec<std::sync::Arc<uqa_sql::routines::SQLUserFunction>>>, SQLError> {
        let (schema, local_name) =
            uqa_core::RelationIdentity::parse_reference(name).map_err(|error| {
                SQLError::Routine {
                    sqlstate: "42602".into(),
                    message: format!("invalid routine name `{name}`: {error}"),
                }
            })?;
        let keys = schema.map_or_else(
            || {
                resolution
                    .search_path
                    .iter()
                    .map(|schema| {
                        uqa_core::RelationIdentity::new(schema, &local_name).qualified_name()
                    })
                    .collect::<Vec<_>>()
            },
            |schema| vec![uqa_core::RelationIdentity::new(schema, &local_name).qualified_name()],
        );
        let mut visible = Vec::new();
        let mut seen = BTreeSet::new();
        for key in keys {
            let Some(overloads) = self.snapshot.definitions.sql_user_functions.get(&key) else {
                continue;
            };
            for function in overloads {
                let identity = (
                    uqa_sql::routines::routine_signature_types(&function.def),
                    function.def.is_procedure,
                );
                if seen.insert(identity) {
                    visible.push(function.clone());
                }
            }
        }
        Ok((!visible.is_empty()).then_some(visible))
    }

    pub fn all_sql_functions(&self) -> Vec<std::sync::Arc<uqa_sql::routines::SQLUserFunction>> {
        self.snapshot
            .definitions
            .sql_user_functions
            .values()
            .flat_map(|functions| functions.iter().cloned())
            .collect()
    }

    pub fn graph_labels(
        &self,
        graph: &str,
    ) -> Result<Option<Vec<uqa_graph::GraphLabelInfo>>, SQLError> {
        let Some(store) = self.snapshot.definitions.graphs.get(graph) else {
            return Ok(None);
        };
        store.graph_labels(graph).map(Some).map_err(|error| {
            SQLError::Internal(format!("read graph `{graph}` catalog labels: {error}"))
        })
    }

    pub fn graph_names(&self) -> Vec<String> {
        self.snapshot.definitions.graphs.keys().cloned().collect()
    }

    pub fn graph_next_label_id(&self, graph: &str) -> Result<Option<u32>, SQLError> {
        self.snapshot
            .definitions
            .graphs
            .get(graph)
            .map(|store| {
                store
                    .label_registry(graph)
                    .map(|registry| registry.next_label_id)
            })
            .transpose()
            .map_err(|error| {
                SQLError::Internal(format!("read graph `{graph}` label sequence: {error}"))
            })
    }

    pub fn graph_label_count(
        &self,
        graph: &str,
        label: &str,
        kind: uqa_graph::LabelKind,
    ) -> Result<Option<usize>, SQLError> {
        let Some(store) = self.snapshot.definitions.graphs.get(graph) else {
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

    pub fn graph_vertices(&self, graph: &str) -> Result<Option<Vec<uqa_core::Vertex>>, SQLError> {
        let Some(store) = self.snapshot.definitions.graphs.get(graph) else {
            return Ok(None);
        };
        store
            .vertices_in_graph(graph)
            .map(Some)
            .map_err(|error| SQLError::Internal(format!("read graph `{graph}` vertices: {error}")))
    }

    pub fn graph_edges(&self, graph: &str) -> Result<Option<Vec<uqa_core::Edge>>, SQLError> {
        let Some(store) = self.snapshot.definitions.graphs.get(graph) else {
            return Ok(None);
        };
        store
            .edges_in_graph(graph)
            .map(Some)
            .map_err(|error| SQLError::Internal(format!("read graph `{graph}` edges: {error}")))
    }

    pub fn table(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<&CatalogTableSnapshot>, SQLError> {
        self.table_resolved(resolution, name)
    }

    pub fn table_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<&CatalogTableSnapshot>, SQLError> {
        for relation in self.relation_lookup_candidates(resolution, name)? {
            if let Some(table) = self.snapshot.tables.get(&relation) {
                return Ok(Some(table));
            }
            if self.relation_exists(&relation) {
                return Ok(None);
            }
        }
        Ok(None)
    }

    pub fn table_name(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<String>, SQLError> {
        self.table_name_resolved(resolution, name)
    }

    pub fn table_name_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<String>, SQLError> {
        for relation in self.relation_lookup_candidates(resolution, name)? {
            if self.snapshot.tables.contains_key(&relation) {
                return Ok(Some(relation.qualified_name()));
            }
            if self.relation_exists(&relation) {
                return Ok(None);
            }
        }
        Ok(None)
    }

    pub fn hierarchy_scan_tables(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        include_descendants: bool,
    ) -> Result<Vec<String>, SQLError> {
        let root = self
            .relation_lookup_candidates(resolution, name)?
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

    pub fn direct_hierarchy_children(
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

    pub fn table_has_rules(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<bool, SQLError> {
        let relation = self
            .relation_lookup_candidates(resolution, name)?
            .into_iter()
            .find(|relation| self.snapshot.tables.contains_key(relation))
            .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
        Ok(self
            .snapshot
            .definitions
            .rules
            .get(&relation)
            .is_some_and(|rules| !rules.is_empty()))
    }

    pub fn relation_has_triggers(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<bool, SQLError> {
        let relation = self
            .relation_lookup_candidates(resolution, name)?
            .into_iter()
            .find(|relation| {
                self.snapshot.tables.contains_key(relation)
                    || self.snapshot.definitions.views.contains_key(relation)
                    || self
                        .snapshot
                        .definitions
                        .foreign_tables
                        .contains_key(relation)
            })
            .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
        if !self.snapshot.tables.contains_key(&relation) {
            return Ok(self
                .snapshot
                .definitions
                .triggers
                .get(&relation)
                .is_some_and(|triggers| !triggers.is_empty()));
        }
        let sources = self.partition_trigger_sources(resolution, &relation.qualified_name())?;
        Ok(sources.iter().enumerate().any(|(index, source)| {
            self.snapshot
                .definitions
                .triggers
                .get(source)
                .is_some_and(|entries| {
                    entries
                        .values()
                        .any(|trigger| index == 0 || trigger.definition.row)
                })
        }))
    }

    pub fn partition_trigger_sources(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Vec<uqa_core::RelationIdentity>, SQLError> {
        let mut current = self
            .relation_lookup_candidates(resolution, name)?
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
            current = uqa_core::RelationIdentity::from_legacy_name(parent).map_err(|error| {
                SQLError::Internal(format!(
                    "decode query trigger partition parent `{parent}`: {error}"
                ))
            })?;
        }
        Ok(sources)
    }

    pub fn view_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<&crate::catalog::view::StoredView>, SQLError> {
        for relation in self.relation_lookup_candidates(resolution, name)? {
            if let Some(view) = self.snapshot.definitions.views.get(&relation) {
                return Ok(Some(view));
            }
            if self.relation_exists(&relation) {
                return Ok(None);
            }
        }
        Ok(None)
    }

    pub fn view_name_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<String>, SQLError> {
        for relation in self.relation_lookup_candidates(resolution, name)? {
            if self.snapshot.definitions.views.contains_key(&relation) {
                return Ok(Some(relation.qualified_name()));
            }
            if self.relation_exists(&relation) {
                return Ok(None);
            }
        }
        Ok(None)
    }

    pub fn foreign_table_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<&crate::catalog::foreign::StoredForeignTable>, SQLError> {
        for relation in self.relation_lookup_candidates(resolution, name)? {
            if let Some(table) = self.snapshot.definitions.foreign_tables.get(&relation) {
                return Ok(Some(table));
            }
            if self.relation_exists(&relation) {
                return Ok(None);
            }
        }
        Ok(None)
    }

    pub fn foreign_table_entry_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<(String, crate::catalog::foreign::StoredForeignTable)>, SQLError> {
        for relation in self.relation_lookup_candidates(resolution, name)? {
            if let Some(table) = self.snapshot.definitions.foreign_tables.get(&relation) {
                return Ok(Some((relation.qualified_name(), table.clone())));
            }
            if self.relation_exists(&relation) {
                return Ok(None);
            }
        }
        Ok(None)
    }

    fn collect_hierarchy_descendants(
        &self,
        parent: &uqa_core::RelationIdentity,
        visiting: &mut BTreeSet<uqa_core::RelationIdentity>,
        visited: &mut BTreeSet<uqa_core::RelationIdentity>,
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

mod lineage;
