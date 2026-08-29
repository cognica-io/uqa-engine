//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Virtual `pg_catalog` relation builders.

use super::helpers::{
    all_schema_names, array_dimension_count, bool_value, catalog_array, catalog_name,
    catalog_ordinal, catalog_usize, constraint_catalog_rows, current_user_name, current_user_oid,
    default_expr_text, index_columns, indexdef, int_value, pg_type_align, pg_type_array_oid,
    pg_type_by_value, pg_type_collation_oid, pg_type_element_oid, pg_type_len, pg_type_modifier,
    pg_type_oid, pg_type_routine_oids, pg_type_storage, pg_type_subscript_handler, relation_oid,
    row, schema_oid, split_index_name, split_schema_name, stable_object_oid, stable_oid, str_value,
    table_columns_for, view_columns_for, PgTypeRoutineOids,
};
use super::{value_to_text, ColumnType, Engine, ResultRow, SQLColumnDef, SQLError, Value};
use uqa_core::ArrayValue;
use uqa_sql::ast::RangeSubtype;
use uqa_sql::ast::RoleAttribute;

use crate::RelationIdentity;

pub(super) fn build_pg_tables(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
    let mut out: Vec<ResultRow> = Vec::new();
    let mut names = engine
        .query_table_names()
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
            (
                "hasrules",
                bool_value(engine.query_relation_has_rules(&name)?),
            ),
            (
                "hastriggers",
                bool_value(engine.query_relation_has_triggers(&name)?),
            ),
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

pub(in crate::sql) fn table_relation_oid(engine: &Engine, table: &str) -> Result<i64, SQLError> {
    let table_state = engine
        .try_query_table(table)
        .map_err(|error| SQLError::Internal(format!("resolve catalog table `{table}`: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    Ok(stable_object_oid("relation", &table_state.object_id()))
}

pub(in crate::sql) fn table_rowtype_oid(engine: &Engine, table: &str) -> Result<i64, SQLError> {
    let table_state = engine
        .try_query_table(table)
        .map_err(|error| SQLError::Internal(format!("resolve catalog table `{table}`: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    Ok(stable_object_oid("rowtype", &table_state.object_id()))
}

#[derive(Debug, Clone)]
pub(super) struct CatalogIndexRelation {
    pub(super) schema: String,
    pub(super) name: String,
    pub(super) table_name: String,
    pub(super) index_type: String,
    pub(super) columns: Vec<String>,
    pub(super) relkind: &'static str,
    pub(super) is_partition: bool,
    pub(super) has_children: bool,
    pub(super) parent_index_oid: Option<i64>,
}

impl CatalogIndexRelation {
    pub(super) fn oid(&self) -> i64 {
        relation_oid(self.relkind, &self.schema, &self.name)
    }
}

pub(super) fn catalog_index_relations(
    engine: &Engine,
) -> Result<Vec<CatalogIndexRelation>, SQLError> {
    let registered = engine
        .list_catalog_indexes()
        .map_err(|error| SQLError::Internal(format!("read index catalog: {error}")))?;
    let mut used = registered
        .iter()
        .map(|index| index.name.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let mut output = Vec::new();
    for index in registered {
        let (table_schema, _) = split_schema_name(&index.table_name)?;
        let (schema, name) = split_index_name(&index.name, &table_schema)?;
        let columns = index_columns(&index.columns_json)?;
        let hierarchy = engine
            .try_table_hierarchy(&index.table_name)
            .map_err(|error| {
                SQLError::Internal(format!("read indexed table hierarchy: {error}"))
            })?;
        let relkind = if hierarchy.partition_spec.is_some() {
            "I"
        } else {
            "i"
        };
        let has_children = relkind == "I"
            && !engine
                .direct_hierarchy_children(&index.table_name)?
                .is_empty();
        let root = CatalogIndexRelation {
            schema,
            name,
            table_name: index.table_name.clone(),
            index_type: index.index_type.clone(),
            columns: columns.clone(),
            relkind,
            is_partition: false,
            has_children,
            parent_index_oid: None,
        };
        let root_oid = root.oid();
        output.push(root);
        if relkind == "I" {
            append_partition_index_children(
                engine,
                &index.table_name,
                root_oid,
                &index.index_type,
                &columns,
                &mut used,
                &mut output,
            )?;
        }
    }
    Ok(output)
}

#[allow(clippy::too_many_arguments)]
fn append_partition_index_children(
    engine: &Engine,
    parent_table: &str,
    parent_index_oid: i64,
    index_type: &str,
    columns: &[String],
    used: &mut std::collections::BTreeSet<String>,
    output: &mut Vec<CatalogIndexRelation>,
) -> Result<(), SQLError> {
    for child in engine.direct_hierarchy_children(parent_table)? {
        let (schema, table) = split_schema_name(&child)?;
        let hierarchy = engine
            .try_table_hierarchy(&child)
            .map_err(|error| SQLError::Internal(format!("read partition hierarchy: {error}")))?;
        let relkind = if hierarchy.partition_spec.is_some() {
            "I"
        } else {
            "i"
        };
        let children = engine.direct_hierarchy_children(&child)?;
        let name = allocate_derived_index_name(&table, columns, used);
        let relation = CatalogIndexRelation {
            schema,
            name,
            table_name: child.clone(),
            index_type: index_type.to_string(),
            columns: columns.to_vec(),
            relkind,
            is_partition: true,
            has_children: !children.is_empty(),
            parent_index_oid: Some(parent_index_oid),
        };
        let relation_oid = relation.oid();
        output.push(relation);
        if relkind == "I" {
            append_partition_index_children(
                engine,
                &child,
                relation_oid,
                index_type,
                columns,
                used,
                output,
            )?;
        }
    }
    Ok(())
}

fn allocate_derived_index_name(
    table: &str,
    columns: &[String],
    used: &mut std::collections::BTreeSet<String>,
) -> String {
    fn component(raw: &str) -> String {
        let mut output = String::with_capacity(raw.len());
        let mut separator = false;
        for character in raw.chars() {
            if character.is_alphanumeric() || character == '_' {
                output.extend(character.to_lowercase());
                separator = false;
            } else if !separator && !output.is_empty() {
                output.push('_');
                separator = true;
            }
        }
        while output.ends_with('_') {
            output.pop();
        }
        output
    }

    let mut parts = std::iter::once(table)
        .chain(columns.iter().map(String::as_str))
        .map(component)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        parts.push("index".into());
    }
    let base = format!("{}_idx", parts.join("_"));
    if used.insert(base.clone()) {
        return base;
    }
    for suffix in 1_u64.. {
        let candidate = format!("{base}_{suffix}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("u64 index-name suffix space is non-empty")
}

pub(super) fn index_access_method_oid(method: &str) -> i64 {
    match method.to_ascii_lowercase().as_str() {
        "" | "btree" => 403,
        "hash" => 405,
        "gist" => 783,
        "gin" => 2_742,
        "spgist" => 4_000,
        "brin" => 3_580,
        _ => 0,
    }
}

pub(super) fn pg_class_row(
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

#[allow(clippy::too_many_arguments)]
pub(super) fn pg_class_row_with_lifecycle(
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

#[allow(clippy::too_many_arguments)]
pub(super) fn pg_class_catalog_row(
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
        .query_table_names()
        .map_err(|err| SQLError::Internal(format!("read table catalog: {err}")))?
    {
        let relid = table_relation_oid(engine, &table_name)?;
        let hierarchy = engine
            .try_table_hierarchy(&table_name)
            .map_err(|error| SQLError::Internal(format!("read table hierarchy: {error}")))?;
        let mut inherited_columns = Vec::new();
        for parent in &hierarchy.parents {
            inherited_columns.push(table_columns_for(engine, parent)?);
        }
        for (idx, col) in table_columns_for(engine, &table_name)?.iter().enumerate() {
            let inheritance_count = inherited_columns
                .iter()
                .filter(|columns| columns.iter().any(|parent| parent.name == col.name))
                .count();
            let mut attribute =
                pg_attribute_row(relid, catalog_ordinal(idx, "pg_attribute column")?, col);
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
    for view_name in engine.list_materialized_views()? {
        let (schema, view) = split_schema_name(&view_name)?;
        let relid = relation_oid("m", &schema, &view);
        for (idx, col) in view_columns_for(engine, &view_name)?.iter().enumerate() {
            out.push(pg_attribute_row(
                relid,
                catalog_ordinal(idx, "pg_attribute materialized-view column")?,
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
                object_id: None,
                missing_value: None,
                primary_key: false,
                not_null: false,
                not_null_explicit: false,
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

pub(super) fn build_pg_attrdef(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
    let mut out = Vec::new();
    for table_name in engine
        .query_table_names()
        .map_err(|err| SQLError::Internal(format!("read table catalog: {err}")))?
    {
        let (_, table) = split_schema_name(&table_name)?;
        let relid = table_relation_oid(engine, &table_name)?;
        for (idx, col) in table_columns_for(engine, &table_name)?.iter().enumerate() {
            let legacy_auto_increment = col
                .auto_increment
                .as_ref()
                .is_some_and(uqa_sql::ast::AutoIncrement::is_legacy);
            if col.default.is_none() && !legacy_auto_increment && col.generated.is_none() {
                continue;
            }
            let default = if legacy_auto_increment {
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
    let mut rows = constraint_catalog_rows(engine)?
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
            let constrained_relation_oid = table_relation_oid(
                engine,
                &format!(
                    "{}.{}",
                    uqa_sql::expr::quote_ident(&constraint.schema),
                    uqa_sql::expr::quote_ident(&constraint.table)
                ),
            )?;
            let referenced_relation_oid = match foreign_key {
                Some(foreign_key) => table_relation_oid(
                    engine,
                    &format!(
                        "{}.{}",
                        uqa_sql::expr::quote_ident(&foreign_key.schema),
                        uqa_sql::expr::quote_ident(&foreign_key.table)
                    ),
                )?,
                None => 0,
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
                ("condeferrable", bool_value(constraint.state.deferrable())),
                (
                    "condeferred",
                    bool_value(constraint.state.initially_deferred()),
                ),
                ("conenforced", bool_value(constraint.state.enforced())),
                ("convalidated", bool_value(constraint.state.validated())),
                ("conrelid", int_value(constrained_relation_oid)),
                ("contypid", int_value(0)),
                ("conindid", int_value(0)),
                ("conparentid", int_value(0)),
                ("confrelid", int_value(referenced_relation_oid)),
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
                ("connoinherit", bool_value(constraint.state.no_inherit())),
                ("conperiod", bool_value(constraint.period)),
                ("conkey", constrained_key),
                ("confkey", referenced_key),
                ("conpfeqop", Value::Null),
                ("conppeqop", Value::Null),
                ("conffeqop", Value::Null),
                ("conexclop", Value::Null),
                ("conbin", Value::Null),
            ]))
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    for (trigger, _) in super::events::catalog_triggers(engine)? {
        if !trigger.definition.constraint {
            continue;
        }
        let definition = &trigger.definition;
        let constraint_name = trigger
            .constraint_name
            .as_deref()
            .unwrap_or(&definition.name);
        let relation = RelationIdentity::from_legacy_name(&definition.table).map_err(|error| {
            SQLError::Internal(format!(
                "decode constraint-trigger relation `{}`: {error}",
                definition.table
            ))
        })?;
        rows.push(row([
            (
                "oid",
                int_value(super::events::trigger_constraint_catalog_oid(
                    engine, &trigger,
                )?),
            ),
            ("conname", str_value(constraint_name)),
            ("connamespace", int_value(schema_oid(&relation.schema))),
            ("contype", str_value("t")),
            (
                "condeferrable",
                bool_value(definition.deferrability.is_deferrable()),
            ),
            (
                "condeferred",
                bool_value(definition.deferrability.is_initially_deferred()),
            ),
            ("conenforced", bool_value(true)),
            ("convalidated", bool_value(true)),
            (
                "conrelid",
                int_value(table_relation_oid(engine, &definition.table)?),
            ),
            ("contypid", int_value(0)),
            ("conindid", int_value(0)),
            ("conparentid", int_value(0)),
            ("confrelid", int_value(0)),
            ("confupdtype", str_value(" ")),
            ("confdeltype", str_value(" ")),
            ("confmatchtype", str_value(" ")),
            ("conislocal", bool_value(true)),
            ("coninhcount", int_value(0)),
            ("connoinherit", bool_value(true)),
            ("conperiod", bool_value(false)),
            ("conkey", Value::Null),
            ("confkey", Value::Null),
            ("conpfeqop", Value::Null),
            ("conppeqop", Value::Null),
            ("conffeqop", Value::Null),
            ("conexclop", Value::Null),
            ("conbin", Value::Null),
        ]));
    }
    Ok(rows)
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
    for index in catalog_index_relations(engine)? {
        let table_cols = engine
            .table_columns(&index.table_name)
            .map_err(|err| SQLError::Internal(format!("read table schema: {err}")))?;
        let mut keys = Vec::with_capacity(index.columns.len());
        for column in &index.columns {
            if let Some(position) = table_cols.iter().position(|name| name == column) {
                keys.push(catalog_ordinal(position, "pg_index key column")?);
            }
        }
        let column_count = catalog_usize(index.columns.len(), "pg_index column count")?;
        rows.push(row([
            ("indexrelid", int_value(index.oid())),
            (
                "indrelid",
                int_value(table_relation_oid(engine, &index.table_name)?),
            ),
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

pub(super) fn build_pg_matviews(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = Vec::new();
    for name in engine.list_materialized_views()? {
        let (schema, matview) = split_schema_name(&name)?;
        let stored = engine.view_definition(&name)?.ok_or_else(|| {
            SQLError::Internal(format!(
                "materialized view `{name}` disappeared during pg_matviews scan"
            ))
        })?;
        rows.push(row([
            ("schemaname", str_value(schema)),
            ("matviewname", str_value(matview)),
            ("matviewowner", str_value(current_user_name())),
            ("tablespace", Value::Null),
            ("hasindexes", bool_value(false)),
            ("ispopulated", bool_value(stored.populated)),
            ("definition", str_value(format!("{:?}", stored.query))),
        ]));
    }
    Ok(rows)
}

pub(super) fn build_pg_indexes(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = Vec::new();
    for index in catalog_index_relations(engine)? {
        let (schema, table) = split_schema_name(&index.table_name)?;
        let qualified_table = format!(
            "{}.{}",
            uqa_sql::expr::quote_ident(&schema),
            uqa_sql::expr::quote_ident(&table)
        );
        let index_target = if index.relkind == "I" {
            format!("ONLY {qualified_table}")
        } else {
            qualified_table
        };
        rows.push(row([
            ("schemaname", str_value(schema)),
            ("tablename", str_value(table.clone())),
            ("indexname", str_value(index.name.clone())),
            ("tablespace", Value::Null),
            (
                "indexdef",
                str_value(indexdef(
                    &index.name,
                    &index.index_type,
                    &index_target,
                    &index.columns,
                )),
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
        (ColumnType::RefCursor, "U", false, "b"),
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
        (ColumnType::Range(RangeSubtype::Integer), "R", false, "r"),
        (ColumnType::Range(RangeSubtype::Numeric), "R", false, "r"),
        (ColumnType::Range(RangeSubtype::Timestamp), "R", false, "r"),
        (
            ColumnType::Range(RangeSubtype::TimestampTz),
            "R",
            false,
            "r",
        ),
        (ColumnType::Range(RangeSubtype::Date), "R", false, "r"),
        (ColumnType::Range(RangeSubtype::BigInteger), "R", false, "r"),
        (
            ColumnType::Multirange(RangeSubtype::Integer),
            "R",
            false,
            "m",
        ),
        (
            ColumnType::Multirange(RangeSubtype::Numeric),
            "R",
            false,
            "m",
        ),
        (
            ColumnType::Multirange(RangeSubtype::Timestamp),
            "R",
            false,
            "m",
        ),
        (
            ColumnType::Multirange(RangeSubtype::TimestampTz),
            "R",
            false,
            "m",
        ),
        (ColumnType::Multirange(RangeSubtype::Date), "R", false, "m"),
        (
            ColumnType::Multirange(RangeSubtype::BigInteger),
            "R",
            false,
            "m",
        ),
        (ColumnType::Vector(0), "U", false, "b"),
        (ColumnType::Tensor(0), "U", false, "b"),
    ];
    let mut types = catalog_types
        .iter()
        .cloned()
        .chain(
            catalog_types
                .iter()
                .filter(|&(ty, _, _, kind)| {
                    matches!(*kind, "b" | "r" | "m") && pg_type_array_oid(ty) != 0
                })
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

pub(super) fn build_pg_range() -> Vec<ResultRow> {
    [
        (RangeSubtype::Integer, 1_978, 3_914, 3_922),
        (RangeSubtype::Numeric, 3_125, 0, 3_924),
        (RangeSubtype::Timestamp, 3_128, 0, 3_929),
        (RangeSubtype::TimestampTz, 3_127, 0, 3_930),
        (RangeSubtype::Date, 3_122, 3_915, 3_925),
        (RangeSubtype::BigInteger, 3_124, 3_928, 3_923),
    ]
    .into_iter()
    .map(|(subtype, subtype_opclass, canonical, subtype_diff)| {
        row([
            (
                "rngtypid",
                int_value(pg_type_oid(&ColumnType::Range(subtype))),
            ),
            ("rngsubtype", int_value(pg_type_oid(&subtype.scalar_type()))),
            (
                "rngmultitypid",
                int_value(pg_type_oid(&ColumnType::Multirange(subtype))),
            ),
            ("rngcollation", int_value(0)),
            ("rngsubopc", int_value(subtype_opclass)),
            ("rngcanonical", int_value(canonical)),
            ("rngsubdiff", int_value(subtype_diff)),
        ])
    })
    .collect()
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

pub(super) fn build_pg_roles(engine: &Engine) -> Vec<ResultRow> {
    engine
        .roles_for_catalog()
        .into_iter()
        .map(|role| {
            row([
                ("oid", int_value(role.oid)),
                ("rolname", str_value(role.name.clone())),
                ("rolsuper", bool_value(role.has(RoleAttribute::Superuser))),
                ("rolinherit", bool_value(role.has(RoleAttribute::Inherit))),
                (
                    "rolcreaterole",
                    bool_value(role.has(RoleAttribute::CreateRole)),
                ),
                ("rolcreatedb", bool_value(role.has(RoleAttribute::CreateDb))),
                ("rolcanlogin", bool_value(role.has(RoleAttribute::Login))),
                (
                    "rolreplication",
                    bool_value(role.has(RoleAttribute::Replication)),
                ),
                ("rolconnlimit", int_value(i64::from(role.connection_limit))),
                ("rolpassword", str_value("********")),
                ("rolvaliduntil", Value::Null),
                (
                    "rolbypassrls",
                    bool_value(role.has(RoleAttribute::BypassRls)),
                ),
                ("rolconfig", Value::Null),
            ])
        })
        .collect()
}

pub(super) fn build_pg_user(engine: &Engine) -> Vec<ResultRow> {
    engine
        .roles_for_catalog()
        .into_iter()
        .filter(|role| role.has(RoleAttribute::Login))
        .map(|role| {
            row([
                ("usename", str_value(role.name.clone())),
                ("usesysid", int_value(role.oid)),
                ("usecreatedb", bool_value(role.has(RoleAttribute::CreateDb))),
                ("usesuper", bool_value(role.has(RoleAttribute::Superuser))),
                ("userepl", bool_value(role.has(RoleAttribute::Replication))),
                (
                    "usebypassrls",
                    bool_value(role.has(RoleAttribute::BypassRls)),
                ),
                ("passwd", str_value("********")),
                ("valuntil", Value::Null),
                ("useconfig", Value::Null),
            ])
        })
        .collect()
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
        (
            "default_transaction_isolation",
            "Client connection defaults",
        ),
        (
            "default_transaction_read_only",
            "Client connection defaults",
        ),
        (
            "default_transaction_deferrable",
            "Client connection defaults",
        ),
        ("transaction_isolation", "Client connection defaults"),
        ("transaction_read_only", "Client connection defaults"),
        ("transaction_deferrable", "Client connection defaults"),
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
