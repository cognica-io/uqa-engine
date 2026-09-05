//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Engine, RelationIdentity};
use uqa_core::{ArrayValue, Value};

mod schema;

pub(crate) use schema::StoredForeignTable;

pub(crate) fn sql_column_type_to_fdw(
    column_type: &uqa_sql::ast::ColumnType,
) -> uqa_fdw::ColumnType {
    match column_type {
        uqa_sql::ast::ColumnType::Boolean => uqa_fdw::ColumnType::Bool,
        uqa_sql::ast::ColumnType::Void => uqa_fdw::ColumnType::Void,
        uqa_sql::ast::ColumnType::SmallInteger => uqa_fdw::ColumnType::SmallInteger,
        uqa_sql::ast::ColumnType::Integer => uqa_fdw::ColumnType::Integer,
        uqa_sql::ast::ColumnType::BigInteger => uqa_fdw::ColumnType::BigInteger,
        uqa_sql::ast::ColumnType::Oid => uqa_fdw::ColumnType::Oid,
        uqa_sql::ast::ColumnType::Xid => uqa_fdw::ColumnType::Xid,
        uqa_sql::ast::ColumnType::Real => uqa_fdw::ColumnType::Real,
        uqa_sql::ast::ColumnType::DoublePrecision => uqa_fdw::ColumnType::DoublePrecision,
        uqa_sql::ast::ColumnType::Numeric { precision, scale } => uqa_fdw::ColumnType::Numeric {
            precision: *precision,
            scale: *scale,
        },
        uqa_sql::ast::ColumnType::Text => uqa_fdw::ColumnType::Text,
        uqa_sql::ast::ColumnType::RefCursor => uqa_fdw::ColumnType::RefCursor,
        uqa_sql::ast::ColumnType::Name => uqa_fdw::ColumnType::Name,
        uqa_sql::ast::ColumnType::Uuid => uqa_fdw::ColumnType::Uuid,
        uqa_sql::ast::ColumnType::Varchar(length) => uqa_fdw::ColumnType::Varchar(*length),
        uqa_sql::ast::ColumnType::Bpchar => uqa_fdw::ColumnType::Bpchar,
        uqa_sql::ast::ColumnType::Character(length) => uqa_fdw::ColumnType::Character(*length),
        uqa_sql::ast::ColumnType::Json => uqa_fdw::ColumnType::Json,
        uqa_sql::ast::ColumnType::JsonB => uqa_fdw::ColumnType::JsonB,
        uqa_sql::ast::ColumnType::Date => uqa_fdw::ColumnType::Date,
        uqa_sql::ast::ColumnType::Time => uqa_fdw::ColumnType::Time,
        uqa_sql::ast::ColumnType::TimeTz => uqa_fdw::ColumnType::TimeTz,
        uqa_sql::ast::ColumnType::Timestamp => uqa_fdw::ColumnType::Timestamp,
        uqa_sql::ast::ColumnType::TimestampTz => uqa_fdw::ColumnType::TimestampTz,
        uqa_sql::ast::ColumnType::Interval => uqa_fdw::ColumnType::Interval,
        uqa_sql::ast::ColumnType::Range(subtype) => {
            uqa_fdw::ColumnType::Range(sql_range_subtype_to_fdw(*subtype))
        }
        uqa_sql::ast::ColumnType::Multirange(subtype) => {
            uqa_fdw::ColumnType::Multirange(sql_range_subtype_to_fdw(*subtype))
        }
        uqa_sql::ast::ColumnType::Bytea => uqa_fdw::ColumnType::Bytes,
        uqa_sql::ast::ColumnType::InternalChar => uqa_fdw::ColumnType::InternalChar,
        uqa_sql::ast::ColumnType::Regproc => uqa_fdw::ColumnType::Regproc,
        uqa_sql::ast::ColumnType::Regprocedure => uqa_fdw::ColumnType::Regprocedure,
        uqa_sql::ast::ColumnType::Regclass => uqa_fdw::ColumnType::Regclass,
        uqa_sql::ast::ColumnType::Regnamespace => uqa_fdw::ColumnType::Regnamespace,
        uqa_sql::ast::ColumnType::Regrole => uqa_fdw::ColumnType::Regrole,
        uqa_sql::ast::ColumnType::Regtype => uqa_fdw::ColumnType::Regtype,
        uqa_sql::ast::ColumnType::PgNodeTree => uqa_fdw::ColumnType::PgNodeTree,
        uqa_sql::ast::ColumnType::AclItem => uqa_fdw::ColumnType::AclItem,
        uqa_sql::ast::ColumnType::Int2Vector => uqa_fdw::ColumnType::Int2Vector,
        uqa_sql::ast::ColumnType::OidVector => uqa_fdw::ColumnType::OidVector,
        uqa_sql::ast::ColumnType::AnyArray => uqa_fdw::ColumnType::AnyArray,
        uqa_sql::ast::ColumnType::Record => uqa_fdw::ColumnType::Record,
        uqa_sql::ast::ColumnType::Vector(dimension) => uqa_fdw::ColumnType::Vector(*dimension),
        uqa_sql::ast::ColumnType::Tensor(dimension) => uqa_fdw::ColumnType::Tensor(*dimension),
        uqa_sql::ast::ColumnType::Array(element) => {
            uqa_fdw::ColumnType::Array(Box::new(sql_column_type_to_fdw(element)))
        }
        uqa_sql::ast::ColumnType::Domain {
            schema,
            name,
            oid,
            base,
        } => uqa_fdw::ColumnType::Domain {
            schema: schema.clone(),
            name: name.clone(),
            oid: *oid,
            base: Box::new(sql_column_type_to_fdw(base)),
        },
    }
}

fn sql_range_subtype_to_fdw(subtype: uqa_sql::ast::RangeSubtype) -> uqa_fdw::RangeSubtype {
    match subtype {
        uqa_sql::ast::RangeSubtype::Integer => uqa_fdw::RangeSubtype::Integer,
        uqa_sql::ast::RangeSubtype::BigInteger => uqa_fdw::RangeSubtype::BigInteger,
        uqa_sql::ast::RangeSubtype::Numeric => uqa_fdw::RangeSubtype::Numeric,
        uqa_sql::ast::RangeSubtype::Date => uqa_fdw::RangeSubtype::Date,
        uqa_sql::ast::RangeSubtype::Timestamp => uqa_fdw::RangeSubtype::Timestamp,
        uqa_sql::ast::RangeSubtype::TimestampTz => uqa_fdw::RangeSubtype::TimestampTz,
    }
}

struct MemoryForeignRowStream<'a> {
    engine: &'a Engine,
    table_name: RelationIdentity,
    columns: Option<Vec<String>>,
    predicates: Vec<uqa_fdw::FDWPredicate>,
    limit: Option<u64>,
    index: usize,
    emitted: u64,
}

fn fdw_column_type_is_array(column_type: &uqa_fdw::ColumnType) -> bool {
    match column_type {
        uqa_fdw::ColumnType::Array(_) => true,
        uqa_fdw::ColumnType::Domain { base, .. } => fdw_column_type_is_array(base),
        _ => false,
    }
}

fn normalize_foreign_array_columns(
    mut row: uqa_fdw::Row,
    array_columns: &[String],
) -> std::result::Result<uqa_fdw::Row, String> {
    for column in array_columns {
        let Some(value) = row.get_mut(column) else {
            continue;
        };
        let normalized = match std::mem::take(value) {
            Value::Null => Value::Null,
            Value::Array(array) => Value::Array(array),
            Value::List(elements) => {
                Value::Array(ArrayValue::try_new(elements).ok_or_else(|| {
                    format!("foreign array column `{column}` contains non-rectangular dimensions")
                })?)
            }
            other => {
                return Err(format!(
                    "foreign array column `{column}` requires an array value, got {other:?}"
                ));
            }
        };
        *value = normalized;
    }
    Ok(row)
}

impl Iterator for MemoryForeignRowStream<'_> {
    type Item = std::result::Result<uqa_fdw::Row, String>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.limit.is_some_and(|limit| self.emitted >= limit) {
            return None;
        }
        loop {
            let row = {
                let tables = self.engine.extensions.foreign_memory_tables.read();
                let Some(rows) = tables.get(&self.table_name) else {
                    return Some(Err(format!(
                        "Foreign table `{}` lost its loaded memory data during the scan",
                        self.table_name.qualified_name()
                    )));
                };
                let row = rows.get(self.index)?;
                row.clone()
            };
            self.index = match self.index.checked_add(1) {
                Some(index) => index,
                None => return Some(Err("memory FDW cursor index overflow".into())),
            };
            match uqa_fdw::row_matches_predicates(&row, &self.predicates) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(error) => return Some(Err(error.to_string())),
            }
            self.emitted = match self.emitted.checked_add(1) {
                Some(emitted) => emitted,
                None => return Some(Err("memory FDW emitted-row count overflow".into())),
            };
            return Some(Ok(uqa_fdw::project_row(&row, self.columns.as_deref())));
        }
    }
}

impl Engine {
    pub fn register_foreign_server(
        &self,
        name: String,
        fdw_type: String,
        options: Vec<(String, String)>,
        if_not_exists: bool,
    ) -> std::result::Result<(), String> {
        self.with_implicit_string_transaction(move |engine| {
            engine.register_foreign_server_inner(name, &fdw_type, options, if_not_exists)
        })
    }

    fn register_foreign_server_inner(
        &self,
        name: String,
        fdw_type: &str,
        options: Vec<(String, String)>,
        if_not_exists: bool,
    ) -> std::result::Result<(), String> {
        self.synchronize_catalog_registries()
            .map_err(|err| format!("refresh FDW catalog: {err}"))?;
        let mut servers = self.durable.foreign_servers.write();
        if servers.contains_key(&name) {
            if if_not_exists {
                return Ok(());
            }
            return Err(format!("Foreign server `{name}` already exists"));
        }
        if !matches!(fdw_type, "duckdb_fdw" | "arrow_fdw" | "memory_fdw") {
            return Err(format!("Unsupported FDW type: `{fdw_type}`"));
        }
        let mut opt_map: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for (k, v) in options {
            opt_map.insert(k, v);
        }
        let server = uqa_fdw::ForeignServer {
            name: name.clone(),
            fdw_type: fdw_type.to_string(),
            options: opt_map.clone(),
        };
        if let Some(catalog) = self.storage.catalog.as_ref() {
            let options_json = serde_json::to_string(&opt_map)
                .map_err(|err| format!("serialize foreign server `{name}`: {err}"))?;
            catalog
                .save_foreign_server(&name, fdw_type, &options_json)
                .map_err(|err| format!("persist foreign server `{name}`: {err}"))?;
        }
        servers.insert(name, server);
        drop(servers);
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub fn register_foreign_table(
        &self,
        name: String,
        server_name: String,
        columns: Vec<uqa_sql::ast::ColumnDef>,
        options: Vec<(String, String)>,
        if_not_exists: bool,
    ) -> std::result::Result<(), String> {
        self.with_implicit_string_transaction(move |engine| {
            engine
                .register_foreign_table_inner(
                    &name,
                    server_name,
                    columns,
                    Vec::new(),
                    options,
                    if_not_exists,
                )
                .map_err(|error| error.to_string())
        })
    }

    pub(crate) fn register_foreign_table_with_checks(
        &self,
        name: String,
        server_name: String,
        columns: Vec<uqa_sql::ast::ColumnDef>,
        checks: Vec<uqa_sql::ast::TableCheck>,
        options: Vec<(String, String)>,
        if_not_exists: bool,
    ) -> Result<(), uqa_sql::SQLError> {
        self.with_implicit_transaction(move |engine| {
            engine.register_foreign_table_inner(
                &name,
                server_name,
                columns,
                checks,
                options,
                if_not_exists,
            )
        })
    }

    pub(crate) fn register_deferred_foreign_table(
        &self,
        deferred: uqa_sql::ast::DeferredCreateForeignTable,
    ) -> Result<(), uqa_sql::SQLError> {
        self.with_implicit_transaction(move |engine| {
            let Some((name, relation)) =
                engine.preflight_foreign_table_creation(&deferred.name, true)?
            else {
                return Ok(());
            };
            let statement = uqa_sql::resolve_deferred_create_foreign_table(&deferred)?;
            Self::validate_foreign_table_schema_envelope(&statement.columns)?;
            engine.register_foreign_table_after_preflight(
                &name,
                relation,
                statement.server_name,
                statement.columns,
                statement.checks,
                statement.options,
            )
        })
    }

    fn preflight_foreign_table_creation(
        &self,
        name: &str,
        if_not_exists: bool,
    ) -> Result<Option<(String, RelationIdentity)>, uqa_sql::SQLError> {
        self.synchronize_catalog_registries().map_err(|error| {
            uqa_sql::SQLError::Internal(format!("refresh FDW catalog: {error}"))
        })?;
        let name = self.try_relation_name_for_sql_create(name)?;
        let relation = RelationIdentity::from_legacy_name(&name).map_err(|error| {
            uqa_sql::SQLError::Internal(format!("decode foreign table `{name}`: {error}"))
        })?;
        if self
            .relation_kind_at(&name)
            .map_err(|error| {
                uqa_sql::SQLError::Internal(format!("resolve relation `{name}`: {error}"))
            })?
            .is_some()
        {
            if if_not_exists {
                self.push_sql_notice(
                    "NOTICE",
                    &format!("relation \"{}\" already exists, skipping", relation.name),
                );
                return Ok(None);
            }
            return Err(uqa_sql::SQLError::Routine {
                sqlstate: "42P07".into(),
                message: format!("relation \"{}\" already exists", relation.name),
            });
        }
        {
            let tables = self.durable.foreign_tables.read();
            let table_security = self.durable.foreign_table_security.read();
            if tables.contains_key(&relation) {
                if !table_security.contains_key(&relation) {
                    return Err(uqa_sql::SQLError::Internal(format!(
                        "foreign table `{name}` has no loaded security metadata"
                    )));
                }
                return Err(uqa_sql::SQLError::Internal(format!(
                    "foreign table `{name}` appeared after relation collision preflight"
                )));
            }
            if table_security.contains_key(&relation) {
                return Err(uqa_sql::SQLError::Internal(format!(
                    "foreign table security metadata exists without table `{name}`"
                )));
            }
        }
        Ok(Some((name, relation)))
    }

    fn ensure_foreign_server_exists(&self, server_name: &str) -> Result<(), uqa_sql::SQLError> {
        if self
            .durable
            .foreign_servers
            .read()
            .contains_key(server_name)
        {
            return Ok(());
        }
        Err(uqa_sql::SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("server \"{server_name}\" does not exist"),
        })
    }

    fn register_foreign_table_inner(
        &self,
        name: &str,
        server_name: String,
        columns: Vec<uqa_sql::ast::ColumnDef>,
        checks: Vec<uqa_sql::ast::TableCheck>,
        options: Vec<(String, String)>,
        if_not_exists: bool,
    ) -> Result<(), uqa_sql::SQLError> {
        if !if_not_exists {
            Self::validate_foreign_table_schema_envelope(&columns)?;
        }
        let Some((name, relation)) = self.preflight_foreign_table_creation(name, if_not_exists)?
        else {
            return Ok(());
        };
        if if_not_exists {
            Self::validate_foreign_table_schema_envelope(&columns)?;
        }
        self.register_foreign_table_after_preflight(
            &name,
            relation,
            server_name,
            columns,
            checks,
            options,
        )
    }

    fn register_foreign_table_after_preflight(
        &self,
        name: &str,
        relation: RelationIdentity,
        server_name: String,
        mut columns: Vec<uqa_sql::ast::ColumnDef>,
        mut checks: Vec<uqa_sql::ast::TableCheck>,
        options: Vec<(String, String)>,
    ) -> Result<(), uqa_sql::SQLError> {
        self.materialize_implicit_sequences(
            "CREATE FOREIGN TABLE",
            name,
            &mut columns,
            uqa_sql::ast::RelationPersistence::Permanent,
        )?;
        self.prepare_foreign_table_schema(name, &mut columns, &mut checks)?;
        self.ensure_foreign_server_exists(&server_name)?;
        let mut opt_map: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for (k, v) in options {
            opt_map.insert(k, v);
        }
        let object_id = crate::new_table_object_id().map_err(|error| {
            uqa_sql::SQLError::Internal(format!(
                "allocate foreign table `{name}` object identity: {error}"
            ))
        })?;
        let owner_columns = columns.clone();
        let table = StoredForeignTable {
            name: name.to_string(),
            object_id,
            server_name,
            columns,
            checks,
            options: opt_map,
        };
        let role_owner = self.current_user_name();
        let security = crate::engine_state::TableSecurity::owner(role_owner);
        let mut tables = self.durable.foreign_tables.write();
        let mut table_security = self.durable.foreign_table_security.write();
        if tables.contains_key(&relation) || table_security.contains_key(&relation) {
            return Err(uqa_sql::SQLError::Internal(format!(
                "foreign table `{name}` changed during creation"
            )));
        }
        if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog
                .save_foreign_table(&table.catalog_row(&relation, &security).map_err(|error| {
                    uqa_sql::SQLError::Internal(format!(
                        "serialize foreign table `{name}`: {error}"
                    ))
                })?)
                .map_err(|error| {
                    uqa_sql::SQLError::Internal(format!("persist foreign table `{name}`: {error}"))
                })?;
        }
        tables.insert(relation.clone(), table);
        table_security.insert(relation, security);
        drop(table_security);
        drop(tables);
        self.attach_implicit_sequence_owners_for_columns(name, object_id, &owner_columns)
            .map_err(|error| {
                uqa_sql::SQLError::Internal(format!(
                    "attach foreign table `{name}` sequence ownership: {error}"
                ))
            })?;
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub fn drop_foreign_server(&self, name: &str) -> Result<bool, String> {
        self.with_implicit_string_transaction(|engine| engine.drop_foreign_server_inner(name))
    }

    fn drop_foreign_server_inner(&self, name: &str) -> Result<bool, String> {
        self.synchronize_catalog_registries()
            .map_err(|err| format!("refresh FDW catalog: {err}"))?;
        // Reject when any foreign table references this server.
        let referenced = self
            .durable
            .foreign_tables
            .read()
            .values()
            .any(|t| t.server_name == name);
        if referenced {
            return Err(format!(
                "foreign server `{name}` is referenced by a foreign table"
            ));
        }
        if !self.durable.foreign_servers.read().contains_key(name) {
            return Ok(false);
        }
        if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog
                .drop_foreign_server(name)
                .map_err(|err| format!("drop foreign server `{name}`: {err}"))?;
        }
        let removed = self.durable.foreign_servers.write().remove(name).is_some();
        if removed {
            self.note_catalog_registry_changed();
        }
        Ok(removed)
    }

    pub fn drop_foreign_table(&self, name: &str) -> Result<bool, String> {
        self.with_implicit_string_transaction(|engine| {
            engine
                .synchronize_catalog_registries()
                .map_err(|error| format!("refresh FDW catalog: {error}"))?;
            let Some(canonical) = engine
                .resolve_foreign_table_name(name)
                .map_err(|error| format!("resolve foreign table: {error}"))?
            else {
                return Ok(false);
            };
            let tables = vec![canonical.clone()];
            let targets = std::collections::BTreeSet::from([canonical.clone()]);
            let owned_sequences = engine
                .foreign_table_owned_sequence_names(&tables)
                .map_err(|error| format!("resolve owned sequences: {error}"))?;
            for sequence in &owned_sequences {
                let dependents = engine
                    .sequence_external_dependents_for_owner_drop(sequence, &targets)
                    .map_err(|error| format!("inspect owned sequence `{sequence}`: {error}"))?;
                if !dependents.is_empty() {
                    return Err(format!(
                        "foreign table `{canonical}` has owned sequence `{sequence}` with dependent object(s) `{}`",
                        dependents.join("`, `")
                    ));
                }
            }
            if !engine.drop_foreign_table_inner(&canonical)? {
                return Err(format!(
                    "foreign table `{canonical}` disappeared after DROP preflight"
                ));
            }
            for sequence in owned_sequences {
                engine
                    .drop_owned_sequence(&sequence, false)
                    .map_err(|error| format!("drop owned sequence `{sequence}`: {error}"))?;
            }
            Ok(true)
        })
    }

    pub(crate) fn drop_foreign_table_inner(&self, name: &str) -> Result<bool, String> {
        self.synchronize_catalog_registries()
            .map_err(|err| format!("refresh FDW catalog: {err}"))?;
        let Some(name) = self
            .resolve_foreign_table_name(name)
            .map_err(|err| format!("resolve foreign table: {err}"))?
        else {
            return Ok(false);
        };
        let relation = RelationIdentity::from_legacy_name(&name)?;
        if !self.durable.foreign_tables.read().contains_key(&relation) {
            return Err(format!("Foreign table `{name}` disappeared before drop"));
        }
        if !self
            .durable
            .foreign_table_security
            .read()
            .contains_key(&relation)
        {
            return Err(format!(
                "Foreign table `{name}` has no loaded security metadata"
            ));
        }
        self.drop_relation_events_inner(&relation)
            .map_err(|error| format!("drop foreign table `{name}` events: {error}"))?;
        if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog
                .drop_foreign_table(&relation)
                .map_err(|err| format!("drop foreign table `{name}`: {err}"))?;
        }
        self.extensions
            .foreign_memory_tables
            .write()
            .remove(&relation);
        let mut tables = self.durable.foreign_tables.write();
        let mut table_security = self.durable.foreign_table_security.write();
        let removed = tables.remove(&relation).is_some();
        table_security.remove(&relation);
        drop(table_security);
        drop(tables);
        if removed {
            self.note_catalog_registry_changed();
        }
        Ok(removed)
    }

    pub fn foreign_server(&self, name: &str) -> Result<Option<uqa_fdw::ForeignServer>, String> {
        if let Some(snapshot) = self.query_catalog_snapshot.as_ref() {
            return Ok(snapshot.foreign_servers.get(name).cloned());
        }
        self.synchronize_catalog_registries()
            .map_err(|err| format!("refresh FDW catalog: {err}"))?;
        Ok(self.durable.foreign_servers.read().get(name).cloned())
    }

    pub fn foreign_table(&self, name: &str) -> Result<Option<uqa_fdw::ForeignTable>, String> {
        if let Some(snapshot) = self.query_catalog_snapshot.as_ref() {
            return Ok(self
                .relation_lookup_candidates(name)
                .map_err(|err| format!("resolve foreign table: {err}"))?
                .into_iter()
                .find_map(|relation| {
                    snapshot
                        .foreign_tables
                        .get(&relation)
                        .map(StoredForeignTable::fdw_definition)
                }));
        }
        self.synchronize_catalog_registries()
            .map_err(|err| format!("refresh FDW catalog: {err}"))?;
        let Some(resolved) = self
            .resolve_foreign_table_name(name)
            .map_err(|err| format!("resolve foreign table: {err}"))?
        else {
            return Ok(None);
        };
        let relation = RelationIdentity::from_legacy_name(&resolved)?;
        Ok(self
            .durable
            .foreign_tables
            .read()
            .get(&relation)
            .map(StoredForeignTable::fdw_definition))
    }

    pub fn list_foreign_servers(&self) -> Result<Vec<String>, String> {
        if let Some(snapshot) = self.query_catalog_snapshot.as_ref() {
            let mut out = snapshot.foreign_servers.keys().cloned().collect::<Vec<_>>();
            out.sort();
            return Ok(out);
        }
        self.synchronize_catalog_registries()
            .map_err(|err| format!("refresh FDW catalog: {err}"))?;
        let mut out: Vec<String> = self
            .durable
            .foreign_servers
            .read()
            .keys()
            .cloned()
            .collect();
        out.sort();
        Ok(out)
    }

    pub fn list_foreign_tables(&self) -> Result<Vec<String>, String> {
        if let Some(snapshot) = self.query_catalog_snapshot.as_ref() {
            let mut out = snapshot
                .foreign_tables
                .keys()
                .map(RelationIdentity::qualified_name)
                .collect::<Vec<_>>();
            out.sort();
            return Ok(out);
        }
        self.synchronize_catalog_registries()
            .map_err(|err| format!("refresh FDW catalog: {err}"))?;
        let mut out: Vec<String> = self
            .durable
            .foreign_tables
            .read()
            .keys()
            .map(RelationIdentity::qualified_name)
            .collect();
        out.sort();
        Ok(out)
    }

    pub fn foreign_table_columns(&self, table: &str) -> Result<Vec<String>, String> {
        let table = self
            .foreign_table(table)?
            .ok_or_else(|| format!("Foreign table `{table}` does not exist"))?;
        Ok(table
            .columns
            .iter()
            .map(|column| column.name.clone())
            .collect())
    }

    pub fn load_memory_foreign_table(
        &self,
        table_name: impl Into<String>,
        rows: Vec<uqa_fdw::Row>,
    ) -> std::result::Result<(), String> {
        let table_name = table_name.into();
        let table_name = self
            .resolve_foreign_table_name(&table_name)
            .map_err(|err| format!("resolve foreign table: {err}"))?
            .ok_or_else(|| format!("Foreign table `{table_name}` does not exist"))?;
        let relation = RelationIdentity::from_legacy_name(&table_name)?;
        let table = self
            .foreign_table(&table_name)?
            .ok_or_else(|| format!("Foreign table `{table_name}` does not exist"))?;
        let server = self
            .foreign_server(&table.server_name)?
            .ok_or_else(|| format!("Foreign server `{}` does not exist", table.server_name))?;
        if server.fdw_type != "memory_fdw" {
            return Err(format!(
                "Foreign table `{table_name}` is backed by `{}` not `memory_fdw`",
                server.fdw_type
            ));
        }
        let array_columns = table
            .columns
            .iter()
            .filter(|column| fdw_column_type_is_array(&column.ty))
            .map(|column| column.name.clone())
            .collect::<Vec<_>>();
        let rows = rows
            .into_iter()
            .map(|row| normalize_foreign_array_columns(row, &array_columns))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        self.extensions
            .foreign_memory_tables
            .write()
            .insert(relation, rows);
        Ok(())
    }

    pub(crate) fn scan_foreign_table_stream<'a>(
        &'a self,
        table_name: &str,
        columns: Option<&[String]>,
        predicates: &[uqa_fdw::FDWPredicate],
        limit: Option<u64>,
    ) -> std::result::Result<
        Box<dyn Iterator<Item = std::result::Result<uqa_fdw::Row, String>> + Send + 'a>,
        String,
    > {
        #[cfg(not(target_os = "emscripten"))]
        use uqa_fdw::FDWHandler as _;

        let table = self
            .foreign_table(table_name)?
            .ok_or_else(|| format!("Foreign table `{table_name}` does not exist"))?;
        let server = self
            .foreign_server(&table.server_name)?
            .ok_or_else(|| format!("Foreign server `{}` does not exist", table.server_name))?;
        let array_columns = table
            .columns
            .iter()
            .filter(|column| fdw_column_type_is_array(&column.ty))
            .map(|column| column.name.clone())
            .collect::<Vec<_>>();

        let rows: Box<dyn Iterator<Item = std::result::Result<uqa_fdw::Row, String>> + Send + 'a> =
            match server.fdw_type.as_str() {
                "memory_fdw" => {
                    let relation = RelationIdentity::from_legacy_name(&table.name)?;
                    if !self
                        .extensions
                        .foreign_memory_tables
                        .read()
                        .contains_key(&relation)
                    {
                        return Err(format!(
                            "Foreign table `{}` has no loaded memory data",
                            table.name
                        ));
                    }
                    Box::new(MemoryForeignRowStream {
                        engine: self,
                        table_name: relation,
                        columns: columns.map(<[String]>::to_vec),
                        predicates: predicates.to_vec(),
                        limit,
                        index: 0,
                        emitted: 0,
                    })
                }
                #[cfg(not(target_os = "emscripten"))]
                "duckdb_fdw" => {
                    let handler = uqa_fdw::DuckDBHandler::new(server);
                    Box::new(
                        handler
                            .scan_stream(&table, columns, predicates, limit)
                            .map_err(|error| error.to_string())?
                            .map(|row| row.map_err(|error| error.to_string())),
                    )
                }
                #[cfg(not(target_os = "emscripten"))]
                "arrow_fdw" => {
                    let handler = uqa_fdw::ArrowIpcHandler::new(server);
                    Box::new(
                        handler
                            .scan_stream(&table, columns, predicates, limit)
                            .map_err(|error| error.to_string())?
                            .map(|row| row.map_err(|error| error.to_string())),
                    )
                }
                other => return Err(format!("FDW type `{other}` is not available in this build")),
            };
        Ok(Box::new(rows.map(move |row| {
            row.and_then(|row| normalize_foreign_array_columns(row, &array_columns))
        })))
    }
}
