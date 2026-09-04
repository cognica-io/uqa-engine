//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable view registration, binding, dependencies, and restoration.

mod materialized;
mod options;
mod ownership;
mod registration;

use super::{
    bind_query_plan_relations, bind_query_plan_sequence_references,
    canonical_virtual_relation_reference, query_plan_references_relation,
    query_plan_references_sequence, BTreeMap, CatalogFacade, Engine, QueryPlan, RelationIdentity,
    SQLError, StorageBackendError, StorageBackendResult, StoredView, StoredViewKind, ViewRow,
};
use uqa_sql::ast::FunctionBinding;

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum RestoredView {
    Current(StoredView),
    Legacy(QueryPlan),
}

pub(crate) fn catalog_view_row(
    relation: &RelationIdentity,
    view: &StoredView,
) -> Result<ViewRow, serde_json::Error> {
    Ok(ViewRow {
        relation: relation.clone(),
        role_owner: view.role_owner.clone(),
        acl: view.acl.clone(),
        column_acls: view.column_acls.clone(),
        definition_json: serde_json::to_string(view)?,
    })
}

fn upgrade_legacy_view_dispatches(plan: &mut QueryPlan) -> bool {
    let mut changed = false;
    plan.rewrite_scalar_expressions(&mut |expression| {
        let uqa_execution::ScalarExpr::Func { name, binding, .. } = expression else {
            return;
        };
        changed |= FunctionBinding::upgrade_legacy_serialized_dispatch(name, binding);
    });
    changed
}

pub(crate) struct ViewRegistration<'a> {
    pub name: &'a str,
    pub column_names: &'a [String],
    pub plan: QueryPlan,
    pub or_replace: bool,
    pub persistence: uqa_sql::ast::RelationPersistence,
    pub options: &'a [(String, String)],
    pub params: &'a [uqa_sql::SQLParam],
}

pub(crate) struct MaterializedViewRegistration<'a> {
    pub name: &'a str,
    pub column_names: &'a [String],
    pub plan: QueryPlan,
    pub if_not_exists: bool,
    pub with_no_data: bool,
    pub options: &'a [(String, String)],
    pub params: &'a [uqa_sql::SQLParam],
}

fn create_view_output_columns(
    schema: &uqa_execution::RowSchema,
    declared: &[String],
) -> Result<Vec<String>, SQLError> {
    if declared.len() > schema.len() {
        return Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: "CREATE VIEW specifies more column names than columns".into(),
        });
    }
    let columns = schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, column)| {
            declared
                .get(position)
                .cloned()
                .unwrap_or_else(|| schema.public_name(position).unwrap_or(column).to_string())
        })
        .collect::<Vec<_>>();
    let mut seen = std::collections::BTreeSet::new();
    for column in &columns {
        if !seen.insert(column) {
            return Err(SQLError::Routine {
                sqlstate: "42701".into(),
                message: format!("column \"{column}\" specified more than once"),
            });
        }
    }
    Ok(columns)
}

fn named_view_schema(
    query_schema: &uqa_execution::RowSchema,
    output_columns: &[String],
) -> Result<uqa_execution::RowSchema, SQLError> {
    if query_schema.len() != output_columns.len() {
        return Err(SQLError::Internal(format!(
            "stored view row type has {} columns but its query has {}",
            output_columns.len(),
            query_schema.len()
        )));
    }
    Ok(uqa_execution::RowSchema::with_types(
        output_columns.to_vec(),
        query_schema.column_types().to_vec(),
    ))
}

fn validate_replacement_schema(
    old: &uqa_execution::RowSchema,
    new: &uqa_execution::RowSchema,
) -> Result<(), SQLError> {
    if new.len() < old.len() {
        return Err(SQLError::Routine {
            sqlstate: "42P16".into(),
            message: "cannot drop columns from view".into(),
        });
    }
    for position in 0..old.len() {
        let old_name = old
            .public_name(position)
            .unwrap_or(&old.columns()[position]);
        let new_name = new
            .public_name(position)
            .unwrap_or(&new.columns()[position]);
        if old_name != new_name {
            return Err(SQLError::Routine {
                sqlstate: "42P16".into(),
                message: format!(
                    "cannot change name of view column \"{old_name}\" to \"{new_name}\""
                ),
            });
        }
        if old.column_type(position) != new.column_type(position) {
            return Err(SQLError::Routine {
                sqlstate: "42P16".into(),
                message: format!("cannot change data type of view column \"{old_name}\""),
            });
        }
    }
    Ok(())
}

fn bind_stored_view_relations(
    plan: &mut QueryPlan,
    relations: &std::collections::BTreeSet<RelationIdentity>,
) -> StorageBackendResult<()> {
    bind_query_plan_relations(plan, &std::collections::BTreeSet::new(), &mut |reference| {
        if let Some(canonical) = canonical_virtual_relation_reference(reference) {
            return Ok(canonical);
        }
        let (schema, local_name) =
            RelationIdentity::parse_reference(reference).map_err(|error| {
                StorageBackendError::Other(format!(
                    "invalid stored view source `{reference}`: {error}"
                ))
            })?;
        if let Some(schema) = schema {
            let candidate = RelationIdentity::new(schema, local_name);
            if relations.contains(&candidate) {
                return Ok(candidate.qualified_name());
            }
        } else {
            let candidates = relations
                .iter()
                .filter(|candidate| candidate.name == local_name)
                .map(RelationIdentity::qualified_name)
                .collect::<Vec<_>>();
            match candidates.as_slice() {
                [candidate] => return Ok(candidate.clone()),
                [] => {}
                _ => {
                    return Err(StorageBackendError::Other(format!(
                        "ambiguous stored view source `{reference}` matches {}",
                        candidates.join(", ")
                    )));
                }
            }
        }
        Err(StorageBackendError::Other(format!(
            "stored view source relation `{reference}` does not exist"
        )))
    })
}

impl Engine {
    pub(crate) fn rewrite_view_relation_references(
        &self,
        replacements: &std::collections::BTreeMap<RelationIdentity, RelationIdentity>,
    ) -> StorageBackendResult<()> {
        if replacements.is_empty() {
            return Ok(());
        }
        let mut updates = Vec::new();
        for (view_relation, stored) in self.durable.views.read().iter() {
            let mut candidate = stored.clone();
            let mut changed = false;
            bind_query_plan_relations(
                &mut candidate.query,
                &std::collections::BTreeSet::new(),
                &mut |reference| -> StorageBackendResult<String> {
                    let identity = RelationIdentity::from_legacy_name(reference)
                        .map_err(StorageBackendError::Other)?;
                    if let Some(replacement) = replacements.get(&identity) {
                        changed = true;
                        Ok(replacement.qualified_name())
                    } else {
                        Ok(reference.to_string())
                    }
                },
            )?;
            if changed {
                updates.push((view_relation.clone(), candidate));
            }
        }
        if let Some(catalog) = self.storage.catalog.as_ref() {
            for (relation, view) in &updates {
                catalog.save_view(&catalog_view_row(relation, view)?)?;
            }
        }
        let mut views = self.durable.views.write();
        for (relation, view) in updates {
            views.insert(relation, view);
        }
        Ok(())
    }

    pub(crate) fn drop_views_depending_on_relations(
        &self,
        relations: &[String],
    ) -> StorageBackendResult<()> {
        let mut pending = relations.to_vec();
        let mut views = std::collections::BTreeSet::new();
        while let Some(relation) = pending.pop() {
            for dependent in self.views_depending_on_relation(&relation)? {
                if views.insert(dependent.clone()) {
                    pending.push(dependent);
                }
            }
        }
        let views = views.into_iter().collect::<Vec<_>>();
        self.drop_rules_depending_on_relations_inner(&views)?;
        self.drop_views_inner(&views, false)
            .map_err(|error| StorageBackendError::Other(error.to_string()))
    }

    #[cfg(test)]
    pub(super) fn bind_stored_view_plan(
        &self,
        plan: &mut QueryPlan,
        relations: &std::collections::BTreeSet<RelationIdentity>,
    ) -> StorageBackendResult<()> {
        bind_stored_view_relations(plan, relations)?;
        bind_query_plan_sequence_references(plan, &mut |reference| {
            self.resolve_stored_sequence_reference(reference)
        })
    }

    pub fn drop_view(&self, name: &str) -> Result<bool, SQLError> {
        self.with_implicit_transaction(|engine| {
            match engine.try_resolve_visible_relation_kind(name)? {
                Some((canonical, "view")) => {
                    engine.drop_views_inner(&[canonical], true)?;
                    Ok(true)
                }
                Some((canonical, kind)) => Err(SQLError::Unsupported(format!(
                    "DROP VIEW: relation `{canonical}` is a {kind}, not a view"
                ))),
                None => Ok(false),
            }
        })
    }

    pub(crate) fn drop_views(&self, names: &[String], cascade: bool) -> Result<(), SQLError> {
        self.with_implicit_transaction(|engine| {
            if !cascade {
                return engine.drop_views_inner(names, true);
            }
            engine.ensure_view_drop_authorities(names)?;
            let closure = engine.cascade_view_closure(names.to_vec())?;
            engine
                .drop_rules_depending_on_relations_inner(&closure)
                .map_err(|error| {
                    SQLError::Internal(format!("drop rules depending on cascading views: {error}"))
                })?;
            engine.drop_views_inner(&closure, false)
        })
    }

    fn ensure_view_drop_authorities(&self, names: &[String]) -> Result<(), SQLError> {
        let views = self.durable.views.read();
        for name in names {
            let relation = RelationIdentity::from_legacy_name(name).map_err(|error| {
                SQLError::Internal(format!("resolve DROP VIEW target `{name}`: {error}"))
            })?;
            let view = views.get(&relation).ok_or_else(|| {
                SQLError::Internal(format!("view `{name}` disappeared before owner check"))
            })?;
            self.ensure_view_drop_authority(name, view)?;
        }
        Ok(())
    }

    pub(crate) fn drop_views_inner(
        &self,
        names: &[String],
        check_authority: bool,
    ) -> Result<(), SQLError> {
        let drop_set = names
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        if check_authority {
            self.ensure_view_drop_authorities(names)?;
        }
        let dependent_rules = self
            .rules_depending_on_relations(names)
            .map_err(|error| SQLError::Internal(format!("inspect rule dependencies: {error}")))?;
        if !dependent_rules.is_empty() {
            return Err(SQLError::Routine {
                sqlstate: "2BP01".into(),
                message: format!(
                    "cannot drop view {} because other objects depend on it: {}",
                    names.join(", "),
                    dependent_rules
                        .into_iter()
                        .map(|(table, rule)| format!(
                            "rule {rule} on table {}",
                            table.qualified_name()
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        }
        for name in names {
            let dependents = self
                .views_depending_on_relation(name)
                .map_err(|err| SQLError::Internal(format!("inspect view dependencies: {err}")))?
                .into_iter()
                .filter(|dependent| !drop_set.contains(dependent))
                .collect::<Vec<_>>();
            if !dependents.is_empty() {
                return Err(SQLError::Unsupported(format!(
                    "DROP VIEW `{name}` rejected: dependent view(s) `{}` still reference it",
                    dependents.join("`, `")
                )));
            }
        }
        for name in names {
            self.drop_view_state_inner(name)?;
        }
        Ok(())
    }

    pub(crate) fn drop_temporary_views_depending_on_relation_inner(
        &self,
        canonical_name: &str,
    ) -> StorageBackendResult<()> {
        let target = RelationIdentity::from_legacy_name(canonical_name)
            .map_err(StorageBackendError::Other)?;
        let empty_ctes = std::collections::BTreeSet::new();
        let views = self.durable.views.read();
        let mut targets = std::collections::BTreeSet::from([target]);
        let mut layers = Vec::new();
        loop {
            let layer = views
                .iter()
                .filter(|(relation, _)| !targets.contains(*relation))
                .filter(|(_, view)| {
                    targets.iter().any(|target| {
                        query_plan_references_relation(&view.query, target, &empty_ctes)
                    })
                })
                .map(|(relation, view)| {
                    if view.persistence != uqa_sql::ast::RelationPersistence::Temporary {
                        return Err(StorageBackendError::Other(format!(
                            "temporary relation `{canonical_name}` has non-temporary dependent view `{}`",
                            relation.qualified_name()
                        )));
                    }
                    Ok(relation.clone())
                })
                .collect::<StorageBackendResult<Vec<_>>>()?;
            if layer.is_empty() {
                break;
            }
            targets.extend(layer.iter().cloned());
            layers.push(layer);
        }
        drop(views);

        // PostgreSQL performs internal ON COMMIT deletion with CASCADE. Drop
        // the outermost dependent views first so no temporary view survives
        // with a binding to a relation that disappeared at commit.
        for layer in layers.into_iter().rev() {
            for relation in layer {
                let name = relation.qualified_name();
                self.drop_view_state_inner(&name)
                    .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            }
        }
        Ok(())
    }

    fn drop_view_state_inner(&self, name: &str) -> Result<(), SQLError> {
        let relation = RelationIdentity::from_legacy_name(name)
            .map_err(|err| SQLError::Internal(format!("invalid canonical view name: {err}")))?;
        self.drop_relation_events_inner(&relation)
            .map_err(|error| SQLError::Internal(format!("drop view rules: {error}")))?;
        let mut views = self.durable.views.write();
        let temporary = views
            .get(&relation)
            .is_some_and(|view| view.persistence == uqa_sql::ast::RelationPersistence::Temporary);
        let removed = if temporary {
            views.contains_key(&relation)
        } else if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog
                .drop_view(&relation)
                .map_err(|err| SQLError::Internal(format!("drop view `{name}`: {err}")))?
        } else {
            views.contains_key(&relation)
        };
        if removed {
            views.remove(&relation);
        }
        drop(views);
        if removed {
            self.note_catalog_registry_changed();
        }
        if removed {
            Ok(())
        } else {
            Err(SQLError::Internal(format!(
                "view `{name}` disappeared after dependency preflight"
            )))
        }
    }

    pub(crate) fn stored_view_schema(
        &self,
        view: &StoredView,
    ) -> Result<uqa_execution::RowSchema, SQLError> {
        self.stored_view_schema_with_catalog(
            view,
            self.restored_catalog_read_view(),
            self.session_execution_view().relation_name_resolution(),
        )
    }

    pub(crate) fn stored_view_schema_with_catalog(
        &self,
        view: &StoredView,
        catalog: crate::engine_capabilities::CatalogReadView,
        resolution: crate::engine_capabilities::RelationNameResolution,
    ) -> Result<uqa_execution::RowSchema, SQLError> {
        if view.kind == StoredViewKind::Materialized {
            let output_columns = view.output_columns.clone().unwrap_or_default();
            if output_columns.len() != view.materialized_column_types.len() {
                return Err(SQLError::Internal(
                    "stored materialized view column metadata is inconsistent".into(),
                ));
            }
            return Ok(uqa_execution::RowSchema::with_types(
                output_columns,
                view.materialized_column_types.clone(),
            ));
        }
        let query_schema = crate::sql::analyze_query_schema_with_catalog(
            self,
            &view.query,
            &[],
            catalog,
            resolution,
        )?;
        let output_columns = match &view.output_columns {
            Some(columns) => columns.clone(),
            None => create_view_output_columns(&query_schema, &[])?,
        };
        named_view_schema(&query_schema, &output_columns)
    }

    pub(crate) fn view_schema(
        &self,
        name: &str,
    ) -> Result<Option<uqa_execution::RowSchema>, SQLError> {
        self.view_definition(name)?
            .map(|view| self.stored_view_schema(&view))
            .transpose()
    }

    pub(crate) fn view_definition(&self, name: &str) -> Result<Option<StoredView>, SQLError> {
        let Some(resolved) = self
            .try_resolve_view_name(name)
            .map_err(|err| SQLError::Internal(format!("refresh view catalog: {err}")))?
        else {
            return Ok(None);
        };
        let relation = Self::resolved_relation_identity(&resolved)
            .map_err(|err| SQLError::Internal(format!("resolve view `{resolved}`: {err}")))?;
        if let Some(snapshot) = self.query_view_snapshots.as_ref() {
            return Ok(snapshot.get(&relation).cloned());
        }
        Ok(self.durable.views.read().get(&relation).cloned())
    }

    /// Resolve a view only against the live restored registry without starting another registry synchronization pass.
    pub(crate) fn restored_catalog_view_definition(
        &self,
        name: &str,
    ) -> Result<Option<StoredView>, SQLError> {
        let views = self.durable.views.read();
        Ok(self
            .relation_lookup_candidates(name)
            .map_err(|error| {
                SQLError::Internal(format!("resolve restored view `{name}`: {error}"))
            })?
            .into_iter()
            .find_map(|relation| views.get(&relation).cloned()))
    }

    pub fn view(&self, name: &str) -> Result<Option<uqa_planner::QueryPlan>, SQLError> {
        Ok(self.view_definition(name)?.and_then(|definition| {
            (definition.kind == StoredViewKind::View).then_some(definition.query)
        }))
    }

    pub(crate) fn view_plan(&self, name: &str) -> Result<Option<uqa_planner::QueryPlan>, SQLError> {
        self.view(name)
    }

    pub fn list_views(&self) -> Result<Vec<String>, SQLError> {
        self.synchronize_catalog_registries()
            .map_err(|err| SQLError::Internal(format!("refresh view catalog: {err}")))?;
        let mut out: Vec<String> = self
            .durable
            .views
            .read()
            .iter()
            .filter(|(_, view)| view.kind == StoredViewKind::View)
            .map(|(relation, _)| relation.qualified_name())
            .collect();
        out.sort_unstable();
        Ok(out)
    }

    /// Return stored views whose plan is bound to `canonical_name`.
    ///
    /// New definitions persist canonical source identities. Legacy plans are
    /// canonicalized during restore only when an unqualified name has exactly
    /// one catalog candidate, so normal dependency checks are exact. The
    /// matcher remains conservative for malformed in-memory plans and fails
    /// closed rather than permitting dangling DDL.
    pub(crate) fn views_depending_on_relation(
        &self,
        canonical_name: &str,
    ) -> StorageBackendResult<Vec<String>> {
        self.synchronize_catalog_registries()?;
        let target = RelationIdentity::from_legacy_name(canonical_name)
            .map_err(StorageBackendError::Other)?;
        let empty_ctes = std::collections::BTreeSet::new();
        let mut dependents = self
            .durable
            .views
            .read()
            .iter()
            .filter(|(relation, view)| {
                *relation != &target
                    && query_plan_references_relation(&view.query, &target, &empty_ctes)
            })
            .map(|(relation, _)| relation.qualified_name())
            .collect::<Vec<_>>();
        dependents.sort_unstable();
        Ok(dependents)
    }

    /// Return stored views with a literal `nextval`, `currval`, or `setval`
    /// dependency on the canonical sequence name.
    pub(crate) fn views_depending_on_sequence(
        &self,
        canonical_name: &str,
    ) -> StorageBackendResult<Vec<String>> {
        self.synchronize_catalog_registries()?;
        let target = RelationIdentity::from_legacy_name(canonical_name)
            .map_err(StorageBackendError::Other)?;
        let mut dependents = self
            .durable
            .views
            .read()
            .iter()
            .filter(|(_, view)| query_plan_references_sequence(&view.query, &target))
            .map(|(relation, _)| relation.qualified_name())
            .collect::<Vec<_>>();
        dependents.sort_unstable();
        Ok(dependents)
    }

    pub(crate) fn rewrite_view_sequence_references(
        &self,
        from: &RelationIdentity,
        to: &str,
    ) -> StorageBackendResult<()> {
        self.synchronize_catalog_registries()?;
        let mut rewritten_views = Vec::new();
        for (relation, stored) in self.durable.views.read().iter() {
            let mut rewritten = stored.clone();
            let mut changed = false;
            bind_query_plan_sequence_references(
                &mut rewritten.query,
                &mut |reference| -> StorageBackendResult<String> {
                    let (schema, name) =
                        RelationIdentity::parse_reference(reference).map_err(|error| {
                            StorageBackendError::Other(format!(
                                "invalid stored view sequence reference `{reference}`: {error}"
                            ))
                        })?;
                    let matches = schema.as_deref().map_or(name == from.name, |schema| {
                        schema == from.schema && name == from.name
                    });
                    if matches {
                        changed = true;
                        Ok(to.to_string())
                    } else {
                        Ok(reference.to_string())
                    }
                },
            )?;
            if changed {
                rewritten_views.push((relation.clone(), rewritten));
            }
        }
        if let Some(catalog) = self.storage.catalog.as_ref() {
            for (relation, view) in &rewritten_views {
                if view.persistence != uqa_sql::ast::RelationPersistence::Temporary {
                    catalog.save_view(&catalog_view_row(relation, view)?)?;
                }
            }
        }
        if !rewritten_views.is_empty() {
            self.durable.views.write().extend(rewritten_views);
            self.note_catalog_registry_changed();
        }
        Ok(())
    }

    pub(crate) fn cascade_view_closure(
        &self,
        initial: Vec<String>,
    ) -> Result<Vec<String>, SQLError> {
        let mut views = initial;
        views.sort();
        views.dedup();
        let mut index = 0;
        while index < views.len() {
            let dependents = self
                .views_depending_on_relation(&views[index])
                .map_err(|error| {
                    SQLError::Internal(format!("read cascading view dependencies: {error}"))
                })?;
            for dependent in dependents {
                if !views.contains(&dependent) {
                    views.push(dependent);
                }
            }
            index += 1;
        }
        views.sort();
        Ok(views)
    }

    /// Return stored views whose persisted query plan is bound to one exact
    /// non-builtin function identity. Return type is deliberately excluded:
    /// `PostgreSQL` function identity is its canonical name plus input types.
    pub(crate) fn views_depending_on_function(
        &self,
        canonical_name: &str,
        argument_types: &[String],
    ) -> StorageBackendResult<Vec<String>> {
        self.synchronize_catalog_registries()?;
        let target = FunctionBinding {
            name: canonical_name.to_string(),
            argument_types: argument_types.to_vec(),
            builtin: false,
            dispatch: None,
            invocation: None,
            resolution_error: None,
        };
        let mut dependents = self
            .durable
            .views
            .read()
            .iter()
            .filter(|(_, view)| {
                super::view_binding::query_plan_references_function(&view.query, &target)
            })
            .map(|(relation, _)| relation.qualified_name())
            .collect::<Vec<_>>();
        dependents.sort_unstable();
        Ok(dependents)
    }

    fn migrate_legacy_views(
        &self,
        catalog: &dyn CatalogFacade,
        views: &mut BTreeMap<RelationIdentity, StoredView>,
        legacy_views: &[RelationIdentity],
        missing_output_columns: &[RelationIdentity],
    ) -> StorageBackendResult<()> {
        if legacy_views.is_empty() && missing_output_columns.is_empty() {
            return Ok(());
        }
        // Install the complete provisional registry so nested legacy views
        // can derive each other's schemas while exact routine identities are
        // bound and persisted in the current format.
        let previous_views = {
            let mut loaded = self.durable.views.write();
            std::mem::replace(&mut *loaded, views.clone())
        };
        let migration = (|| -> StorageBackendResult<()> {
            for relation in legacy_views {
                let view_name = relation.qualified_name();
                let view = views.get_mut(relation).ok_or_else(|| {
                    StorageBackendError::Other(format!(
                        "legacy view `{view_name}` disappeared during restoration"
                    ))
                })?;
                crate::sql::bind_catalog_query_routines(self, &mut view.query, &[]).map_err(
                    |error| {
                        StorageBackendError::Other(format!(
                            "restore view `{view_name}` routine bindings: {error}"
                        ))
                    },
                )?;
                self.durable
                    .views
                    .write()
                    .insert(relation.clone(), view.clone());
            }
            for relation in missing_output_columns {
                let view_name = relation.qualified_name();
                let output_columns = {
                    let view = views.get(relation).ok_or_else(|| {
                        StorageBackendError::Other(format!(
                            "legacy view `{view_name}` disappeared while restoring column metadata"
                        ))
                    })?;
                    let schema = self.stored_view_schema(view).map_err(|error| {
                        StorageBackendError::Other(format!(
                            "restore legacy view `{view_name}` column metadata: {error}"
                        ))
                    })?;
                    schema
                        .columns()
                        .iter()
                        .enumerate()
                        .map(|(position, column)| {
                            schema.public_name(position).unwrap_or(column).to_string()
                        })
                        .collect::<Vec<_>>()
                };
                let view = views.get_mut(relation).ok_or_else(|| {
                    StorageBackendError::Other(format!(
                        "legacy view `{view_name}` disappeared while installing column metadata"
                    ))
                })?;
                view.output_columns = Some(output_columns);
                self.durable
                    .views
                    .write()
                    .insert(relation.clone(), view.clone());
            }
            let migrated_views = legacy_views
                .iter()
                .chain(missing_output_columns)
                .cloned()
                .collect::<std::collections::BTreeSet<_>>();
            for relation in migrated_views {
                let view_name = relation.qualified_name();
                let view = views.get(&relation).ok_or_else(|| {
                    StorageBackendError::Other(format!(
                        "legacy view `{view_name}` disappeared during migration"
                    ))
                })?;
                catalog
                    .save_view(&catalog_view_row(&relation, view)?)
                    .map_err(|error| {
                        StorageBackendError::Other(format!(
                            "migrate legacy view `{view_name}` routine bindings: {error}"
                        ))
                    })?;
            }
            Ok(())
        })();
        if let Err(error) = migration {
            *self.durable.views.write() = previous_views;
            return Err(error);
        }
        Ok(())
    }

    fn validate_and_persist_restored_views(
        &self,
        catalog: &dyn CatalogFacade,
        views: &BTreeMap<RelationIdentity, StoredView>,
        legacy_views: &[RelationIdentity],
        dispatch_upgraded_views: &[RelationIdentity],
    ) -> StorageBackendResult<()> {
        for (relation, view) in views {
            let output_columns = view.output_columns.as_deref().ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "view `{}` has no public column metadata after catalog migration",
                    relation.qualified_name()
                ))
            })?;
            crate::engine_table_security::validate_table_security_invariants(
                &view.security(),
                Some(output_columns),
                &self.durable.roles.read(),
            )
            .map_err(|error| {
                StorageBackendError::Other(format!(
                    "view `{}` has invalid privilege metadata after migration: {error}",
                    relation.qualified_name()
                ))
            })?;
        }
        for relation in dispatch_upgraded_views {
            if legacy_views.contains(relation) {
                continue;
            }
            let view = views.get(relation).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "dispatch-upgraded view `{}` disappeared during restoration",
                    relation.qualified_name()
                ))
            })?;
            catalog.save_view(&catalog_view_row(relation, view)?)?;
        }
        Ok(())
    }

    fn temporary_stored_views(&self) -> BTreeMap<RelationIdentity, StoredView> {
        self.durable
            .views
            .read()
            .iter()
            .filter(|(_, view)| view.persistence == uqa_sql::ast::RelationPersistence::Temporary)
            .map(|(relation, view)| (relation.clone(), view.clone()))
            .collect()
    }

    pub(crate) fn restore_views_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        let rows = catalog.load_views()?;
        let temporary_views = self.temporary_stored_views();
        let mut relations = self
            .storage
            .tables
            .read()
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        relations.extend(self.durable.foreign_tables.read().keys().cloned());
        relations.extend(rows.iter().map(|row| row.relation.clone()));
        for (graph, store) in self.durable.graphs.read().iter() {
            let labels = store.graph_labels(graph).map_err(|error| {
                StorageBackendError::Other(format!(
                    "restore graph-label view dependencies for `{graph}`: {error}"
                ))
            })?;
            relations.extend(
                labels
                    .into_iter()
                    .map(|label| RelationIdentity::new(graph, label.name)),
            );
        }
        relations.extend(temporary_views.keys().cloned());

        let mut views = BTreeMap::new();
        let mut legacy_views = Vec::new();
        let mut missing_output_columns = Vec::new();
        let mut dispatch_upgraded_views = Vec::new();
        for row in rows {
            let view_name = row.relation.qualified_name();
            let mut view = match serde_json::from_str::<RestoredView>(&row.definition_json)? {
                RestoredView::Current(view) => view,
                RestoredView::Legacy(query) => {
                    legacy_views.push(row.relation.clone());
                    StoredView {
                        role_owner: row.role_owner.clone(),
                        acl: row.acl.clone(),
                        column_acls: row.column_acls.clone(),
                        query,
                        output_columns: None,
                        persistence: uqa_sql::ast::RelationPersistence::Permanent,
                        options: Vec::new(),
                        kind: StoredViewKind::View,
                        materialized_rows: Vec::new(),
                        materialized_column_types: Vec::new(),
                        populated: true,
                    }
                }
            };
            view.role_owner = row.role_owner;
            view.acl = row.acl;
            view.column_acls = row.column_acls;
            if view.kind == StoredViewKind::View && view.output_columns.is_none() {
                missing_output_columns.push(row.relation.clone());
            }
            if !self.durable.roles.read().contains_key(&view.role_owner) {
                return Err(StorageBackendError::Other(format!(
                    "view `{view_name}` is owned by missing role `{}`",
                    view.role_owner
                )));
            }
            crate::engine_table_security::validate_table_security_invariants(
                &view.security(),
                view.output_columns.as_deref(),
                &self.durable.roles.read(),
            )
            .map_err(|error| {
                StorageBackendError::Other(format!(
                    "view `{view_name}` has invalid privilege metadata: {error}"
                ))
            })?;
            if upgrade_legacy_view_dispatches(&mut view.query) {
                dispatch_upgraded_views.push(row.relation.clone());
            }
            bind_stored_view_relations(&mut view.query, &relations).map_err(|error| {
                StorageBackendError::Other(format!("restore view `{view_name}`: {error}"))
            })?;
            bind_query_plan_sequence_references(&mut view.query, &mut |reference| {
                self.resolve_stored_sequence_reference_from_loaded_registry(reference)
            })
            .map_err(|error| {
                StorageBackendError::Other(format!("restore view `{view_name}`: {error}"))
            })?;
            views.insert(row.relation, view);
        }
        views.extend(temporary_views);
        self.migrate_legacy_views(catalog, &mut views, &legacy_views, &missing_output_columns)?;
        self.validate_and_persist_restored_views(
            catalog,
            &views,
            &legacy_views,
            &dispatch_upgraded_views,
        )?;
        *self.durable.views.write() = views;
        Ok(())
    }
}
