//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Attribute and default catalog projection.

use uqa_core::{ArrayValue, Value};
use uqa_sql::ast::{ColumnDef as SQLColumnDef, ColumnType};
use uqa_sql::{ResultRow, SQLError};

use crate::engine_capabilities::{CatalogReadView, RelationNameResolution};
use crate::Engine;

use super::super::expression_text::default_expr_text;
use super::super::helpers::information_schema_types::array_dimension_count;
use super::super::helpers::oids::{split_schema_name, stable_oid};
use super::super::helpers::rows::{
    bool_value, catalog_ordinal, catalog_usize, int_value, row, str_value,
};
use super::super::helpers::type_metadata::{
    pg_type_align, pg_type_by_value, pg_type_collation_oid, pg_type_len, pg_type_modifier,
    pg_type_oid, pg_type_storage,
};
use super::super::helpers::views::view_columns_for;
use super::table_relation_oid_from;
use crate::sql::value_to_text;

#[expect(
    clippy::too_many_lines,
    reason = "preserves catalog column and OID order"
)]
pub(in crate::sql::catalog) fn build_pg_attribute(
    engine: &Engine,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut out = vec![row([
        ("attrelid", int_value(13_313)),
        ("attname", str_value("catalog_name")),
        ("atttypid", int_value(13_312)),
        ("attstattarget", Value::Null),
        ("attlen", int_value(64)),
        ("attnum", int_value(1)),
        ("attndims", int_value(0)),
        ("atttypmod", int_value(-1)),
        ("attbyval", bool_value(false)),
        ("attalign", str_value("c")),
        ("attstorage", str_value("p")),
        ("attcompression", str_value("")),
        ("attnotnull", bool_value(false)),
        ("atthasdef", bool_value(false)),
        ("atthasmissing", bool_value(false)),
        ("attidentity", str_value("")),
        ("attgenerated", str_value("")),
        ("attisdropped", bool_value(false)),
        ("attislocal", bool_value(true)),
        ("attinhcount", int_value(0)),
        ("attcollation", int_value(950)),
        ("attacl", Value::Null),
        ("attoptions", Value::Null),
        ("attfdwoptions", Value::Null),
        ("attmissingval", Value::Null),
    ])];
    for table_name in catalog.table_names() {
        let relid = table_relation_oid_from(catalog, resolution, &table_name)?;
        let table = catalog
            .table(resolution, &table_name)?
            .ok_or_else(|| SQLError::UnknownTable(table_name.clone()))?;
        let hierarchy = &table.hierarchy;
        let mut inherited_columns = Vec::new();
        for parent in &hierarchy.parents {
            inherited_columns.push(
                catalog
                    .table(resolution, parent)?
                    .ok_or_else(|| SQLError::UnknownTable(parent.clone()))?
                    .columns
                    .clone(),
            );
        }
        for (idx, col) in table.columns.iter().enumerate() {
            let inheritance_count = inherited_columns
                .iter()
                .filter(|columns| columns.iter().any(|parent| parent.name == col.name))
                .count();
            let mut attribute =
                pg_attribute_row(relid, catalog_ordinal(idx, "pg_attribute column")?, col);
            attribute.insert(
                "attacl".into(),
                super::super::relation_catalog::table_acl_catalog_value(
                    &table.role_owner,
                    table.column_acls.get(&col.name),
                )?,
            );
            attribute.insert(
                "attinhcount".into(),
                int_value(catalog_usize(
                    inheritance_count,
                    "pg_attribute inheritance count",
                )?),
            );
            let is_local = if hierarchy.local_columns.is_empty() {
                inheritance_count == 0
            } else {
                hierarchy
                    .local_columns
                    .iter()
                    .any(|local| local == &col.name)
            };
            attribute.insert("attislocal".into(), bool_value(is_local));
            out.push(attribute);
        }
    }
    for (_, stored) in catalog.views_of_kind(crate::StoredViewKind::View) {
        let relid = crate::sql::view_relation_oid(&stored);
        let columns = view_columns_for(engine, catalog, resolution, &stored)?;
        for (idx, col) in columns.iter().enumerate() {
            let mut attribute = pg_attribute_row(
                relid,
                catalog_ordinal(idx, "pg_attribute view column")?,
                col,
            );
            attribute.insert(
                "attacl".into(),
                super::super::relation_catalog::table_acl_catalog_value(
                    &stored.role_owner,
                    stored.column_acls.get(&col.name),
                )?,
            );
            out.push(attribute);
        }
    }
    for (_, stored) in catalog.views_of_kind(crate::StoredViewKind::Materialized) {
        let relid = crate::sql::view_relation_oid(&stored);
        let columns = view_columns_for(engine, catalog, resolution, &stored)?;
        for (idx, col) in columns.iter().enumerate() {
            let mut attribute = pg_attribute_row(
                relid,
                catalog_ordinal(idx, "pg_attribute materialized-view column")?,
                col,
            );
            attribute.insert(
                "attacl".into(),
                super::super::relation_catalog::table_acl_catalog_value(
                    &stored.role_owner,
                    stored.column_acls.get(&col.name),
                )?,
            );
            out.push(attribute);
        }
    }
    for (table_name, foreign_table) in catalog.foreign_tables() {
        let relid = crate::sql::foreign_table_relation_oid(&foreign_table);
        let security = catalog.foreign_table_security(&table_name)?;
        for (idx, column) in foreign_table.columns.into_iter().enumerate() {
            let mut attribute = pg_attribute_row(
                relid,
                catalog_ordinal(idx, "pg_attribute foreign-table column")?,
                &column,
            );
            attribute.insert(
                "attacl".into(),
                super::super::relation_catalog::table_acl_catalog_value(
                    &security.role_owner,
                    security.column_acls.get(&column.name),
                )?,
            );
            out.push(attribute);
        }
    }
    for (_, _, object_id, _) in catalog.sequences() {
        let relid = crate::sql::sequence_relation_oid(object_id);
        for (idx, column) in sequence_attribute_columns().iter().enumerate() {
            out.push(pg_attribute_row(
                relid,
                catalog_ordinal(idx, "pg_attribute sequence column")?,
                column,
            ));
        }
    }
    out.extend(super::super::ag_catalog::age_pg_attribute_rows(catalog)?);
    Ok(out)
}

fn sequence_attribute_columns() -> [SQLColumnDef; 3] {
    [
        sequence_attribute_column("last_value", ColumnType::BigInteger),
        sequence_attribute_column("log_cnt", ColumnType::BigInteger),
        sequence_attribute_column("is_called", ColumnType::Boolean),
    ]
}

fn sequence_attribute_column(name: &str, ty: ColumnType) -> SQLColumnDef {
    SQLColumnDef {
        name: name.into(),
        ty,
        object_id: None,
        missing_value: None,
        primary_key: false,
        not_null: true,
        not_null_explicit: true,
        not_null_name: None,
        not_null_validated: true,
        not_null_no_inherit: false,
        auto_increment: None,
        unique: false,
        default: None,
        generated: None,
        check: None,
        check_name: None,
        check_enforced: true,
        check_validated: true,
        check_no_inherit: false,
        references: None,
    }
}

pub(super) fn pg_attribute_row(relid: i64, attnum: i64, col: &SQLColumnDef) -> ResultRow {
    let missing_value = col.missing_value.clone().map_or(Value::Null, |value| {
        Value::Array(
            ArrayValue::try_new(vec![value])
                .expect("a single PostgreSQL missing value always forms a rectangular array"),
        )
    });
    row([
        ("attrelid", int_value(relid)),
        ("attname", str_value(col.name.clone())),
        ("atttypid", int_value(pg_type_oid(&col.ty))),
        ("attstattarget", int_value(-1)),
        ("attlen", int_value(pg_type_len(&col.ty))),
        ("attnum", int_value(attnum)),
        ("attndims", int_value(array_dimension_count(&col.ty))),
        ("atttypmod", int_value(pg_type_modifier(&col.ty))),
        ("attbyval", bool_value(pg_type_by_value(&col.ty))),
        ("attalign", str_value(pg_type_align(&col.ty))),
        ("attstorage", str_value(pg_type_storage(&col.ty))),
        ("attcompression", str_value("")),
        ("attnotnull", bool_value(col.not_null || col.primary_key)),
        (
            "atthasdef",
            bool_value(
                col.default.is_some()
                    || col.generated.is_some()
                    || col
                        .auto_increment
                        .as_ref()
                        .is_some_and(uqa_sql::ast::AutoIncrement::is_legacy),
            ),
        ),
        ("atthasmissing", bool_value(col.missing_value.is_some())),
        (
            "attidentity",
            str_value(match col.auto_increment.as_ref().map(|value| value.kind) {
                Some(uqa_sql::ast::AutoIncrementKind::IdentityAlways) => "a",
                Some(
                    uqa_sql::ast::AutoIncrementKind::IdentityByDefault
                    | uqa_sql::ast::AutoIncrementKind::Legacy,
                ) => "d",
                Some(uqa_sql::ast::AutoIncrementKind::Serial) | None => "",
            }),
        ),
        (
            "attgenerated",
            str_value(
                col.generated
                    .as_ref()
                    .map_or("", |generated| match generated.kind {
                        uqa_sql::ast::GeneratedColumnKind::Virtual => "v",
                        uqa_sql::ast::GeneratedColumnKind::Stored => "s",
                    }),
            ),
        ),
        ("attisdropped", bool_value(false)),
        ("attislocal", bool_value(true)),
        ("attinhcount", int_value(0)),
        ("attcollation", int_value(pg_type_collation_oid(&col.ty))),
        ("attacl", Value::Null),
        ("attoptions", Value::Null),
        ("attfdwoptions", Value::Null),
        ("attmissingval", missing_value),
    ])
}

pub(in crate::sql::catalog) fn build_pg_attrdef(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut out = Vec::new();
    for table_name in catalog.table_names() {
        let (_, table) = split_schema_name(&table_name)?;
        let relid = table_relation_oid_from(catalog, resolution, &table_name)?;
        let columns = &catalog
            .table(resolution, &table_name)?
            .ok_or_else(|| SQLError::UnknownTable(table_name.clone()))?
            .columns;
        append_pg_attrdef_rows(&mut out, &table_name, &table, relid, columns)?;
    }
    for (table_name, table) in catalog.foreign_tables() {
        let (_, local_name) = split_schema_name(&table_name)?;
        append_pg_attrdef_rows(
            &mut out,
            &table_name,
            &local_name,
            crate::sql::foreign_table_relation_oid(&table),
            &table.columns,
        )?;
    }
    Ok(out)
}

fn append_pg_attrdef_rows(
    out: &mut Vec<ResultRow>,
    table_name: &str,
    local_table_name: &str,
    relid: i64,
    columns: &[SQLColumnDef],
) -> Result<(), SQLError> {
    for (idx, col) in columns.iter().enumerate() {
        let legacy_auto_increment = col
            .auto_increment
            .as_ref()
            .is_some_and(uqa_sql::ast::AutoIncrement::is_legacy);
        if col.default.is_none() && !legacy_auto_increment && col.generated.is_none() {
            continue;
        }
        let default = if legacy_auto_increment {
            format!("nextval('{}_{}_seq')", local_table_name, col.name)
        } else if let Some(generated) = &col.generated {
            super::super::expression_text::schema_expr_text(&generated.expression)
        } else {
            value_to_text(&default_expr_text(col.default.as_ref()))
        };
        out.push(row([
            (
                "oid",
                int_value(stable_oid("attrdef", &format!("{table_name}.{}", col.name))),
            ),
            ("adrelid", int_value(relid)),
            (
                "adnum",
                int_value(catalog_ordinal(idx, "pg_attrdef column")?),
            ),
            ("adbin", str_value(default)),
        ]));
    }
    Ok(())
}
