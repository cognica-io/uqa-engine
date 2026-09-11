//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute sequence removal through native dependency owners and retained catalog publication.
use super::{
    dependency_lifecycle::SequenceDependencyContext, owner_publication::SequenceOwnerNames,
};
use crate::{
    routines::removal::context::RoutineRemovalContext,
    schema::{events::context::EventLifecycleContext, view_removal::context::ViewRemovalContext},
};
use uqa_core::RelationIdentity;
use uqa_sql::{
    catalog::security::sequence_inquiry::SequencePrivilegeInquiry,
    schema::sequences::dependents::SequenceSchemaDependent, SQLError,
};
use uqa_storage::{SequenceOwnerDependency, StorageBackendError, StorageBackendResult};
pub trait SequenceRemovalPublication {
    fn remove_state(&self, name: &str) -> Result<bool, String>;
}
/// Borrow native inputs lazily so recursive column/sequence cascades do not construct recursive context values.
pub trait SequenceRemovalInputs {
    fn sequence_removal_context(&self) -> SequenceRemovalContext<'_>;
}
pub struct SequenceRemovalContext<'a> {
    pub names: &'a dyn SequenceOwnerNames,
    pub publication: &'a dyn SequenceRemovalPublication,
    pub privileges: SequencePrivilegeInquiry<'a>,
    pub dependencies: SequenceDependencyContext<'a>,
    pub routines: RoutineRemovalContext<'a>,
    pub views: ViewRemovalContext<'a>,
    pub events: EventLifecycleContext<'a>,
}
struct SequenceDropDependents {
    schema: Vec<SequenceSchemaDependent>,
    views: Vec<String>,
    rules: Vec<(RelationIdentity, String)>,
}

impl SequenceDropDependents {
    fn is_empty(&self) -> bool {
        self.schema.is_empty() && self.views.is_empty() && self.rules.is_empty()
    }
}

impl SequenceRemovalContext<'_> {
    pub fn drop_sequence(&self, name: &str) -> Result<bool, String> {
        let Some(name) = self
            .names
            .resolve_sequence_name(name)
            .map_err(|err| format!("load sequence catalog: {err}"))?
        else {
            return Ok(false);
        };
        self.drop_sequences(std::slice::from_ref(&name), false)
            .map_err(|error| error.to_string())?;
        Ok(true)
    }
    pub fn drop_sequences(&self, names: &[String], cascade: bool) -> Result<(), SQLError> {
        self.drop_sequences_with_owner(names, cascade, false)
    }
    fn sequence_drop_dependents(&self, name: &str) -> Result<SequenceDropDependents, SQLError> {
        let schema = self
            .dependencies
            .sequence_schema_expression_dependents(name)
            .map_err(|error| {
                SQLError::Internal(format!(
                    "inspect column dependencies for sequence `{name}`: {error}"
                ))
            })?;
        let views = crate::schema::view_dependencies::views_depending_on_sequence(
            &self.views.dependencies,
            name,
        )
        .map_err(|error| {
            SQLError::Internal(format!(
                "inspect view dependencies for sequence `{name}`: {error}"
            ))
        })?;
        let rules = self
            .events
            .lookup
            .rules_depending_on_relations(&[name.to_string()])
            .map_err(uqa_storage::StorageBackendError::Other)
            .map_err(|error| {
                SQLError::Internal(format!(
                    "inspect rule dependencies for sequence `{name}`: {error}"
                ))
            })?;
        Ok(SequenceDropDependents {
            schema,
            views,
            rules,
        })
    }
    fn drop_rules_for_sequence_cascade(
        &self,
        names: &[String],
        cascade_views: &[String],
    ) -> Result<(), SQLError> {
        self.events
            .drop_rules_depending_on_relations_inner(names)
            .map_err(|error| {
                SQLError::Internal(format!("drop rules depending on sequence: {error}"))
            })?;
        if !cascade_views.is_empty() {
            self.events
                .drop_rules_depending_on_relations_inner(cascade_views)
                .map_err(|error| {
                    SQLError::Internal(format!("drop rules depending on cascading views: {error}"))
                })?;
        }
        Ok(())
    }
    fn ensure_sequence_drop_owners(
        &self,
        names: &[String],
        owner_initiated: bool,
    ) -> Result<(), SQLError> {
        for name in names {
            if !owner_initiated {
                let relation = RelationIdentity::from_legacy_name(name)
                    .map_err(StorageBackendError::Other)
                    .map_err(|error| {
                        SQLError::Internal(format!("resolve sequence `{name}`: {error}"))
                    })?;
                self.privileges.ensure_sequence_owner(name, &relation)?;
                let owner = self
                    .dependencies
                    .sequences
                    .states()
                    .get(&relation)
                    .and_then(|state| state.owner)
                    .filter(|owner| owner.dependency == SequenceOwnerDependency::Internal);
                if let Some(owner) = owner {
                    let (table, column, foreign) = self
                        .dependencies
                        .sequence_owner_target(owner)
                        .ok_or_else(|| {
                        SQLError::Internal(format!(
                            "identity sequence `{name}` has a dangling owner dependency"
                        ))
                    })?;
                    let relation_kind = if foreign { "foreign table" } else { "table" };
                    return Err(SQLError::Routine {
                        sqlstate: "2BP01".into(),
                        message: format!(
                            "cannot drop sequence {name} because column {column} of {relation_kind} {table} requires it"
                        ),
                    });
                }
            }
        }
        Ok(())
    }

    fn drop_sequences_with_owner(
        &self,
        names: &[String],
        cascade: bool,
        owner_initiated: bool,
    ) -> Result<(), SQLError> {
        let mut cascade_schema = Vec::new();
        let mut direct_views = Vec::new();
        self.ensure_sequence_drop_owners(names, owner_initiated)?;
        crate::routines::removal::drop_relation_routine_dependents(
            &self.routines,
            names,
            cascade,
            "sequence",
        )?;
        for name in names {
            let dependents = self.sequence_drop_dependents(name)?;
            if !cascade && !dependents.is_empty() {
                return Err(crate::routines::removal::relation_dependents_drop_error(
                    &self.routines,
                    names,
                    "sequence",
                )?);
            }
            cascade_schema.extend(dependents.schema);
            direct_views.extend(dependents.views);
        }
        cascade_schema.sort();
        cascade_schema.dedup();
        let columns = cascade_schema
            .iter()
            .filter_map(|dependent| {
                if let SequenceSchemaDependent::GeneratedColumn { table, column, .. } = dependent {
                    Some((table.clone(), column.clone()))
                } else {
                    None
                }
            })
            .collect();
        let rewritten = crate::routines::removal::prepare_routine_column_alias_drop(
            &self.routines,
            columns,
            &[],
        )?;
        let cascade_views = crate::schema::view_dependencies::cascade_view_closure(
            &self.views.dependencies,
            direct_views,
        )?;
        if cascade {
            self.drop_rules_for_sequence_cascade(names, &cascade_views)?;
        }
        if cascade && !cascade_views.is_empty() {
            crate::schema::view_removal::drop_views_inner(&self.views, &cascade_views, false)?;
        }
        for name in names {
            self.dependencies
                .detach_sequence_column_dependencies(name, cascade)
                .map_err(|error| {
                    SQLError::Internal(format!(
                        "detach column dependencies for sequence `{name}`: {error}"
                    ))
                })?;
            if !self
                .publication
                .remove_state(name)
                .map_err(SQLError::Internal)?
            {
                return Err(SQLError::Internal(format!(
                    "resolved sequence `{name}` disappeared before DROP"
                )));
            }
        }
        crate::routines::rewrites::publish_stored_routine_body_rewrites(
            &self.routines.bodies,
            rewritten,
        )?;
        if cascade {
            let mut dependents = cascade_schema
                .iter()
                .map(SequenceSchemaDependent::object_label)
                .collect::<Vec<_>>();
            dependents.extend(cascade_views.iter().map(|view| format!("view {view}")));
            match dependents.as_slice() {
                [] => {}
                [dependent] => {
                    self.routines
                        .notices
                        .routine_drop_notice("NOTICE", &format!("drop cascades to {dependent}"));
                }
                _ => self.routines.notices.routine_drop_notice(
                    "NOTICE",
                    &format!("drop cascades to {} other objects", dependents.len()),
                ),
            }
        }
        Ok(())
    }
    pub fn drop_owned_sequence(&self, name: &str, cascade: bool) -> StorageBackendResult<()> {
        let canonical = self.names.resolve_sequence_name(name)?.ok_or_else(|| {
            StorageBackendError::Other(format!("owned sequence `{name}` does not exist"))
        })?;
        self.drop_sequences_with_owner(std::slice::from_ref(&canonical), cascade, true)
            .map_err(|error| StorageBackendError::Other(error.to_string()))
    }
}
