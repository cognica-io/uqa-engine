//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Virtual `pg_catalog` relation builders.

use super::helpers::{
    all_schema_names, array_dimension_count, bool_value, catalog_name, catalog_ordinal,
    catalog_usize, constraint_catalog_rows, current_user_name, current_user_oid, default_expr_text,
    index_columns, indexdef, int_value, list_int, pg_type_len, pg_type_modifier, pg_type_oid,
    relation_oid, routine_type_oid, row, schema_oid, split_index_name, split_schema_name,
    stable_oid, str_value, table_columns_for, PG18_BUILTIN_ROUTINES,
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
    let mut out = Vec::new();
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
        out.push(pg_class_row(&schema, &view, "v", 0, 0.0, false));
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
    row([
        ("oid", int_value(relation_oid(relkind, schema, name))),
        ("relname", str_value(name)),
        ("relnamespace", int_value(schema_oid(schema))),
        (
            "reltype",
            int_value(stable_oid("rowtype", &format!("{schema}.{name}"))),
        ),
        ("reloftype", int_value(0)),
        ("relowner", int_value(current_user_oid())),
        ("relam", int_value(0)),
        ("relfilenode", int_value(0)),
        ("reltablespace", int_value(0)),
        ("relpages", int_value(0)),
        ("reltuples", Value::Float(tuples)),
        ("relallvisible", int_value(0)),
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
        ("relreplident", str_value("d")),
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
    let mut out = Vec::new();
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
    for table_name in engine.list_foreign_tables().map_err(SQLError::Internal)? {
        let (schema, table) = split_schema_name(&table_name)?;
        let relid = relation_oid("f", &schema, &table);
        for (idx, col) in engine
            .foreign_table_columns(&table_name)
            .map_err(SQLError::Internal)?
            .iter()
            .enumerate()
        {
            let col = SQLColumnDef {
                name: col.clone(),
                ty: ColumnType::Text,
                primary_key: false,
                not_null: false,
                not_null_explicit: false,
                not_null_name: None,
                auto_increment: false,
                unique: false,
                default: None,
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
        ("attcacheoff", int_value(-1)),
        ("atttypmod", int_value(pg_type_modifier(&col.ty))),
        (
            "attbyval",
            bool_value(matches!(
                col.ty,
                ColumnType::Integer | ColumnType::Boolean | ColumnType::Real
            )),
        ),
        ("attalign", str_value("i")),
        ("attstorage", str_value("x")),
        ("attcompression", str_value("")),
        ("attnotnull", bool_value(col.not_null || col.primary_key)),
        (
            "atthasdef",
            bool_value(col.default.is_some() || col.auto_increment),
        ),
        ("atthasmissing", bool_value(false)),
        (
            "attidentity",
            str_value(if col.auto_increment { "d" } else { "" }),
        ),
        ("attgenerated", str_value("")),
        ("attisdropped", bool_value(false)),
        ("attislocal", bool_value(true)),
        ("attinhcount", int_value(0)),
        ("attcollation", int_value(0)),
        ("attacl", Value::Null),
        ("attoptions", Value::Null),
        ("attfdwoptions", Value::Null),
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
            if col.default.is_none() && !col.auto_increment {
                continue;
            }
            let default = if col.auto_increment {
                format!("nextval('{}_{}_seq')", table, col.name)
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
                ("adbin", str_value(default.clone())),
                ("adsrc", str_value(default)),
            ]));
        }
    }
    Ok(out)
}

pub(super) fn build_pg_constraint(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
    Ok(constraint_catalog_rows(engine)?
        .into_iter()
        .map(|constraint| {
            let foreign_key = constraint.foreign_key.as_ref();
            let constrained_key: Vec<i64> = constraint
                .columns
                .iter()
                .map(|column| column.table_ordinal)
                .collect();
            let constrained_key = if constrained_key.is_empty() {
                Value::Null
            } else {
                list_int(&constrained_key)
            };
            let referenced_key = foreign_key.map_or(Value::Null, |foreign_key| {
                list_int(&foreign_key.column_ordinals)
            });
            row([
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
            ])
        })
        .collect())
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
    let types = [
        (16_i64, "bool", 1_i64, "B"),
        (17, "bytea", -1, "U"),
        (20, "int8", 8, "N"),
        (21, "int2", 2, "N"),
        (23, "int4", 4, "N"),
        (25, "text", -1, "S"),
        (700, "float4", 4, "N"),
        (701, "float8", 8, "N"),
        (1043, "varchar", -1, "S"),
        (1082, "date", 4, "D"),
        (1083, "time", 8, "D"),
        (1114, "timestamp", 8, "D"),
        (1184, "timestamptz", 8, "D"),
        (1266, "timetz", 8, "D"),
        (114, "json", -1, "U"),
        (3802, "jsonb", -1, "U"),
        (1700, "numeric", -1, "N"),
        (2278, "void", 4, "P"),
        (380_000, "vector", -1, "U"),
    ];
    types
        .into_iter()
        .map(|(oid, name, len, category)| {
            row([
                ("oid", int_value(oid)),
                ("typname", str_value(name)),
                ("typnamespace", int_value(schema_oid("pg_catalog"))),
                ("typowner", int_value(current_user_oid())),
                ("typlen", int_value(len)),
                ("typbyval", bool_value(len > 0 && len <= 8)),
                ("typtype", str_value("b")),
                ("typcategory", str_value(category)),
                ("typispreferred", bool_value(false)),
                ("typisdefined", bool_value(true)),
                ("typdelim", str_value(",")),
                ("typrelid", int_value(0)),
                ("typsubscript", str_value("-")),
                ("typelem", int_value(0)),
                ("typarray", int_value(0)),
                ("typinput", int_value(0)),
                ("typoutput", int_value(0)),
                ("typreceive", int_value(0)),
                ("typsend", int_value(0)),
                ("typmodin", int_value(0)),
                ("typmodout", int_value(0)),
                ("typanalyze", int_value(0)),
                ("typalign", str_value("i")),
                ("typstorage", str_value("x")),
                ("typnotnull", bool_value(false)),
                ("typbasetype", int_value(0)),
                ("typtypmod", int_value(-1)),
                ("typndims", int_value(0)),
                ("typcollation", int_value(0)),
                ("typdefaultbin", Value::Null),
                ("typdefault", Value::Null),
                ("typacl", Value::Null),
            ])
        })
        .collect()
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
                        Value::List(
                            routine
                                .argument_names
                                .iter()
                                .map(|name| str_value(*name))
                                .collect(),
                        )
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
        let arg_names: Vec<Value> = def
            .params
            .iter()
            .map(|p| str_value(p.name.clone()))
            .collect();
        let arg_modes: Vec<Value> = def
            .params
            .iter()
            .map(|p| {
                str_value(match p.mode {
                    uqa_sql::ast::FunctionParamMode::In => "i",
                    uqa_sql::ast::FunctionParamMode::Out => "o",
                    uqa_sql::ast::FunctionParamMode::InOut => "b",
                    uqa_sql::ast::FunctionParamMode::Table => "t",
                })
            })
            .collect();
        let defaults = def.params.iter().filter(|p| p.default.is_some()).count();
        let argument_type_oids = def
            .signature_params()
            .iter()
            .map(|parameter| int_value(routine_type_oid(&parameter.type_name)))
            .collect::<Vec<_>>();
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
                int_value(catalog_usize(
                    def.signature_arity(),
                    "pg_proc argument count",
                )?),
            ),
            (
                "pronargdefaults",
                int_value(catalog_usize(defaults, "pg_proc default argument count")?),
            ),
            ("prorettype", int_value(25)),
            ("proargtypes", Value::List(argument_type_oids)),
            ("proallargtypes", Value::Null),
            ("proargmodes", Value::List(arg_modes)),
            ("proargnames", Value::List(arg_names)),
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
    engine
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
        .collect::<Result<Vec<_>, SQLError>>()
}
