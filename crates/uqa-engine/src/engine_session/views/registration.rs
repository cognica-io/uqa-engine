//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Regular-view creation, source binding, and replacement validation.

use super::{
    bind_query_plan_relations, bind_query_plan_sequence_references,
    canonical_virtual_relation_reference, catalog_view_row, create_view_output_columns,
    named_view_schema, validate_replacement_schema, Engine, QueryPlan, RelationIdentity, SQLError,
    StoredView, StoredViewKind, ViewRegistration,
};

fn resolve_loaded_sequence_reference_for_query_binding(
    engine: &Engine,
    reference: &str,
) -> crate::StorageBackendResult<String> {
    let sequences = engine.durable.sequences.read();
    engine
        .relation_lookup_candidates(reference)?
        .into_iter()
        .find(|candidate| sequences.contains_key(candidate))
        .map(|candidate| candidate.qualified_name())
        .ok_or_else(|| {
            crate::StorageBackendError::Other(format!("Sequence `{reference}` does not exist"))
        })
}

impl Engine {
    pub(super) fn bind_view_plan_for_create(&self, plan: &mut QueryPlan) -> Result<bool, SQLError> {
        self.bind_stored_query_relations(plan, "CREATE VIEW", true)
    }

    /// Bind the relation identities owned by a stored SQL query. The resulting plan no longer participates in the executing session's relation namespace.
    pub(crate) fn bind_stored_query_relations(
        &self,
        plan: &mut QueryPlan,
        context: &str,
        reject_transition_relations: bool,
    ) -> Result<bool, SQLError> {
        self.bind_stored_query_relations_with_loaded_catalog(
            plan,
            context,
            reject_transition_relations,
            false,
        )
    }

    /// Bind a stored query while catalog restoration already owns the synchronization boundary.
    pub(crate) fn bind_loaded_stored_query_relations(
        &self,
        plan: &mut QueryPlan,
        context: &str,
        reject_transition_relations: bool,
    ) -> Result<bool, SQLError> {
        self.bind_stored_query_relations_with_loaded_catalog(
            plan,
            context,
            reject_transition_relations,
            true,
        )
    }

    fn bind_stored_query_relations_with_loaded_catalog(
        &self,
        plan: &mut QueryPlan,
        context: &str,
        reject_transition_relations: bool,
        loaded_catalog: bool,
    ) -> Result<bool, SQLError> {
        let temporary_schema = self.temporary_schema_name();
        let transition_relations = crate::sql::active_trigger_transition_relation_names();
        let mut uses_temporary_relation = false;
        bind_query_plan_relations(plan, &std::collections::BTreeSet::new(), &mut |reference| {
            // Catalog relations win for their supported spellings just
            // as they do in FROM execution (notably unqualified
            // `pg_class`). Explicit user schemas remain ordinary catalog
            // identities.
            if let Some(canonical) = canonical_virtual_relation_reference(reference) {
                return Ok(canonical);
            }
            if let Some(canonical) = crate::sql::resolve_age_label_relation_name(self, reference)? {
                return Ok(canonical);
            }
            if RelationIdentity::parse_reference(reference)
                .ok()
                .is_some_and(|(schema, relation)| {
                    schema.is_none() && transition_relations.contains(&relation)
                })
            {
                if reject_transition_relations {
                    return Err(SQLError::Routine {
                        sqlstate: "0A000".into(),
                        message: "transition tables cannot be referenced in a view definition"
                            .into(),
                    });
                }
                return Ok(reference.to_string());
            }
            let resolved = if loaded_catalog {
                self.resolve_loaded_visible_relation_kind(reference)?
                    .into_found()
            } else {
                self.try_resolve_visible_relation_kind(reference)?
            };
            match resolved {
                Some((canonical, "table" | "view" | "materialized view" | "foreign table")) => {
                    uses_temporary_relation |= RelationIdentity::from_legacy_name(&canonical)
                        .is_ok_and(|relation| relation.schema == temporary_schema);
                    Ok(canonical)
                }
                Some((canonical, kind)) => Err(SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: format!(
                        "{context} source \"{canonical}\" is a {kind}, not a row relation"
                    ),
                }),
                None => Err(SQLError::UnknownTable(reference.to_string())),
            }
        })?;
        bind_query_plan_sequence_references(plan, &mut |reference| {
            let resolved = if loaded_catalog {
                resolve_loaded_sequence_reference_for_query_binding(self, reference)
            } else {
                self.resolve_sequence_reference_for_binding(reference)
            };
            resolved.map_err(|error| {
                SQLError::Unsupported(format!(
                    "{context} sequence reference `{reference}`: {error}"
                ))
            })
        })?;
        Ok(uses_temporary_relation)
    }

    pub fn register_view(
        &self,
        name: &str,
        body: uqa_sql::ast::SelectStmt,
    ) -> Result<(), SQLError> {
        self.with_implicit_transaction(move |engine| {
            let plan = uqa_planner::UnifiedPlan::Query(Box::new(
                uqa_planner::QueryPlan::lower_with(body, &|aggregate: &str| {
                    engine.has_registered_aggregate_function(aggregate)
                }),
            ));
            let plan = crate::sql::optimize_engine_plan(engine, plan)?;
            let uqa_planner::UnifiedPlan::Query(plan) = plan else {
                return Err(SQLError::Internal(
                    "view lowering produced a non-query plan".into(),
                ));
            };
            engine.register_view_plan_inner(ViewRegistration {
                name,
                column_names: &[],
                plan: *plan,
                or_replace: true,
                persistence: uqa_sql::ast::RelationPersistence::Permanent,
                options: &[],
                params: &[],
            })
        })
    }

    pub(crate) fn register_view_plan(
        &self,
        registration: ViewRegistration<'_>,
    ) -> Result<(), SQLError> {
        self.with_implicit_transaction(move |engine| engine.register_view_plan_inner(registration))
    }

    fn replacement_view(
        &self,
        name: &str,
        relation: &RelationIdentity,
        or_replace: bool,
        replacement_schema: &uqa_execution::RowSchema,
    ) -> Result<Option<StoredView>, SQLError> {
        match self
            .relation_kind_at(name)
            .map_err(|error| SQLError::Internal(format!("resolve relation `{name}`: {error}")))?
        {
            Some(_) if !or_replace => Err(SQLError::Routine {
                sqlstate: "42P07".into(),
                message: format!("relation \"{name}\" already exists"),
            }),
            Some("view") => {
                let existing = self
                    .durable
                    .views
                    .read()
                    .get(relation)
                    .cloned()
                    .ok_or_else(|| {
                        SQLError::Internal(format!(
                            "view `{name}` exists in the catalog but has no loaded definition"
                        ))
                    })?;
                let existing_schema = self.stored_view_schema(&existing)?;
                validate_replacement_schema(&existing_schema, replacement_schema)?;
                self.ensure_view_owner(name, &existing)?;
                Ok(Some(existing))
            }
            Some(kind) => Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("\"{name}\" is not a view; it is a {kind}"),
            }),
            None => Ok(None),
        }
    }

    fn register_view_plan_inner(&self, registration: ViewRegistration<'_>) -> Result<(), SQLError> {
        let ViewRegistration {
            name,
            column_names,
            mut plan,
            or_replace,
            persistence,
            options,
            params,
        } = registration;
        self.synchronize_catalog_registries()
            .map_err(|err| SQLError::Internal(format!("refresh view catalog: {err}")))?;
        let uses_temporary_relation = self.bind_view_plan_for_create(&mut plan)?;
        let persistence = if uses_temporary_relation {
            uqa_sql::ast::RelationPersistence::Temporary
        } else {
            persistence
        };
        let name = if persistence == uqa_sql::ast::RelationPersistence::Temporary {
            let (schema, _) =
                RelationIdentity::parse_reference(name).map_err(SQLError::Unsupported)?;
            if uses_temporary_relation
                && schema.as_deref().is_some_and(|schema| {
                    schema != "pg_temp" && schema != self.temporary_schema_name()
                })
            {
                self.try_relation_name_for_sql_create(name)?;
            }
            self.try_temporary_relation_name_for_create(name)?
        } else {
            self.try_relation_name_for_sql_create(name)?
        };
        let relation = RelationIdentity::from_legacy_name(&name)
            .map_err(|err| SQLError::Internal(format!("invalid canonical view name: {err}")))?;
        let query_schema = crate::sql::bind_catalog_query_routines(self, &mut plan, params)?;
        crate::sql::reject_stored_query_regrole_constants(self, &mut plan)?;
        let output_columns = create_view_output_columns(&query_schema, column_names)?;
        for (position, column) in output_columns.iter().enumerate() {
            if let Some(ty) = query_schema.column_type(position) {
                crate::sql::validate_postgres_relation_column_type(column, ty)?;
            }
        }
        let replacement_schema = named_view_schema(&query_schema, &output_columns)?;
        let existing_view =
            self.replacement_view(&name, &relation, or_replace, &replacement_schema)?;
        let object_id = if let Some(existing) = existing_view.as_ref() {
            existing.object_id
        } else {
            crate::new_view_object_id().map_err(|error| {
                SQLError::Internal(format!("allocate view `{name}` identity: {error}"))
            })?
        };
        let view = StoredView {
            object_id,
            role_owner: existing_view
                .as_ref()
                .map_or_else(|| self.current_user_name(), |view| view.role_owner.clone()),
            acl: existing_view.as_ref().and_then(|view| view.acl.clone()),
            column_acls: existing_view
                .as_ref()
                .map_or_else(std::collections::BTreeMap::new, |view| {
                    view.column_acls.clone()
                }),
            query: plan,
            output_columns: Some(output_columns),
            persistence,
            options: options.to_vec(),
            kind: StoredViewKind::View,
            materialized_rows: Vec::new(),
            materialized_column_types: Vec::new(),
            populated: true,
        };
        crate::sql::validate_stored_view_check_option(self, &name, &view)?;
        let mut views = self.durable.views.write();
        if persistence != uqa_sql::ast::RelationPersistence::Temporary {
            if let Some(catalog) = self.storage.catalog.as_ref() {
                catalog
                    .save_view(&catalog_view_row(&relation, &view).map_err(|err| {
                        SQLError::Internal(format!("serialize view `{name}`: {err}"))
                    })?)
                    .map_err(|err| SQLError::Internal(format!("persist view `{name}`: {err}")))?;
            }
        }
        views.insert(relation, view);
        drop(views);
        self.note_catalog_registry_changed();
        Ok(())
    }
}
