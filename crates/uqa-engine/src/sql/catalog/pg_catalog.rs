//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Virtual `pg_catalog` relation builders.

use super::helpers::{
    all_schema_names, array_dimension_count, bool_value, catalog_array, catalog_name,
    catalog_ordinal, catalog_usize, constraint_catalog_rows, current_user_name, current_user_oid,
    default_expr_text, index_columns, indexdef, int_value, list_int, pg_type_align,
    pg_type_array_oid, pg_type_by_value, pg_type_collation_oid, pg_type_element_oid, pg_type_len,
    pg_type_modifier, pg_type_oid, pg_type_routine_oids, pg_type_storage,
    pg_type_subscript_handler, relation_oid, routine_type_oid, row, schema_oid, split_index_name,
    split_schema_name, stable_oid, str_value, table_columns_for, view_columns_for,
    PgTypeRoutineOids, PG18_BUILTIN_ROUTINES,
};
use super::{
    registered_names, routine_signature_types, value_to_text, ColumnType, Engine, ResultRow,
    SQLColumnDef, SQLError, Value,
};

pub(super) fn build_pg_tables(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
    let mut out: Vec<ResultRow> = Vec::new();
    let mut names = engine
        .table_names()
        .map_err(|err| SQLError::Internal(format!("read table catalog: {err}")))?;
    names.sort();
    for name in names {
        let (schema, table) = split_schema_name(&name)?;
        out.push(row([
            ("schemaname", str_value(schema.clone())),
            ("tablename", str_value(table)),
            ("tableowner", str_value(current_user_name())),
            ("tablespace", Value::Null),
            (
                "hasindexes",
                bool_value(
                    engine
                        .list_catalog_indexes()
                        .map_err(|err| SQLError::Internal(format!("read index catalog: {err}")))?
                        .iter()
                        .any(|idx| idx.table_name == name),
                ),
            ),
            ("hasrules", bool_value(false)),
            ("hastriggers", bool_value(false)),
            ("rowsecurity", bool_value(false)),
            ("table_catalog", catalog_name()),
            ("table_schema", str_value(schema)),
            ("table_type", str_value("BASE TABLE")),
        ]));
    }
    Ok(out)
}

pub(super) fn build_pg_namespace(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
    Ok(all_schema_names(engine)?
        .into_iter()
        .map(|schema| {
            row([
                ("oid", int_value(schema_oid(&schema))),
                ("nspname", str_value(schema)),
                ("nspowner", int_value(current_user_oid())),
                ("nspacl", Value::Null),
            ])
        })
        .collect())
}

pub(super) fn build_pg_class(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
    let mut out = vec![pg_class_catalog_row(
        13_313,
        13_315,
        "information_schema",
        "information_schema_catalog_name",
        "v",
        1,
        -1.0,
        false,
    )];
    for name in engine
        .table_names()
        .map_err(|err| SQLError::Internal(format!("read table catalog: {err}")))?
    {
        let (schema, table) = split_schema_name(&name)?;
        let columns = table_columns_for(engine, &name)?;
        out.push(pg_class_row(
            &schema,
            &table,
            "r",
            catalog_usize(columns.len(), "pg_class column count")?,
            engine.document_count(&name)? as f64,
            engine
                .list_catalog_indexes()
                .map_err(|err| SQLError::Internal(format!("read index catalog: {err}")))?
                .iter()
                .any(|idx| idx.table_name == name),
        ));
    }
    for name in engine.list_views()? {
        let (schema, view) = split_schema_name(&name)?;
        let columns = view_columns_for(engine, &name)?;
        out.push(pg_class_row(
            &schema,
            &view,
            "v",
            catalog_usize(columns.len(), "pg_class view column count")?,
            0.0,
            false,
        ));
    }
    for name in engine.list_foreign_tables().map_err(SQLError::Internal)? {
        let (schema, table) = split_schema_name(&name)?;
        out.push(pg_class_row(
            &schema,
            &table,
            "f",
            catalog_usize(
                engine
                    .foreign_table_columns(&name)
                    .map_err(SQLError::Internal)?
                    .len(),
                "pg_class foreign-table column count",
            )?,
            0.0,
            false,
        ));
    }
    for sequence in engine
        .list_sequences()
        .map_err(|err| SQLError::Internal(format!("read sequence catalog: {err}")))?
    {
        let (schema, name) = split_schema_name(&sequence)?;
        out.push(pg_class_row(&schema, &name, "S", 0, 0.0, false));
    }
    for idx in engine
        .list_catalog_indexes()
        .map_err(|err| SQLError::Internal(format!("read index catalog: {err}")))?
    {
        let (table_schema, _) = split_schema_name(&idx.table_name)?;
        let (schema, index_name) = split_index_name(&idx.name, &table_schema)?;
        out.push(pg_class_row(&schema, &index_name, "i", 0, 0.0, false));
    }
    out.extend(super::ag_catalog::age_pg_class_rows(engine)?);
    Ok(out)
}

pub(super) fn pg_class_row(
    schema: &str,
    name: &str,
    relkind: &str,
    natts: i64,
    tuples: f64,
    has_index: bool,
) -> ResultRow {
    let oid = relation_oid(relkind, schema, name);
    let reltype = if matches!(relkind, "r" | "v" | "m" | "c" | "f" | "p") {
        stable_oid("rowtype", &format!("{schema}.{name}"))
    } else {
        0
    };
    pg_class_catalog_row(
        oid, reltype, schema, name, relkind, natts, tuples, has_index,
    )
}

#[allow(clippy::too_many_arguments)]
fn pg_class_catalog_row(
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
        ("relhasrules", bool_value(relkind == "v")),
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

pub(super) fn build_pg_attribute(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
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
    for table_name in engine
        .table_names()
        .map_err(|err| SQLError::Internal(format!("read table catalog: {err}")))?
    {
        let (schema, table) = split_schema_name(&table_name)?;
        let relid = relation_oid("r", &schema, &table);
        for (idx, col) in table_columns_for(engine, &table_name)?.iter().enumerate() {
            out.push(pg_attribute_row(
                relid,
                catalog_ordinal(idx, "pg_attribute column")?,
                col,
            ));
        }
    }
    for view_name in engine.list_views()? {
        let (schema, view) = split_schema_name(&view_name)?;
        let relid = relation_oid("v", &schema, &view);
        for (idx, col) in view_columns_for(engine, &view_name)?.iter().enumerate() {
            out.push(pg_attribute_row(
                relid,
                catalog_ordinal(idx, "pg_attribute view column")?,
                col,
            ));
        }
    }
    for table_name in engine.list_foreign_tables().map_err(SQLError::Internal)? {
        let (schema, table) = split_schema_name(&table_name)?;
        let relid = relation_oid("f", &schema, &table);
        for (idx, (name, ty)) in engine
            .foreign_table_typed_columns(&table_name)
            .map_err(SQLError::Internal)?
            .into_iter()
            .enumerate()
        {
            let col = SQLColumnDef {
                name,
                ty,
                primary_key: false,
                not_null: false,
                not_null_explicit: false,
                not_null_name: None,
                auto_increment: false,
                unique: false,
                default: None,
                generated: None,
                check: None,
                check_name: None,
                check_enforced: true,
                references: None,
            };
            out.push(pg_attribute_row(
                relid,
                catalog_ordinal(idx, "pg_attribute foreign-table column")?,
                &col,
            ));
        }
    }
    out.extend(super::ag_catalog::age_pg_attribute_rows(engine)?);
    Ok(out)
}

pub(super) fn pg_attribute_row(relid: i64, attnum: i64, col: &SQLColumnDef) -> ResultRow {
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
            bool_value(col.default.is_some() || col.auto_increment || col.generated.is_some()),
        ),
        ("atthasmissing", bool_value(false)),
        (
            "attidentity",
            str_value(if col.auto_increment { "d" } else { "" }),
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
        ("attmissingval", Value::Null),
    ])
}

pub(super) fn build_pg_attrdef(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
    let mut out = Vec::new();
    for table_name in engine
        .table_names()
        .map_err(|err| SQLError::Internal(format!("read table catalog: {err}")))?
    {
        let (schema, table) = split_schema_name(&table_name)?;
        let relid = relation_oid("r", &schema, &table);
        for (idx, col) in table_columns_for(engine, &table_name)?.iter().enumerate() {
            if col.default.is_none() && !col.auto_increment && col.generated.is_none() {
                continue;
            }
            let default = if col.auto_increment {
                format!("nextval('{}_{}_seq')", table, col.name)
            } else if let Some(generated) = &col.generated {
                super::helpers::schema_expr_text(&generated.expression)
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
    }
    Ok(out)
}

pub(super) fn build_pg_constraint(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
    constraint_catalog_rows(engine)?
        .into_iter()
        .map(|constraint| -> Result<ResultRow, SQLError> {
            let foreign_key = constraint.foreign_key.as_ref();
            let constrained_key: Vec<i64> = constraint
                .columns
                .iter()
                .map(|column| column.table_ordinal)
                .collect();
            let constrained_key = if constrained_key.is_empty() {
                Value::Null
            } else {
                catalog_array(
                    constrained_key.into_iter().map(Value::Int).collect(),
                    "pg_constraint.conkey",
                )?
            };
            let referenced_key = match foreign_key {
                Some(foreign_key) => catalog_array(
                    foreign_key
                        .column_ordinals
                        .iter()
                        .copied()
                        .map(Value::Int)
                        .collect(),
                    "pg_constraint.confkey",
                )?,
                None => Value::Null,
            };
            Ok(row([
                (
                    "oid",
                    int_value(stable_oid(
                        "constraint",
                        &format!(
                            "{}.{}.{}",
                            constraint.schema, constraint.table, constraint.name
                        ),
                    )),
                ),
                ("conname", str_value(constraint.name)),
                ("connamespace", int_value(schema_oid(&constraint.schema))),
                ("contype", str_value(constraint.kind.pg_type())),
                ("condeferrable", bool_value(false)),
                ("condeferred", bool_value(false)),
                ("conenforced", bool_value(constraint.enforced)),
                ("convalidated", bool_value(constraint.enforced)),
                (
                    "conrelid",
                    int_value(relation_oid("r", &constraint.schema, &constraint.table)),
                ),
                ("contypid", int_value(0)),
                ("conindid", int_value(0)),
                ("conparentid", int_value(0)),
                (
                    "confrelid",
                    int_value(foreign_key.map_or(0, |foreign_key| {
                        relation_oid("r", &foreign_key.schema, &foreign_key.table)
                    })),
                ),
                (
                    "confupdtype",
                    str_value(foreign_key.map_or(" ", |foreign_key| {
                        foreign_key_action_code(foreign_key.on_update)
                    })),
                ),
                (
                    "confdeltype",
                    str_value(foreign_key.map_or(" ", |foreign_key| {
                        foreign_key_action_code(foreign_key.on_delete)
                    })),
                ),
                (
                    "confmatchtype",
                    str_value(foreign_key.map_or(" ", |foreign_key| {
                        foreign_key_match_code(foreign_key.match_type)
                    })),
                ),
                ("conislocal", bool_value(true)),
                ("coninhcount", int_value(0)),
                ("connoinherit", bool_value(constraint.kind.no_inherit())),
                ("conperiod", bool_value(false)),
                ("conkey", constrained_key),
                ("confkey", referenced_key),
                ("conpfeqop", Value::Null),
                ("conppeqop", Value::Null),
                ("conffeqop", Value::Null),
                ("conexclop", Value::Null),
                ("conbin", Value::Null),
            ]))
        })
        .collect()
}

const fn foreign_key_action_code(action: uqa_sql::ast::ForeignKeyAction) -> &'static str {
    match action {
        uqa_sql::ast::ForeignKeyAction::NoAction => "a",
        uqa_sql::ast::ForeignKeyAction::Restrict => "r",
        uqa_sql::ast::ForeignKeyAction::Cascade => "c",
        uqa_sql::ast::ForeignKeyAction::SetNull => "n",
        uqa_sql::ast::ForeignKeyAction::SetDefault => "d",
    }
}

const fn foreign_key_match_code(match_type: uqa_sql::ast::ForeignKeyMatch) -> &'static str {
    match match_type {
        uqa_sql::ast::ForeignKeyMatch::Simple => "s",
        uqa_sql::ast::ForeignKeyMatch::Full => "f",
    }
}

pub(super) fn build_pg_index(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = Vec::new();
    for idx in engine
        .list_catalog_indexes()
        .map_err(|err| SQLError::Internal(format!("read index catalog: {err}")))?
    {
        let columns = index_columns(&idx.columns_json)?;
        let (schema, table) = split_schema_name(&idx.table_name)?;
        let (index_schema, index_name) = split_index_name(&idx.name, &schema)?;
        let table_cols = engine
            .table_columns(&idx.table_name)
            .map_err(|err| SQLError::Internal(format!("read table schema: {err}")))?;
        let mut keys = Vec::with_capacity(columns.len());
        for column in &columns {
            if let Some(position) = table_cols.iter().position(|name| name == column) {
                keys.push(catalog_ordinal(position, "pg_index key column")?);
            }
        }
        let column_count = catalog_usize(columns.len(), "pg_index column count")?;
        rows.push(row([
            (
                "indexrelid",
                int_value(relation_oid("i", &index_schema, &index_name)),
            ),
            ("indrelid", int_value(relation_oid("r", &schema, &table))),
            ("indnatts", int_value(column_count)),
            ("indnkeyatts", int_value(column_count)),
            ("indisunique", bool_value(false)),
            ("indnullsnotdistinct", bool_value(false)),
            ("indisprimary", bool_value(false)),
            ("indisexclusion", bool_value(false)),
            ("indimmediate", bool_value(true)),
            ("indisclustered", bool_value(false)),
            ("indisvalid", bool_value(true)),
            ("indcheckxmin", bool_value(false)),
            ("indisready", bool_value(true)),
            ("indislive", bool_value(true)),
            ("indisreplident", bool_value(false)),
            (
                "indkey",
                Value::List(keys.into_iter().map(Value::Int).collect()),
            ),
            ("indcollation", Value::Null),
            ("indclass", Value::Null),
            ("indoption", Value::Null),
            ("indexprs", Value::Null),
            ("indpred", Value::Null),
        ]));
    }
    Ok(rows)
}

pub(super) fn build_pg_views(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = Vec::new();
    for name in engine.list_views()? {
        let (schema, view) = split_schema_name(&name)?;
        let definition = engine
            .view(&name)?
            .map_or_else(String::new, |stmt| format!("{stmt:?}"));
        rows.push(row([
            ("schemaname", str_value(schema)),
            ("viewname", str_value(view)),
            ("viewowner", str_value(current_user_name())),
            ("definition", str_value(definition)),
        ]));
    }
    Ok(rows)
}

pub(super) fn build_pg_indexes(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = Vec::new();
    for idx in engine
        .list_catalog_indexes()
        .map_err(|err| SQLError::Internal(format!("read index catalog: {err}")))?
    {
        let columns = index_columns(&idx.columns_json)?;
        let (schema, table) = split_schema_name(&idx.table_name)?;
        let (_, index_name) = split_index_name(&idx.name, &schema)?;
        rows.push(row([
            ("schemaname", str_value(schema)),
            ("tablename", str_value(table.clone())),
            ("indexname", str_value(index_name.clone())),
            ("tablespace", Value::Null),
            (
                "indexdef",
                str_value(indexdef(&index_name, &idx.index_type, &table, &columns)),
            ),
        ]));
    }
    Ok(rows)
}

pub(super) fn build_pg_type() -> Vec<ResultRow> {
    let catalog_types = [
        (ColumnType::Boolean, "B", true, "b"),
        (ColumnType::Bytea, "U", false, "b"),
        (ColumnType::InternalChar, "Z", false, "b"),
        (ColumnType::Name, "S", false, "b"),
        (ColumnType::BigInteger, "N", false, "b"),
        (ColumnType::Int2Vector, "A", false, "b"),
        (ColumnType::SmallInteger, "N", false, "b"),
        (ColumnType::Integer, "N", false, "b"),
        (ColumnType::Regproc, "N", false, "b"),
        (ColumnType::Regclass, "N", false, "b"),
        (ColumnType::Text, "S", true, "b"),
        (ColumnType::Oid, "N", true, "b"),
        (ColumnType::Xid, "U", false, "b"),
        (ColumnType::OidVector, "A", false, "b"),
        (ColumnType::Json, "U", false, "b"),
        (ColumnType::PgNodeTree, "Z", false, "b"),
        (ColumnType::Real, "N", false, "b"),
        (ColumnType::DoublePrecision, "N", true, "b"),
        (ColumnType::AclItem, "U", false, "b"),
        (ColumnType::Bpchar, "S", false, "b"),
        (ColumnType::Varchar(None), "S", false, "b"),
        (ColumnType::Date, "D", false, "b"),
        (ColumnType::Time, "D", false, "b"),
        (ColumnType::Timestamp, "D", false, "b"),
        (ColumnType::TimestampTz, "D", true, "b"),
        (ColumnType::Interval, "T", true, "b"),
        (ColumnType::TimeTz, "D", false, "b"),
        (
            ColumnType::Numeric {
                precision: None,
                scale: None,
            },
            "N",
            false,
            "b",
        ),
        (ColumnType::Regtype, "N", false, "b"),
        (ColumnType::Regnamespace, "N", false, "b"),
        (ColumnType::AnyArray, "P", false, "p"),
        (ColumnType::Uuid, "U", false, "b"),
        (ColumnType::JsonB, "U", false, "b"),
        (ColumnType::Vector(0), "U", false, "b"),
        (ColumnType::Tensor(0), "U", false, "b"),
    ];
    let mut types = catalog_types
        .iter()
        .cloned()
        .chain(
            catalog_types
                .iter()
                .filter(|&(ty, _, _, kind)| *kind == "b" && pg_type_array_oid(ty) != 0)
                .cloned()
                .map(|(ty, _, _, _)| (ColumnType::Array(Box::new(ty)), "A", false, "b")),
        )
        .map(|(ty, category, preferred, kind)| {
            pg_type_catalog_row(
                &ty,
                schema_oid("pg_catalog"),
                kind,
                category,
                preferred,
                0,
                -1,
            )
        })
        .collect::<Vec<_>>();
    for domain in super::schema::information_schema_domains() {
        let ColumnType::Domain { oid, base, .. } = &domain else {
            unreachable!("information schema type constructor returned a non-domain")
        };
        let (category, type_modifier) = match *oid {
            13_307 => ("N", -1),
            13_310 | 13_312 => ("S", -1),
            13_318 => ("D", 2),
            13_320 => ("S", 7),
            _ => unreachable!("unknown PostgreSQL 18 information schema domain {oid}"),
        };
        types.push(pg_type_catalog_row(
            &domain,
            schema_oid("information_schema"),
            "d",
            category,
            false,
            pg_type_oid(base),
            type_modifier,
        ));
        types.push(pg_type_catalog_row(
            &ColumnType::Array(Box::new(domain)),
            schema_oid("information_schema"),
            "b",
            "A",
            false,
            0,
            -1,
        ));
    }
    for domain in super::schema::ag_catalog_domains() {
        let ColumnType::Domain { name, base, .. } = &domain else {
            unreachable!("ag_catalog type constructor returned a non-domain")
        };
        let category = match name.as_str() {
            "label_id" => "N",
            "label_kind" => "Z",
            other => unreachable!("unknown ag_catalog domain {other}"),
        };
        types.push(pg_type_catalog_row(
            &domain,
            schema_oid(super::schema::AG_CATALOG_SCHEMA),
            "d",
            category,
            false,
            pg_type_oid(base),
            -1,
        ));
        types.push(pg_type_catalog_row(
            &ColumnType::Array(Box::new(domain)),
            schema_oid(super::schema::AG_CATALOG_SCHEMA),
            "b",
            "A",
            false,
            0,
            -1,
        ));
    }
    types.extend(super::ag_catalog::age_pg_type_rows());
    types.extend([
        special_pg_type_catalog_row(PgTypeCatalogMetadata {
            oid: 2249,
            name: "record".into(),
            namespace_oid: schema_oid("pg_catalog"),
            len: -1,
            by_value: false,
            kind: "p",
            category: "P",
            preferred: false,
            relation_oid: 0,
            subscript: "-",
            element_oid: 0,
            array_oid: 2287,
            routines: PgTypeRoutineOids {
                input: 2290,
                output: 2291,
                receive: 2402,
                send: 2403,
                modifier_input: 0,
                modifier_output: 0,
                analyze: 0,
            },
            align: "d",
            storage: "x",
            base_oid: 0,
            type_modifier: -1,
            collation_oid: 0,
        }),
        special_pg_type_catalog_row(PgTypeCatalogMetadata {
            oid: 2278,
            name: "void".into(),
            namespace_oid: schema_oid("pg_catalog"),
            len: 4,
            by_value: true,
            kind: "p",
            category: "P",
            preferred: false,
            relation_oid: 0,
            subscript: "-",
            element_oid: 0,
            array_oid: 0,
            routines: PgTypeRoutineOids {
                input: 2298,
                output: 2299,
                receive: 3120,
                send: 3121,
                modifier_input: 0,
                modifier_output: 0,
                analyze: 0,
            },
            align: "i",
            storage: "p",
            base_oid: 0,
            type_modifier: -1,
            collation_oid: 0,
        }),
        special_pg_type_catalog_row(PgTypeCatalogMetadata {
            oid: 2287,
            name: "_record".into(),
            namespace_oid: schema_oid("pg_catalog"),
            len: -1,
            by_value: false,
            kind: "p",
            category: "P",
            preferred: false,
            relation_oid: 0,
            subscript: "array_subscript_handler",
            element_oid: 2249,
            array_oid: 0,
            routines: PgTypeRoutineOids {
                input: 750,
                output: 751,
                receive: 2400,
                send: 2401,
                modifier_input: 0,
                modifier_output: 0,
                analyze: 3816,
            },
            align: "d",
            storage: "x",
            base_oid: 0,
            type_modifier: -1,
            collation_oid: 0,
        }),
        special_pg_type_catalog_row(PgTypeCatalogMetadata {
            oid: 13_314,
            name: "_information_schema_catalog_name".into(),
            namespace_oid: schema_oid("information_schema"),
            len: -1,
            by_value: false,
            kind: "b",
            category: "A",
            preferred: false,
            relation_oid: 0,
            subscript: "array_subscript_handler",
            element_oid: 13_315,
            array_oid: 0,
            routines: PgTypeRoutineOids {
                input: 750,
                output: 751,
                receive: 2400,
                send: 2401,
                modifier_input: 0,
                modifier_output: 0,
                analyze: 3816,
            },
            align: "d",
            storage: "x",
            base_oid: 0,
            type_modifier: -1,
            collation_oid: 0,
        }),
        special_pg_type_catalog_row(PgTypeCatalogMetadata {
            oid: 13_315,
            name: "information_schema_catalog_name".into(),
            namespace_oid: schema_oid("information_schema"),
            len: -1,
            by_value: false,
            kind: "c",
            category: "C",
            preferred: false,
            relation_oid: 13_313,
            subscript: "-",
            element_oid: 0,
            array_oid: 13_314,
            routines: PgTypeRoutineOids {
                input: 2290,
                output: 2291,
                receive: 2402,
                send: 2403,
                modifier_input: 0,
                modifier_output: 0,
                analyze: 0,
            },
            align: "d",
            storage: "x",
            base_oid: 0,
            type_modifier: -1,
            collation_oid: 0,
        }),
    ]);
    types.sort_by_key(|entry| match entry.get("oid") {
        Some(Value::Int(oid)) => *oid,
        _ => i64::MAX,
    });
    types
}

struct PgTypeCatalogMetadata<'a> {
    oid: i64,
    name: String,
    namespace_oid: i64,
    len: i64,
    by_value: bool,
    kind: &'a str,
    category: &'a str,
    preferred: bool,
    relation_oid: i64,
    subscript: &'a str,
    element_oid: i64,
    array_oid: i64,
    routines: PgTypeRoutineOids,
    align: &'a str,
    storage: &'a str,
    base_oid: i64,
    type_modifier: i64,
    collation_oid: i64,
}

fn pg_type_catalog_row(
    ty: &ColumnType,
    namespace_oid: i64,
    kind: &str,
    category: &str,
    preferred: bool,
    base_oid: i64,
    type_modifier: i64,
) -> ResultRow {
    special_pg_type_catalog_row(PgTypeCatalogMetadata {
        oid: pg_type_oid(ty),
        name: super::helpers::info_udt_name(ty),
        namespace_oid,
        len: pg_type_len(ty),
        by_value: pg_type_by_value(ty),
        kind,
        category,
        preferred,
        relation_oid: 0,
        subscript: pg_type_subscript_handler(ty),
        element_oid: pg_type_element_oid(ty),
        array_oid: pg_type_array_oid(ty),
        routines: pg_type_routine_oids(ty),
        align: pg_type_align(ty),
        storage: pg_type_storage(ty),
        base_oid,
        type_modifier,
        collation_oid: pg_type_collation_oid(ty),
    })
}

fn special_pg_type_catalog_row(metadata: PgTypeCatalogMetadata<'_>) -> ResultRow {
    row([
        ("oid", int_value(metadata.oid)),
        ("typname", str_value(metadata.name)),
        ("typnamespace", int_value(metadata.namespace_oid)),
        ("typowner", int_value(current_user_oid())),
        ("typlen", int_value(metadata.len)),
        ("typbyval", bool_value(metadata.by_value)),
        ("typtype", str_value(metadata.kind)),
        ("typcategory", str_value(metadata.category)),
        ("typispreferred", bool_value(metadata.preferred)),
        ("typisdefined", bool_value(true)),
        ("typdelim", str_value(",")),
        ("typrelid", int_value(metadata.relation_oid)),
        ("typsubscript", str_value(metadata.subscript)),
        ("typelem", int_value(metadata.element_oid)),
        ("typarray", int_value(metadata.array_oid)),
        ("typinput", int_value(metadata.routines.input)),
        ("typoutput", int_value(metadata.routines.output)),
        ("typreceive", int_value(metadata.routines.receive)),
        ("typsend", int_value(metadata.routines.send)),
        ("typmodin", int_value(metadata.routines.modifier_input)),
        ("typmodout", int_value(metadata.routines.modifier_output)),
        ("typanalyze", int_value(metadata.routines.analyze)),
        ("typalign", str_value(metadata.align)),
        ("typstorage", str_value(metadata.storage)),
        ("typnotnull", bool_value(false)),
        ("typbasetype", int_value(metadata.base_oid)),
        ("typtypmod", int_value(metadata.type_modifier)),
        ("typndims", int_value(0)),
        ("typcollation", int_value(metadata.collation_oid)),
        ("typdefaultbin", Value::Null),
        ("typdefault", Value::Null),
        ("typacl", Value::Null),
    ])
}

pub(super) fn build_pg_proc(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows: Vec<ResultRow> = PG18_BUILTIN_ROUTINES
        .iter()
        .map(|routine| {
            Ok(row([
                ("oid", int_value(routine.oid)),
                ("proname", str_value(routine.name)),
                ("pronamespace", int_value(schema_oid("pg_catalog"))),
                ("proowner", int_value(current_user_oid())),
                ("prolang", int_value(12)),
                ("procost", Value::Float(1.0)),
                ("prorows", Value::Float(0.0)),
                ("provariadic", int_value(0)),
                ("prosupport", str_value("-")),
                ("prokind", str_value(routine.kind)),
                ("prosecdef", bool_value(false)),
                ("proleakproof", bool_value(routine.leakproof)),
                ("proisstrict", bool_value(routine.strict)),
                ("proretset", bool_value(false)),
                ("provolatile", str_value(routine.volatility)),
                ("proparallel", str_value("s")),
                (
                    "pronargs",
                    int_value(catalog_usize(
                        routine.argument_types.len(),
                        "pg_proc built-in argument count",
                    )?),
                ),
                (
                    "pronargdefaults",
                    int_value(catalog_usize(
                        routine.default_arguments,
                        "pg_proc built-in default argument count",
                    )?),
                ),
                ("prorettype", int_value(routine.return_type)),
                ("proargtypes", list_int(routine.argument_types)),
                ("proallargtypes", Value::Null),
                ("proargmodes", Value::Null),
                (
                    "proargnames",
                    if routine.argument_names.is_empty() {
                        Value::Null
                    } else {
                        catalog_array(
                            routine
                                .argument_names
                                .iter()
                                .map(|name| str_value(*name))
                                .collect(),
                            "pg_proc.proargnames",
                        )?
                    },
                ),
                (
                    "proargdefaults",
                    routine.argument_defaults.map_or(Value::Null, str_value),
                ),
                ("protrftypes", Value::Null),
                ("prosrc", str_value(routine.source)),
                ("probin", Value::Null),
                ("prosqlbody", Value::Null),
                ("proconfig", Value::Null),
                ("proacl", Value::Null),
            ]))
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    rows.extend(registered_names().into_iter().map(|name| {
        row([
            ("oid", int_value(stable_oid("proc", name))),
            ("proname", str_value(name)),
            ("pronamespace", int_value(schema_oid("pg_catalog"))),
            ("proowner", int_value(current_user_oid())),
            ("prolang", int_value(0)),
            ("procost", Value::Float(1.0)),
            ("prorows", Value::Float(0.0)),
            ("provariadic", int_value(0)),
            ("prosupport", str_value("-")),
            ("prokind", str_value("f")),
            ("prosecdef", bool_value(false)),
            ("proleakproof", bool_value(false)),
            ("proisstrict", bool_value(false)),
            ("proretset", bool_value(false)),
            ("provolatile", str_value("s")),
            ("proparallel", str_value("s")),
            ("pronargs", int_value(0)),
            ("pronargdefaults", int_value(0)),
            ("prorettype", int_value(25)),
            ("proargtypes", Value::List(Vec::new())),
            ("proallargtypes", Value::Null),
            ("proargmodes", Value::Null),
            ("proargnames", Value::Null),
            ("proargdefaults", Value::Null),
            ("protrftypes", Value::Null),
            ("prosrc", str_value(name)),
            ("probin", Value::Null),
            ("prosqlbody", Value::Null),
            ("proconfig", Value::Null),
            ("proacl", Value::Null),
        ])
    }));
    for function in engine.list_sql_functions() {
        let def = &function.def;
        let (routine_schema, routine_name) = split_schema_name(&def.name)?;
        let signature = routine_signature_types(def);
        let identity = format!(
            "{}:{}:{}",
            def.name,
            if def.is_procedure {
                "procedure"
            } else {
                "function"
            },
            signature.join(",")
        );
        let source = match &def.body {
            uqa_sql::ast::FunctionBody::Source(source) => source.clone(),
            uqa_sql::ast::FunctionBody::Statements(_) => String::new(),
        };
        let volatile = match def.volatility {
            uqa_sql::ast::FunctionVolatility::Immutable => "i",
            uqa_sql::ast::FunctionVolatility::Stable => "s",
            uqa_sql::ast::FunctionVolatility::Volatile => "v",
        };
        let input_params = def
            .params
            .iter()
            .filter(|parameter| {
                matches!(
                    parameter.mode,
                    uqa_sql::ast::FunctionParamMode::In | uqa_sql::ast::FunctionParamMode::InOut
                )
            })
            .collect::<Vec<_>>();
        let defaults = input_params
            .iter()
            .filter(|parameter| parameter.default.is_some())
            .count();
        let argument_type_oids = input_params
            .iter()
            .map(|parameter| int_value(routine_type_oid(&parameter.type_name)))
            .collect::<Vec<_>>();
        let has_output_mode = def
            .params
            .iter()
            .any(|parameter| parameter.mode != uqa_sql::ast::FunctionParamMode::In);
        let all_argument_type_oids = if has_output_mode {
            catalog_array(
                def.params
                    .iter()
                    .map(|parameter| int_value(routine_type_oid(&parameter.type_name)))
                    .collect(),
                "pg_proc.proallargtypes",
            )?
        } else {
            Value::Null
        };
        let arg_modes = if has_output_mode {
            catalog_array(
                def.params
                    .iter()
                    .map(|parameter| {
                        str_value(match parameter.mode {
                            uqa_sql::ast::FunctionParamMode::In => "i",
                            uqa_sql::ast::FunctionParamMode::Out => "o",
                            uqa_sql::ast::FunctionParamMode::InOut => "b",
                            uqa_sql::ast::FunctionParamMode::Table => "t",
                        })
                    })
                    .collect(),
                "pg_proc.proargmodes",
            )?
        } else {
            Value::Null
        };
        let arg_names = if def
            .params
            .iter()
            .any(|parameter| !parameter.name.is_empty())
        {
            catalog_array(
                def.params
                    .iter()
                    .map(|parameter| str_value(parameter.name.clone()))
                    .collect(),
                "pg_proc.proargnames",
            )?
        } else {
            Value::Null
        };
        let return_type_oid = if def.is_procedure {
            if def.output_params().is_empty() {
                2278
            } else {
                2249
            }
        } else {
            match &def.returns {
                uqa_sql::ast::FunctionReturns::Scalar { type_name }
                | uqa_sql::ast::FunctionReturns::SetOf { type_name } => routine_type_oid(type_name),
                uqa_sql::ast::FunctionReturns::Table => 2249,
                uqa_sql::ast::FunctionReturns::None => match def.output_params().as_slice() {
                    [output] => routine_type_oid(&output.type_name),
                    [] => 2278,
                    _ => 2249,
                },
            }
        };
        rows.push(row([
            ("oid", int_value(stable_oid("proc", &identity))),
            ("proname", str_value(routine_name)),
            ("pronamespace", int_value(schema_oid(&routine_schema))),
            ("proowner", int_value(current_user_oid())),
            ("prolang", int_value(0)),
            ("procost", Value::Float(100.0)),
            (
                "prorows",
                Value::Float(if def.returns_set() { 1000.0 } else { 0.0 }),
            ),
            ("provariadic", int_value(0)),
            ("prosupport", str_value("-")),
            (
                "prokind",
                str_value(if def.is_procedure { "p" } else { "f" }),
            ),
            ("prosecdef", bool_value(false)),
            ("proleakproof", bool_value(false)),
            ("proisstrict", bool_value(def.strict)),
            ("proretset", bool_value(def.returns_set())),
            ("provolatile", str_value(volatile)),
            ("proparallel", str_value("u")),
            (
                "pronargs",
                int_value(catalog_usize(input_params.len(), "pg_proc argument count")?),
            ),
            (
                "pronargdefaults",
                int_value(catalog_usize(defaults, "pg_proc default argument count")?),
            ),
            ("prorettype", int_value(return_type_oid)),
            ("proargtypes", Value::List(argument_type_oids)),
            ("proallargtypes", all_argument_type_oids),
            ("proargmodes", arg_modes),
            ("proargnames", arg_names),
            ("proargdefaults", Value::Null),
            ("protrftypes", Value::Null),
            ("prosrc", str_value(source)),
            ("probin", Value::Null),
            ("prosqlbody", Value::Null),
            ("proconfig", Value::Null),
            ("proacl", Value::Null),
        ]));
    }
    Ok(rows)
}

pub(super) fn build_pg_database() -> Vec<ResultRow> {
    vec![row([
        ("oid", int_value(5)),
        ("datname", str_value("uqa")),
        ("datdba", int_value(current_user_oid())),
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
        ("datacl", Value::Null),
    ])]
}

pub(super) fn build_pg_roles() -> Vec<ResultRow> {
    vec![row([
        ("oid", int_value(current_user_oid())),
        ("rolname", str_value(current_user_name())),
        ("rolsuper", bool_value(true)),
        ("rolinherit", bool_value(true)),
        ("rolcreaterole", bool_value(true)),
        ("rolcreatedb", bool_value(true)),
        ("rolcanlogin", bool_value(true)),
        ("rolreplication", bool_value(false)),
        ("rolconnlimit", int_value(-1)),
        ("rolpassword", str_value("********")),
        ("rolvaliduntil", Value::Null),
        ("rolbypassrls", bool_value(true)),
        ("rolconfig", Value::Null),
    ])]
}

pub(super) fn build_pg_user() -> Vec<ResultRow> {
    vec![row([
        ("usename", str_value(current_user_name())),
        ("usesysid", int_value(current_user_oid())),
        ("usecreatedb", bool_value(true)),
        ("usesuper", bool_value(true)),
        ("userepl", bool_value(false)),
        ("usebypassrls", bool_value(true)),
        ("passwd", str_value("********")),
        ("valuntil", Value::Null),
        ("useconfig", Value::Null),
    ])]
}

pub(super) fn build_pg_settings(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
    let settings = [
        ("server_version", "Version and compatibility"),
        ("server_encoding", "Client connection defaults"),
        ("client_encoding", "Client connection defaults"),
        ("DateStyle", "Locale and formatting"),
        ("TimeZone", "Locale and formatting"),
        ("work_mem", "Resource usage"),
        ("search_path", "Client connection defaults"),
    ];
    settings
        .into_iter()
        .map(|(name, category)| {
            let setting = engine.show_variable(name)?;
            Ok(row([
                ("name", str_value(name)),
                ("setting", str_value(setting.as_str())),
                ("unit", Value::Null),
                ("category", str_value(category)),
                ("short_desc", str_value(name)),
                ("extra_desc", Value::Null),
                ("context", str_value("user")),
                ("vartype", str_value("string")),
                ("source", str_value("default")),
                ("min_val", Value::Null),
                ("max_val", Value::Null),
                ("enumvals", Value::Null),
                ("boot_val", str_value(setting.as_str())),
                ("reset_val", str_value(setting)),
                ("sourcefile", Value::Null),
                ("sourceline", Value::Null),
                ("pending_restart", bool_value(false)),
            ]))
        })
        .collect()
}

pub(super) fn build_pg_sequences(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = engine
        .list_sequences()
        .map_err(|err| SQLError::Internal(format!("read sequence catalog: {err}")))?
        .into_iter()
        .map(|name| {
            let (schema, sequence) = split_schema_name(&name)?;
            Ok(row([
                ("schemaname", str_value(schema)),
                ("sequencename", str_value(sequence)),
                ("sequenceowner", str_value(current_user_name())),
                ("data_type", str_value("bigint")),
                ("start_value", Value::Null),
                ("min_value", Value::Null),
                ("max_value", Value::Null),
                ("increment_by", Value::Null),
                ("cycle", bool_value(false)),
                ("cache_size", Value::Int(1)),
                ("last_value", Value::Null),
            ]))
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    rows.extend(super::ag_catalog::age_pg_sequences_rows(engine)?);
    Ok(rows)
}
