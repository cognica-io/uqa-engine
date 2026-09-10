//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable regular-view and materialized-view lifecycle changes.

use super::{catalog_view_row, Engine, RelationIdentity, SQLError};

impl Engine {
    fn rename_view_inner(
        &self,
        relation: &RelationIdentity,
        new_name: &str,
        expected_kind: &str,
    ) -> Result<(), SQLError> {
        let target = self.relation_rename_target(relation, new_name, "ALTER VIEW RENAME TO")?;
        self.rewrite_relation_rename_dependents(relation, &target)
            .map_err(|error| {
                SQLError::Internal(format!(
                    "rewrite dependencies while renaming {expected_kind} `{}`: {error}",
                    relation.qualified_name()
                ))
            })?;
        let persistent =
            self.durable.views.read().get(relation).is_some_and(|view| {
                view.persistence != uqa_sql::ast::RelationPersistence::Temporary
            });
        if persistent {
            if let Some(catalog) = self.storage.catalog.as_ref() {
                if !catalog.rename_view(relation, &target).map_err(|error| {
                    SQLError::Internal(format!(
                        "persist {expected_kind} rename `{}` to `{}`: {error}",
                        relation.qualified_name(),
                        target.qualified_name()
                    ))
                })? {
                    return Err(SQLError::Internal(format!(
                        "{expected_kind} `{}` disappeared during rename",
                        relation.qualified_name()
                    )));
                }
            }
        }
        let mut views = self.durable.views.write();
        if views.contains_key(&target) {
            return Err(SQLError::Internal(format!(
                "{expected_kind} rename target `{}` appeared after preflight",
                target.qualified_name()
            )));
        }
        let view = views.remove(relation).ok_or_else(|| {
            SQLError::Internal(format!(
                "{expected_kind} `{}` disappeared during rename",
                relation.qualified_name()
            ))
        })?;
        views.insert(target, view);
        drop(views);
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn alter_view(
        &self,
        statement: &uqa_sql::ast::AlterViewStmt,
    ) -> Result<(), SQLError> {
        use uqa_sql::ast::{AlterViewAction, AlterViewKind};

        self.with_implicit_transaction(move |engine| {
            let expected_kind = match statement.kind {
                AlterViewKind::View => "view",
                AlterViewKind::MaterializedView => "materialized view",
            };
            let resolution = if matches!(statement.action, AlterViewAction::RenameTo(_)) {
                engine.resolve_relation_rename_source(&statement.name, statement.if_exists)?
            } else {
                engine.try_resolve_visible_relation_kind(&statement.name)?
            };
            let Some((canonical, actual_kind)) = resolution else {
                if statement.if_exists {
                    return Ok(());
                }
                return Err(SQLError::Routine {
                    sqlstate: "42P01".into(),
                    message: format!("relation \"{}\" does not exist", statement.name),
                });
            };
            if actual_kind != expected_kind {
                return Err(SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: format!("\"{}\" is not a {expected_kind}", statement.name),
                });
            }
            let relation = RelationIdentity::from_legacy_name(&canonical).map_err(|error| {
                SQLError::Internal(format!("invalid ALTER VIEW target `{canonical}`: {error}"))
            })?;
            let mut view = engine
                .durable
                .views
                .read()
                .get(&relation)
                .cloned()
                .ok_or_else(|| {
                    SQLError::Internal(format!("{expected_kind} `{canonical}` disappeared"))
                })?;
            engine.ensure_view_owner(&canonical, &view)?;
            engine.lock_relation(
                &canonical,
                crate::row_locks::RelationLockMode::AccessExclusive,
            )?;
            match &statement.action {
                AlterViewAction::Set(changes) => {
                    for (name, value) in changes {
                        view.options.retain(|(current, _)| current != name);
                        view.options.push((name.clone(), value.clone()));
                    }
                }
                AlterViewAction::Reset(names) => {
                    view.options.retain(|(current, _)| !names.contains(current));
                }
                AlterViewAction::OwnerTo(owner) => {
                    engine.alter_view_role_owner(&canonical, &mut view, owner)?;
                }
                AlterViewAction::RenameTo(new_name) => {
                    return engine.rename_view_inner(&relation, new_name, expected_kind);
                }
            }
            if statement.kind == AlterViewKind::View {
                crate::sql::validate_stored_view_check_option(engine, &canonical, &view)?;
            }
            if view.persistence != uqa_sql::ast::RelationPersistence::Temporary {
                if let Some(catalog) = engine.storage.catalog.as_ref() {
                    catalog
                        .save_view(&catalog_view_row(&relation, &view).map_err(|error| {
                            SQLError::Internal(format!(
                                "serialize altered {expected_kind} `{canonical}`: {error}"
                            ))
                        })?)
                        .map_err(|error| {
                            SQLError::Internal(format!(
                                "persist altered {expected_kind} `{canonical}`: {error}"
                            ))
                        })?;
                }
            }
            engine.durable.views.write().insert(relation, view);
            engine.note_catalog_registry_changed();
            Ok(())
        })
    }
}
