//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Namespace ownership, dependency removal, and atomic multi-schema DROP.

use std::collections::BTreeSet;

use uqa_sql::ast::DropStmt;
use uqa_sql::SQLError;

use crate::{Engine, RelationIdentity, StorageBackendError};

fn storage_error(error: &StorageBackendError) -> SQLError {
    SQLError::Internal(format!("DROP SCHEMA: {error}"))
}

impl Engine {
    pub(crate) fn drop_schemas_sql(&self, statement: &DropStmt) -> Result<(), SQLError> {
        self.synchronize_catalog_registries()
            .map_err(|error| storage_error(&error))?;
        let mut schemas = BTreeSet::new();
        let mut graphs = BTreeSet::new();
        for name in &statement.names {
            let Some(security) = self.schema_security_for_privilege(name) else {
                if statement.if_exists {
                    self.push_sql_notice(
                        "NOTICE",
                        &format!("schema \"{name}\" does not exist, skipping"),
                    );
                    continue;
                }
                return Err(SQLError::Routine {
                    sqlstate: "3F000".into(),
                    message: format!("schema \"{name}\" does not exist"),
                });
            };
            if !self.current_user_has_role_privileges(&security.role_owner) {
                return Err(SQLError::Routine {
                    sqlstate: "42501".into(),
                    message: format!("must be owner of schema {name}"),
                });
            }
            if super::is_virtual_system_schema(name) {
                return Err(SQLError::Routine {
                    sqlstate: "2BP01".into(),
                    message: format!("schema `{name}` cannot be dropped"),
                });
            }
            if self
                .has_graph(name)
                .map_err(|error| storage_error(&error))?
            {
                graphs.insert(name.clone());
            } else {
                schemas.insert(name.clone());
            }
        }
        if !statement.cascade {
            let occupied = schemas
                .iter()
                .find(|name| !self.schema_is_empty(name))
                .or_else(|| graphs.first());
            if let Some(name) = occupied {
                let single = schemas.len() + graphs.len() == 1;
                let object = if single {
                    format!("schema {name}")
                } else {
                    "desired object(s)".into()
                };
                return Err(SQLError::Routine {
                    sqlstate: "2BP01".into(),
                    message: format!(
                        "cannot drop {object} because other objects depend on {}",
                        if single { "it" } else { "them" }
                    ),
                });
            }
        }
        if statement.cascade {
            self.drop_schema_types_and_routines(&schemas)?;
            self.drop_schema_relations(&schemas)?;
            let sequences = self
                .durable
                .sequences
                .read()
                .keys()
                .filter(|relation| schemas.contains(&relation.schema))
                .map(RelationIdentity::qualified_name)
                .collect::<Vec<_>>();
            for sequence in sequences {
                self.drop_owned_sequence(&sequence, true)
                    .map_err(|error| storage_error(&error))?;
            }
        }
        for graph in graphs {
            for table in self
                .tables_in_schema(&graph)
                .map_err(|error| storage_error(&error))?
            {
                let relation = RelationIdentity {
                    schema: graph.clone(),
                    name: table,
                };
                self.lock_relation(
                    &relation.qualified_name(),
                    crate::row_locks::RelationLockMode::AccessExclusive,
                )?;
            }
            self.drop_graph(&graph)
                .map_err(|error| storage_error(&error))?;
        }
        for schema in schemas {
            self.drop_schema(&schema)
                .map_err(|error| storage_error(&error))?;
        }
        Ok(())
    }

    fn drop_schema_relations(&self, schemas: &BTreeSet<String>) -> Result<(), SQLError> {
        let tables = self
            .storage
            .tables
            .read()
            .keys()
            .filter(|relation| schemas.contains(&relation.schema))
            .map(RelationIdentity::qualified_name)
            .collect::<Vec<_>>();
        let (tables, _) = self.hierarchy_drop_targets(&tables, true);
        for table in &tables {
            self.lock_relation(table, crate::row_locks::RelationLockMode::AccessExclusive)?;
            self.ensure_no_pending_trigger_events(table, "DROP TABLE")?;
        }
        if !tables.is_empty() {
            self.try_drop_tables(&tables, true)
                .map_err(|error| storage_error(&error))?;
        }
        let foreign = self
            .durable
            .foreign_tables
            .read()
            .keys()
            .filter(|relation| schemas.contains(&relation.schema))
            .map(RelationIdentity::qualified_name)
            .collect::<Vec<_>>();
        let owned_sequences = self
            .foreign_table_owned_sequence_names(&foreign)
            .map_err(|error| storage_error(&error))?;
        for table in &foreign {
            self.lock_relation(table, crate::row_locks::RelationLockMode::AccessExclusive)?;
        }
        self.drop_rules_depending_on_relations_inner(&foreign)
            .map_err(|error| storage_error(&error))?;
        self.drop_views_depending_on_relations(&foreign)
            .map_err(|error| storage_error(&error))?;
        for table in foreign {
            self.drop_foreign_table_inner(&table)
                .map_err(SQLError::Internal)?;
        }
        for sequence in owned_sequences {
            self.drop_owned_sequence(&sequence, true)
                .map_err(|error| storage_error(&error))?;
        }
        let views = self
            .durable
            .views
            .read()
            .keys()
            .filter(|relation| schemas.contains(&relation.schema))
            .map(RelationIdentity::qualified_name)
            .collect::<Vec<_>>();
        self.drop_relation_routine_dependents(&views, true, "view")?;
        let views = self.remaining_view_drop_targets(&views)?;
        let closure = self.cascade_view_closure(views)?;
        for view in &closure {
            self.lock_relation(view, crate::row_locks::RelationLockMode::AccessExclusive)?;
        }
        self.drop_rules_depending_on_relations_inner(&closure)
            .map_err(|error| storage_error(&error))?;
        self.drop_views_inner(&closure, false)
    }
}
