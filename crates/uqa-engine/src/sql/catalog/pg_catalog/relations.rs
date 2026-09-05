//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation, view, database, and stable OID catalog projection.

use uqa_core::Value;
use uqa_sql::{ResultRow, SQLError};

use crate::engine_capabilities::{CatalogReadView, RelationNameResolution};

use super::super::helpers::acl::acl_identifier;
use super::super::helpers::oids::{
    current_user_oid, relation_oid, schema_oid, split_schema_name, stable_object_oid, stable_oid,
};
use super::super::helpers::rows::{
    bool_value, catalog_array, catalog_name, int_value, row, str_value,
};

pub(in crate::sql::catalog) fn build_pg_tables(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut out: Vec<ResultRow> = Vec::new();
    for name in catalog.table_names() {
        let (schema, table) = split_schema_name(&name)?;
        let table_snapshot = catalog
            .table(resolution, &name)?
            .ok_or_else(|| SQLError::UnknownTable(name.clone()))?;
        out.push(row([
            ("schemaname", str_value(schema.clone())),
            ("tablename", str_value(table)),
            ("tableowner", str_value(table_snapshot.role_owner.clone())),
            ("tablespace", Value::Null),
            (
                "hasindexes",
                bool_value(
                    catalog
                        .catalog_indexes()
                        .any(|index| index.table_name == name)
                        || !table_snapshot.keys.is_empty(),
                ),
            ),
            (
                "hasrules",
                bool_value(catalog.table_has_rules(resolution, &name)?),
            ),
            (
                "hastriggers",
                bool_value(catalog.relation_has_triggers(resolution, &name)?),
            ),
            ("rowsecurity", bool_value(false)),
            ("table_catalog", catalog_name()),
            ("table_schema", str_value(schema)),
            ("table_type", str_value("BASE TABLE")),
        ]));
    }
    Ok(out)
}

pub(in crate::sql::catalog) fn table_relation_oid_from(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    table: &str,
) -> Result<i64, SQLError> {
    if let Some(table_state) = catalog.table(resolution, table)? {
        return Ok(stable_object_oid("relation", &table_state.object_id));
    }
    if let Some((_, table)) = catalog.foreign_table_entry_resolved(resolution, table)? {
        return Ok(crate::sql::foreign_table_relation_oid(&table));
    }
    Err(SQLError::UnknownTable(table.to_string()))
}

pub(in crate::sql::catalog) fn table_rowtype_oid_from(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    table: &str,
) -> Result<i64, SQLError> {
    let table_state = catalog
        .table(resolution, table)?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    Ok(stable_object_oid("rowtype", &table_state.object_id))
}

pub(in crate::sql::catalog) fn pg_class_row(
    schema: &str,
    name: &str,
    relkind: &str,
    natts: i64,
    tuples: f64,
    has_index: bool,
) -> ResultRow {
    pg_class_row_with_lifecycle(
        schema,
        name,
        relkind,
        natts,
        tuples,
        has_index,
        uqa_sql::ast::RelationPersistence::Permanent,
        true,
        &[],
    )
}

#[expect(clippy::too_many_arguments, reason = "keeps catalog metadata aligned")]
pub(in crate::sql::catalog) fn pg_class_row_with_lifecycle(
    schema: &str,
    name: &str,
    relkind: &str,
    natts: i64,
    tuples: f64,
    has_index: bool,
    persistence: uqa_sql::ast::RelationPersistence,
    populated: bool,
    options: &[(String, String)],
) -> ResultRow {
    let oid = relation_oid(relkind, schema, name);
    let reltype = if matches!(relkind, "r" | "v" | "m" | "c" | "f" | "p") {
        stable_oid("rowtype", &format!("{schema}.{name}"))
    } else {
        0
    };
    let mut row = pg_class_catalog_row(
        oid, reltype, schema, name, relkind, natts, tuples, has_index,
    );
    row.insert(
        "relpersistence".into(),
        str_value(persistence.catalog_code()),
    );
    row.insert("relispopulated".into(), bool_value(populated));
    if !options.is_empty() {
        let values = options
            .iter()
            .map(|(name, value)| str_value(format!("{name}={value}")))
            .collect();
        row.insert(
            "reloptions".into(),
            catalog_array(values, "pg_class.reloptions").unwrap_or(Value::Null),
        );
    }
    row
}

#[expect(clippy::too_many_arguments, reason = "keeps catalog metadata aligned")]
pub(in crate::sql::catalog) fn pg_class_catalog_row(
    oid: i64,
    reltype: i64,
    schema: &str,
    name: &str,
    relkind: &str,
    natts: i64,
    tuples: f64,
    has_index: bool,
) -> ResultRow {
    row([
        ("oid", int_value(oid)),
        ("relname", str_value(name)),
        ("relnamespace", int_value(schema_oid(schema))),
        ("reltype", int_value(reltype)),
        ("reloftype", int_value(0)),
        ("relowner", int_value(current_user_oid())),
        ("relam", int_value(0)),
        ("relfilenode", int_value(0)),
        ("reltablespace", int_value(0)),
        ("relpages", int_value(0)),
        ("reltuples", Value::Float(tuples)),
        ("relallvisible", int_value(0)),
        ("relallfrozen", int_value(0)),
        ("reltoastrelid", int_value(0)),
        ("relhasindex", bool_value(has_index)),
        ("relisshared", bool_value(false)),
        ("relpersistence", str_value("p")),
        ("relkind", str_value(relkind)),
        ("relnatts", int_value(natts)),
        ("relchecks", int_value(0)),
        ("relhasrules", bool_value(matches!(relkind, "v" | "m"))),
        ("relhastriggers", bool_value(false)),
        ("relhassubclass", bool_value(false)),
        ("relrowsecurity", bool_value(false)),
        ("relforcerowsecurity", bool_value(false)),
        ("relispopulated", bool_value(true)),
        (
            "relreplident",
            str_value(if matches!(relkind, "r" | "m" | "p") {
                "d"
            } else {
                "n"
            }),
        ),
        ("relispartition", bool_value(false)),
        ("relrewrite", int_value(0)),
        ("relfrozenxid", int_value(0)),
        ("relminmxid", int_value(0)),
        ("relacl", Value::Null),
        ("reloptions", Value::Null),
        ("relpartbound", Value::Null),
    ])
}

pub(in crate::sql::catalog) fn build_pg_views(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = Vec::new();
    for (name, stored) in catalog.views_of_kind(crate::StoredViewKind::View) {
        let (schema, view) = split_schema_name(&name)?;
        let definition = crate::sql::catalog::view_definition::view_definition(
            catalog, resolution, &stored, false, 0,
        )?;
        rows.push(row([
            ("schemaname", str_value(schema)),
            ("viewname", str_value(view)),
            ("viewowner", str_value(stored.role_owner)),
            ("definition", str_value(definition)),
        ]));
    }
    Ok(rows)
}

pub(in crate::sql::catalog) fn build_pg_matviews(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = Vec::new();
    for (name, stored) in catalog.views_of_kind(crate::StoredViewKind::Materialized) {
        let (schema, matview) = split_schema_name(&name)?;
        rows.push(row([
            ("schemaname", str_value(schema)),
            ("matviewname", str_value(matview)),
            ("matviewowner", str_value(&stored.role_owner)),
            ("tablespace", Value::Null),
            ("hasindexes", bool_value(false)),
            ("ispopulated", bool_value(stored.populated)),
            (
                "definition",
                str_value(crate::sql::catalog::view_definition::view_definition(
                    catalog, resolution, &stored, false, 0,
                )?),
            ),
        ]));
    }
    Ok(rows)
}

pub(in crate::sql::catalog) fn build_pg_database(
    catalog: &CatalogReadView,
) -> Result<Vec<ResultRow>, SQLError> {
    let security = catalog.database_security();
    Ok(vec![row([
        (
            "oid",
            int_value(crate::engine_database_security::DATABASE_OID),
        ),
        (
            "datname",
            str_value(crate::engine_database_security::DATABASE_NAME),
        ),
        (
            "datdba",
            int_value(crate::engine_roles::role_oid(&security.role_owner)),
        ),
        ("encoding", int_value(6)),
        ("datlocprovider", str_value("b")),
        ("datistemplate", bool_value(false)),
        ("datallowconn", bool_value(true)),
        ("dathasloginevt", bool_value(false)),
        ("datconnlimit", int_value(-1)),
        ("datfrozenxid", int_value(0)),
        ("datminmxid", int_value(0)),
        ("dattablespace", int_value(0)),
        ("datcollate", str_value("C")),
        ("datctype", str_value("C")),
        ("datlocale", str_value("PG_UNICODE_FAST")),
        ("daticurules", Value::Null),
        ("datcollversion", str_value("1")),
        ("datacl", database_acl_catalog_value(security)?),
    ])])
}

fn database_acl_catalog_value(
    security: &crate::engine_state::DatabaseSecurity,
) -> Result<Value, SQLError> {
    let Some(acl) = security.acl.as_ref() else {
        return Ok(Value::Null);
    };
    catalog_array(
        acl.iter()
            .map(|entry| {
                let grantee = if entry.role == "PUBLIC" {
                    String::new()
                } else {
                    acl_identifier(&entry.role)
                };
                let grantor =
                    acl_identifier(entry.grantor.as_deref().unwrap_or(&security.role_owner));
                let mut privileges = String::new();
                for (enabled, grant_option, code) in [
                    (entry.privileges.create, entry.grant_options.create, 'C'),
                    (
                        entry.privileges.temporary,
                        entry.grant_options.temporary,
                        'T',
                    ),
                    (entry.privileges.connect, entry.grant_options.connect, 'c'),
                ] {
                    if enabled {
                        privileges.push(code);
                        if grant_option {
                            privileges.push('*');
                        }
                    }
                }
                str_value(format!("{grantee}={privileges}/{grantor}"))
            })
            .collect(),
        "pg_database.datacl",
    )
}
