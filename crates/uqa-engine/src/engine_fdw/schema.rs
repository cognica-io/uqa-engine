//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical SQL definition and FDW projection for a foreign table.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uqa_sql::ast::{ColumnDef, TableCheck};

use crate::{
    CatalogFacade, Engine, RelationIdentity, SQLError, StorageBackendError, StorageBackendResult,
};

const FOREIGN_TABLE_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone)]
pub(crate) struct StoredForeignTable {
    pub(crate) name: String,
    pub(crate) object_id: [u8; 16],
    pub(crate) server_name: String,
    pub(crate) columns: Vec<ColumnDef>,
    pub(crate) checks: Vec<TableCheck>,
    pub(crate) options: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize)]
struct PersistedForeignTableSchema {
    version: u8,
    #[serde(default)]
    object_id: [u8; 16],
    columns: Vec<ColumnDef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    checks: Vec<TableCheck>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ForeignTableSchemaFormat {
    Current(PersistedForeignTableSchema),
    Legacy(Vec<ColumnDef>),
}

impl StoredForeignTable {
    pub(crate) fn from_catalog(
        name: String,
        server_name: String,
        options: BTreeMap<String, String>,
        schema_json: &str,
    ) -> StorageBackendResult<(Self, bool)> {
        let schema = serde_json::from_str::<ForeignTableSchemaFormat>(schema_json)?;
        let (object_id, columns, checks, legacy) = match schema {
            ForeignTableSchemaFormat::Current(schema) => {
                if schema.version != FOREIGN_TABLE_SCHEMA_VERSION {
                    return Err(StorageBackendError::Other(format!(
                        "foreign table `{name}` has unsupported schema version {}",
                        schema.version
                    )));
                }
                (schema.object_id, schema.columns, schema.checks, false)
            }
            ForeignTableSchemaFormat::Legacy(columns) => ([0; 16], columns, Vec::new(), true),
        };
        Ok((
            Self {
                name,
                object_id,
                server_name,
                columns,
                checks,
                options,
            },
            legacy,
        ))
    }

    pub(crate) fn schema_json(&self) -> StorageBackendResult<String> {
        serde_json::to_string(&PersistedForeignTableSchema {
            version: FOREIGN_TABLE_SCHEMA_VERSION,
            object_id: self.object_id,
            columns: self.columns.clone(),
            checks: self.checks.clone(),
        })
        .map_err(StorageBackendError::from)
    }

    pub(crate) fn fdw_definition(&self) -> uqa_fdw::ForeignTable {
        uqa_fdw::ForeignTable {
            name: self.name.clone(),
            server_name: self.server_name.clone(),
            columns: self
                .columns
                .iter()
                .map(|column| uqa_fdw::ColumnDef {
                    name: column.name.clone(),
                    ty: super::sql_column_type_to_fdw(&column.ty),
                })
                .collect(),
            options: self.options.clone(),
        }
    }

    pub(crate) fn catalog_row(
        &self,
        relation: &RelationIdentity,
        security: &crate::engine_state::TableSecurity,
    ) -> StorageBackendResult<uqa_storage::ForeignTableRow> {
        Ok(uqa_storage::ForeignTableRow {
            relation: relation.clone(),
            role_owner: security.role_owner.clone(),
            acl: security.acl.clone(),
            column_acls: security.column_acls.clone(),
            server_name: self.server_name.clone(),
            columns_json: self.schema_json()?,
            options_json: serde_json::to_string(&self.options)?,
        })
    }
}

impl Engine {
    pub(crate) fn validate_foreign_table_schema_envelope(
        columns: &[ColumnDef],
    ) -> Result<(), SQLError> {
        let mut names = std::collections::BTreeSet::new();
        for column in columns {
            if !names.insert(column.name.as_str()) {
                return Err(SQLError::Routine {
                    sqlstate: "42701".into(),
                    message: format!("column \"{}\" specified more than once", column.name),
                });
            }
            crate::sql::validate_postgres_column_name(&column.name)?;
            crate::sql::validate_postgres_relation_column_type(&column.name, &column.ty)?;
            if column.primary_key || column.unique {
                let kind = if column.primary_key {
                    "primary key"
                } else {
                    "unique"
                };
                return Err(SQLError::Unsupported(format!(
                    "{kind} constraints are not supported on foreign tables"
                )));
            }
            if column.references.is_some() {
                return Err(SQLError::Unsupported(
                    "foreign key constraints are not supported on foreign tables".into(),
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn migrate_foreign_table_identities(
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        for mut row in catalog.load_foreign_tables()? {
            let relation_name = row.relation.qualified_name();
            let options = serde_json::from_str(&row.options_json)?;
            let (mut table, legacy_schema) = StoredForeignTable::from_catalog(
                relation_name,
                row.server_name.clone(),
                options,
                &row.columns_json,
            )?;
            let mut changed = legacy_schema;
            if table.object_id == [0; 16] {
                table.object_id = crate::new_table_object_id()?;
                changed = true;
            }
            let mut constraints = uqa_sql::ast::TableConstraintSet {
                checks: std::mem::take(&mut table.checks),
                ..uqa_sql::ast::TableConstraintSet::default()
            };
            changed |= crate::engine_table_storage::materialize_constraint_metadata(
                &row.relation,
                &mut table.columns,
                &mut constraints,
            )?;
            table.checks = constraints.checks;
            changed |= Self::materialize_persisted_foreign_implicit_sequences(
                catalog,
                &row.relation,
                &row.role_owner,
                table.object_id,
                &mut table.columns,
            )?;
            if changed {
                row.columns_json = table.schema_json()?;
                catalog.save_foreign_table(&row)?;
            }
        }
        Ok(())
    }

    pub(crate) fn prepare_foreign_table_schema(
        &self,
        table_name: &str,
        columns: &mut [ColumnDef],
        checks: &mut Vec<TableCheck>,
    ) -> Result<(), SQLError> {
        self.prepare_foreign_table_schema_inner(table_name, columns, checks, false)
    }

    pub(crate) fn prepare_stored_foreign_table_schema(
        &self,
        table_name: &str,
        columns: &mut [ColumnDef],
        checks: &mut Vec<TableCheck>,
    ) -> Result<(), SQLError> {
        Self::validate_foreign_table_schema_envelope(columns)?;
        self.prepare_foreign_table_schema_inner(table_name, columns, checks, true)
    }

    fn prepare_foreign_table_schema_inner(
        &self,
        table_name: &str,
        columns: &mut [ColumnDef],
        checks: &mut Vec<TableCheck>,
        stored: bool,
    ) -> Result<(), SQLError> {
        let relation = RelationIdentity::from_legacy_name(table_name).map_err(|error| {
            SQLError::Internal(format!("decode foreign table `{table_name}`: {error}"))
        })?;
        let qualifier = relation.name.clone();
        let check_columns = columns.to_vec();
        for column in columns.iter_mut() {
            if let Some(default) = &mut column.default {
                self.prepare_foreign_table_sequence_references(default, stored)?;
                crate::sql::validate_default_expression(self, default, &column.ty)?;
            }
            if let Some(check) = &mut column.check {
                self.prepare_foreign_table_sequence_references(check, stored)?;
                crate::sql::validate_check_expression(
                    self,
                    table_name,
                    &qualifier,
                    &check_columns,
                    check,
                )?;
                crate::sql::reject_stored_regrole_constants(self, check, None)?;
            }
            if let Some(generated) = &mut column.generated {
                self.prepare_foreign_table_sequence_references(&mut generated.expression, stored)?;
            }
        }
        for check in checks.iter_mut() {
            self.prepare_foreign_table_sequence_references(&mut check.expr, stored)?;
            crate::sql::validate_check_expression(
                self,
                table_name,
                &qualifier,
                &check_columns,
                &mut check.expr,
            )?;
            crate::sql::reject_stored_regrole_constants(self, &check.expr, None)?;
        }
        crate::sql::prepare_generated_columns(self, &qualifier, columns, &[], &[])?;
        let mut constraints = uqa_sql::ast::TableConstraintSet {
            checks: std::mem::take(checks),
            ..uqa_sql::ast::TableConstraintSet::default()
        };
        crate::engine_table_storage::materialize_constraint_metadata(
            &relation,
            columns,
            &mut constraints,
        )
        .map_err(|error| SQLError::Internal(error.to_string()))?;
        *checks = constraints.checks;
        Ok(())
    }

    fn prepare_foreign_table_sequence_references(
        &self,
        expression: &mut uqa_sql::ast::Expr,
        stored: bool,
    ) -> Result<(), SQLError> {
        let result = if stored {
            self.resolve_loaded_sequence_references_in_expr(expression)
        } else {
            self.bind_sequence_references_in_expr(expression)
        };
        result.map_err(|error| SQLError::Internal(error.to_string()))
    }

    pub(crate) fn persist_foreign_table_definition(
        &self,
        relation: &RelationIdentity,
        table: &StoredForeignTable,
    ) -> StorageBackendResult<()> {
        let Some(catalog) = self.storage.catalog.as_ref() else {
            return Ok(());
        };
        let security = self
            .durable
            .foreign_table_security
            .read()
            .get(relation)
            .cloned()
            .ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "foreign table `{}` has no security metadata",
                    relation.qualified_name()
                ))
            })?;
        catalog.save_foreign_table(&table.catalog_row(relation, &security)?)
    }

    fn update_foreign_table_definition(
        &self,
        table_name: &str,
        update: impl FnOnce(&mut StoredForeignTable) -> StorageBackendResult<bool>,
    ) -> StorageBackendResult<Option<bool>> {
        let relation =
            RelationIdentity::from_legacy_name(table_name).map_err(StorageBackendError::Other)?;
        let Some(mut table) = self.durable.foreign_tables.read().get(&relation).cloned() else {
            return Ok(None);
        };
        if !update(&mut table)? {
            return Ok(Some(false));
        }
        self.persist_foreign_table_definition(&relation, &table)?;
        self.durable.foreign_tables.write().insert(relation, table);
        self.note_catalog_registry_changed();
        Ok(Some(true))
    }

    pub(crate) fn clear_foreign_table_default_dependency(
        &self,
        table_name: &str,
        column_name: &str,
    ) -> StorageBackendResult<Option<bool>> {
        self.update_foreign_table_definition(table_name, |table| {
            let Some(column) = table
                .columns
                .iter_mut()
                .find(|column| column.name == column_name)
            else {
                return Ok(false);
            };
            Ok(column.default.take().is_some())
        })
    }

    pub(crate) fn drop_foreign_table_check_dependency(
        &self,
        table_name: &str,
        constraint_name: &str,
    ) -> StorageBackendResult<Option<bool>> {
        self.update_foreign_table_definition(table_name, |table| {
            for column in &mut table.columns {
                if column.check.is_some() && column.check_name.as_deref() == Some(constraint_name) {
                    column.check = None;
                    column.check_name = None;
                    column.check_enforced = true;
                    column.check_validated = true;
                    column.check_no_inherit = false;
                    return Ok(true);
                }
            }
            let Some(index) = table
                .checks
                .iter()
                .position(|check| check.name.as_deref() == Some(constraint_name))
            else {
                return Ok(false);
            };
            table.checks.remove(index);
            Ok(true)
        })
    }

    pub(crate) fn drop_foreign_table_generated_column_dependency(
        &self,
        table_name: &str,
        column_name: &str,
    ) -> StorageBackendResult<Option<bool>> {
        let relation =
            RelationIdentity::from_legacy_name(table_name).map_err(StorageBackendError::Other)?;
        let Some(mut table) = self.durable.foreign_tables.read().get(&relation).cloned() else {
            return Ok(None);
        };
        let Some(column_index) = table
            .columns
            .iter()
            .position(|column| column.name == column_name && column.generated.is_some())
        else {
            return Ok(Some(false));
        };
        let dependent_views = self.views_depending_on_relation(table_name)?;
        if !dependent_views.is_empty() {
            return Err(StorageBackendError::Other(format!(
                "cannot drop generated column `{table_name}`.`{column_name}` while dependent view(s) `{}` remain",
                dependent_views.join("`, `")
            )));
        }
        for column in &table.columns {
            if column.name == column_name {
                continue;
            }
            if column.default.as_ref().is_some_and(|expression| {
                crate::engine_table_storage::schema_expr_references_column(expression, column_name)
            }) || column.generated.as_ref().is_some_and(|generated| {
                crate::engine_table_storage::schema_expr_references_column(
                    &generated.expression,
                    column_name,
                )
            }) {
                return Err(StorageBackendError::Other(format!(
                    "cannot drop generated column `{table_name}`.`{column_name}` because column `{}` depends on it",
                    column.name
                )));
            }
        }
        self.handle_drop_column_event_dependencies(table_name, column_name, false)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        for column in &mut table.columns {
            if column.name != column_name
                && column.check.as_ref().is_some_and(|expression| {
                    crate::engine_table_storage::schema_expr_references_column(
                        expression,
                        column_name,
                    )
                })
            {
                column.check = None;
                column.check_name = None;
                column.check_enforced = true;
                column.check_validated = true;
                column.check_no_inherit = false;
            }
        }
        table.columns.remove(column_index);
        table.checks.retain(|check| {
            !crate::engine_table_storage::schema_expr_references_column(&check.expr, column_name)
        });
        let mut security = self
            .durable
            .foreign_table_security
            .read()
            .get(&relation)
            .cloned()
            .ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "foreign table `{table_name}` has no security metadata"
                ))
            })?;
        security.column_acls.remove(column_name);
        if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog.save_foreign_table(&table.catalog_row(&relation, &security)?)?;
        }
        self.durable
            .foreign_tables
            .write()
            .insert(relation.clone(), table);
        self.durable
            .foreign_table_security
            .write()
            .insert(relation, security);
        self.note_catalog_registry_changed();
        Ok(Some(true))
    }

    pub(crate) fn detach_foreign_table_sequence_provenance(
        &self,
        sequence: &str,
    ) -> StorageBackendResult<bool> {
        let mut updates = Vec::new();
        for (relation, mut table) in self.durable.foreign_tables.read().clone() {
            let mut changed = false;
            for column in &mut table.columns {
                if column
                    .auto_increment
                    .as_ref()
                    .is_some_and(|provenance| provenance.sequence.as_deref() == Some(sequence))
                {
                    column.auto_increment = None;
                    changed = true;
                }
            }
            if changed {
                updates.push((relation, table));
            }
        }
        for (relation, table) in &updates {
            self.persist_foreign_table_definition(relation, table)?;
        }
        let changed = !updates.is_empty();
        if changed {
            let mut tables = self.durable.foreign_tables.write();
            for (relation, table) in updates {
                tables.insert(relation, table);
            }
            drop(tables);
            self.note_catalog_registry_changed();
        }
        Ok(changed)
    }
}
