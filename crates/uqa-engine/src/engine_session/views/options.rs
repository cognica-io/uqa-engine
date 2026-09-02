//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable regular-view and materialized-view lifecycle changes.

use super::{catalog_view_row, Engine, RelationIdentity, SQLError};

impl Engine {
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
            let Some((canonical, actual_kind)) =
                engine.try_resolve_visible_relation_kind(&statement.name)?
            else {
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
