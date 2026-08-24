//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable regular-view and materialized-view reloption changes.

use super::{Engine, RelationIdentity, SQLError, ViewRow};

impl Engine {
    pub(crate) fn alter_view_options(
        &self,
        statement: &uqa_sql::ast::AlterViewOptionsStmt,
    ) -> Result<(), SQLError> {
        use uqa_sql::ast::{AlterViewKind, AlterViewOptionsAction};

        self.with_implicit_transaction(move |engine| {
            let expected_kind = match statement.kind {
                AlterViewKind::View => "view",
                AlterViewKind::MaterializedView => "materialized view",
            };
            let Some((canonical, actual_kind)) = engine
                .try_resolve_relation_kind(&statement.name)
                .map_err(|error| {
                    SQLError::Internal(format!(
                        "resolve ALTER {} target `{}`: {error}",
                        expected_kind.to_ascii_uppercase(),
                        statement.name
                    ))
                })?
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
            match &statement.action {
                AlterViewOptionsAction::Set(changes) => {
                    for (name, value) in changes {
                        view.options.retain(|(current, _)| current != name);
                        view.options.push((name.clone(), value.clone()));
                    }
                }
                AlterViewOptionsAction::Reset(names) => {
                    view.options.retain(|(current, _)| !names.contains(current));
                }
            }
            if view.persistence != uqa_sql::ast::RelationPersistence::Temporary {
                if let Some(catalog) = engine.storage.catalog.as_ref() {
                    let definition_json = serde_json::to_string(&view).map_err(|error| {
                        SQLError::Internal(format!(
                            "serialize altered {expected_kind} `{canonical}`: {error}"
                        ))
                    })?;
                    catalog
                        .save_view(&ViewRow {
                            relation: relation.clone(),
                            definition_json,
                        })
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
