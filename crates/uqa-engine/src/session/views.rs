//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable view registration, binding, dependencies, and restoration.

mod columns;
mod restoration;

use super::{
    bind_query_plan_relations, canonical_virtual_relation_reference,
    query_plan_references_relation, Engine, QueryPlan, RelationIdentity, SQLError,
    StorageBackendError, StorageBackendResult, StoredView, StoredViewKind,
};
use uqa_sql::ast::FunctionBinding;

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum RestoredView {
    Current(StoredView),
    Legacy(QueryPlan),
}

pub(crate) use uqa_execution::catalog::view::catalog_view_row;

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
        self.drop_relation_routine_dependents(relations, true, "relation")
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
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
        let mut refreshed = false;
        uqa_sql::binding::view_dependencies::bind_query_plan_sequence_references(
            plan,
            &mut |reference| {
                if !refreshed {
                    self.refresh_sequences_from_catalog()?;
                    refreshed = true;
                }
                self.resolve_stored_sequence_reference_from_loaded_registry(reference)
            },
        )
    }

    pub fn drop_view(&self, name: &str) -> Result<bool, SQLError> {
        self.with_implicit_transaction(|engine| {
            match engine.try_resolve_visible_relation_kind(name)? {
                Some((canonical, "view")) => {
                    engine.drop_views(&[canonical], false, "view")?;
                    Ok(true)
                }
                Some((canonical, kind)) => Err(SQLError::Unsupported(format!(
                    "DROP VIEW: relation `{canonical}` is a {kind}, not a view"
                ))),
                None => Ok(false),
            }
        })
    }

    pub(crate) fn drop_views(
        &self,
        names: &[String],
        cascade: bool,
        kind: &str,
    ) -> Result<(), SQLError> {
        self.with_implicit_transaction(|engine| {
            engine.ensure_view_drop_authorities(names)?;
            engine.drop_relation_routine_dependents(names, cascade, kind)?;
            if !cascade {
                return engine.drop_views_inner(names, false);
            }
            let remaining = engine.remaining_view_drop_targets(names)?;
            let closure = engine.cascade_view_closure(remaining)?;
            engine
                .drop_rules_depending_on_relations_inner(&closure)
                .map_err(|error| {
                    SQLError::Internal(format!("drop rules depending on cascading views: {error}"))
                })?;
            engine.drop_views_inner(&closure, false)
        })
    }

    pub(crate) fn remaining_view_drop_targets(
        &self,
        names: &[String],
    ) -> Result<Vec<String>, SQLError> {
        let remaining = names
            .iter()
            .filter_map(|name| match RelationIdentity::from_legacy_name(name) {
                Ok(identity) => self
                    .durable
                    .views
                    .read()
                    .contains_key(&identity)
                    .then(|| Ok(name.clone())),
                Err(error) => Some(Err(SQLError::Internal(error))),
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(remaining)
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
        catalog: crate::capabilities::CatalogReadView,
        resolution: crate::capabilities::RelationNameResolution,
    ) -> Result<uqa_execution::RowSchema, SQLError> {
        view.row_schema(self, std::sync::Arc::new(catalog), resolution)
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
}
