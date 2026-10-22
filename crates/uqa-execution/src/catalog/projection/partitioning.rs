//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 declarative-partition catalog rendering and helpers.

use super::helpers::rows::{
    catalog_int2vector, catalog_oidvector, catalog_usize, int_value, row, str_value,
};
use super::helpers::type_metadata::pg_type_collation_oid;
use super::pg_catalog::table_relation_oid_from;
use super::view_definition::stored_expression_definition;
use crate::catalog::context::CatalogContext;
use crate::catalog::{CatalogReadView, RelationNameResolution};
use uqa_core::Value;
use uqa_sql::ast::{
    ColumnType, Expr, PartitionBound, PartitionRangeDatum, PartitionSpec, PartitionStrategy,
};
use uqa_sql::{ResultRow, SQLError};

#[cfg(test)]
mod tests;

pub fn build_pg_partitioned_table(
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = Vec::new();
    for table in catalog.table_names() {
        let table_snapshot = catalog
            .table(resolution, &table)?
            .ok_or_else(|| SQLError::UnknownTable(table.clone()))?;
        let hierarchy = &table_snapshot.hierarchy;
        let Some(spec) = hierarchy.partition_spec.as_ref() else {
            continue;
        };
        let columns = &table_snapshot.columns;
        let key_types = partition_key_types(context, spec, columns)?;
        let attributes = spec
            .keys
            .iter()
            .map(|key| match key {
                Expr::Column(name) => columns
                    .iter()
                    .position(|column| column.name == *name)
                    .map_or(Ok(0), |position| {
                        super::helpers::rows::column_ordinal(position, &columns[position])
                    }),
                _ => Ok(0),
            })
            .collect::<Result<Vec<_>, SQLError>>()?;
        let operator_classes = key_types
            .iter()
            .map(|ty| int_value(partition_operator_class(spec.strategy, ty)))
            .collect();
        let collations = key_types
            .iter()
            .map(|ty| int_value(pg_type_collation_oid(ty)))
            .collect();
        let expressions = spec
            .keys
            .iter()
            .filter(|key| !matches!(key, Expr::Column(_)))
            .map(|expression| {
                super::expression_nodes::encode(context, catalog, columns, None, expression, None)
                    .map(uqa_sql::catalog::node_tree::Field::from)
            })
            .collect::<Result<Vec<_>, SQLError>>()?;
        rows.push(row([
            (
                "partrelid",
                int_value(table_relation_oid_from(catalog, resolution, &table)?),
            ),
            (
                "partstrat",
                str_value(partition_strategy_code(spec.strategy)),
            ),
            (
                "partnatts",
                int_value(catalog_usize(
                    spec.keys.len(),
                    "pg_partitioned_table.partnatts",
                )?),
            ),
            (
                "partdefid",
                int_value(default_partition_oid(catalog, resolution, &table)?),
            ),
            (
                "partattrs",
                catalog_int2vector(
                    attributes.into_iter().map(Value::Int).collect(),
                    "pg_partitioned_table.partattrs",
                )?,
            ),
            (
                "partclass",
                catalog_oidvector(operator_classes, "pg_partitioned_table.partclass")?,
            ),
            (
                "partcollation",
                catalog_oidvector(collations, "pg_partitioned_table.partcollation")?,
            ),
            (
                "partexprs",
                if expressions.is_empty() {
                    Value::Null
                } else {
                    str_value(uqa_sql::catalog::node_tree::Field::List(expressions).to_string())
                },
            ),
        ]));
    }
    Ok(rows)
}

pub fn pg_get_expr_value(context: &CatalogContext<'_>, args: &[Value]) -> Result<Value, SQLError> {
    if !matches!(args.len(), 2 | 3) {
        return Err(SQLError::BadArity {
            name: "pg_get_expr".into(),
            expected: "2..=3".into(),
            actual: args.len(),
        });
    }
    if args.iter().any(|argument| matches!(argument, Value::Null)) {
        return Ok(Value::Null);
    }
    let node = match &args[0] {
        Value::Str(node) | Value::FixedChar(node) => node,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "pg_get_expr expression must be pg_node_tree, got {other:?}"
            )))
        }
    };
    let relation_oid = expect_oid("pg_get_expr", &args[1])?;
    if let Some(pretty) = args.get(2) {
        if !matches!(pretty, Value::Bool(_)) {
            return Err(SQLError::TypeMismatch(format!(
                "pg_get_expr pretty_bool must be boolean, got {pretty:?}"
            )));
        }
    }
    let node_tree = uqa_sql::catalog::node_tree::parse(node)?;
    uqa_sql::catalog::node_tree::deparse::validate_relation(&node_tree, relation_oid != 0)?;
    let catalog = context.catalog_read_view();
    let context = CatalogContext {
        catalog: &catalog,
        ..*context
    };
    let resolution = context.session_execution_view().relation_name_resolution();
    let relation = if relation_oid == 0 {
        None
    } else {
        let Some(identity) = super::regtypes::relation_identity_by_oid(&context, relation_oid)?
        else {
            return Ok(Value::Null);
        };
        Some(identity)
    };
    let pretty = matches!(args.get(2), Some(Value::Bool(true)));
    if let Some(identity) = relation.as_ref() {
        let name = identity.qualified_name();
        if let Some(table) = catalog.table(&resolution, &name)? {
            return super::expression_nodes::render_with_relation(
                &context,
                &table.columns,
                Some(&identity.name),
                &node_tree,
                pretty,
            )
            .map(str_value);
        }
        if let Some(table) = catalog.snapshot().definitions.foreign_tables.get(identity) {
            return super::expression_nodes::render_with_relation(
                &context,
                &table.columns,
                Some(&identity.name),
                &node_tree,
                pretty,
            )
            .map(str_value);
        }
        if let Some(view) = catalog.snapshot().definitions.views.get(identity) {
            let columns =
                super::helpers::views::view_columns_for(&context, &catalog, &resolution, view)?;
            return super::expression_nodes::render_with_relation(
                &context,
                &columns,
                Some(&identity.name),
                &node_tree,
                pretty,
            )
            .map(str_value);
        }
        if let Some(schema) =
            crate::catalog::schema::virtual_relation_schema(&catalog, &resolution, &name)?
        {
            let columns = schema
                .into_iter()
                .map(|(name, ty)| uqa_sql::ast::ColumnDef::nullable(name, ty))
                .collect::<Vec<_>>();
            return super::expression_nodes::render_with_relation(
                &context,
                &columns,
                Some(&identity.name),
                &node_tree,
                pretty,
            )
            .map(str_value);
        }
    }
    super::expression_nodes::render(&context, &[], &node_tree, pretty).map(str_value)
}

pub fn pg_get_partkeydef_value(
    context: &CatalogContext<'_>,
    args: &[Value],
) -> Result<Value, SQLError> {
    let [argument] = args else {
        return Err(SQLError::BadArity {
            name: "pg_get_partkeydef".into(),
            expected: "1".into(),
            actual: args.len(),
        });
    };
    if matches!(argument, Value::Null) {
        return Ok(Value::Null);
    }
    let relation_oid = expect_oid("pg_get_partkeydef", argument)?;
    let catalog = context.catalog_read_view();
    let resolution = context.session_execution_view().relation_name_resolution();
    for table in catalog.table_names() {
        if table_relation_oid_from(&catalog, &resolution, &table)? != relation_oid {
            continue;
        }
        let hierarchy = &catalog
            .table(&resolution, &table)?
            .ok_or_else(|| SQLError::UnknownTable(table.clone()))?
            .hierarchy;
        return hierarchy
            .partition_spec
            .as_ref()
            .map_or(Ok(Value::Null), |spec| {
                partition_key_definition(
                    Some(&crate::catalog::projection::CatalogOutput(*context)),
                    &catalog,
                    &resolution,
                    spec,
                )
                .map(str_value)
            });
    }
    Ok(Value::Null)
}

pub fn partition_bound_node(
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    table: &str,
    bound: &PartitionBound,
) -> Result<String, SQLError> {
    let hierarchy = &catalog
        .table(resolution, table)?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?
        .hierarchy;
    let strategy = hierarchy
        .parents
        .first()
        .map(|parent| {
            catalog
                .table(resolution, parent)?
                .ok_or_else(|| SQLError::UnknownTable(parent.clone()))?
                .hierarchy
                .partition_spec
                .as_ref()
                .map(|spec| spec.strategy)
                .ok_or_else(|| {
                    SQLError::Internal(format!("partition parent `{parent}` has no partition key"))
                })
        })
        .transpose()?
        .ok_or_else(|| SQLError::Internal(format!("partition `{table}` has no parent")))?;
    let key_types = hierarchy
        .parents
        .first()
        .map(|parent| partition_key_types_for_table(context, catalog, resolution, parent))
        .transpose()?
        .unwrap_or_default();
    let (is_default, modulus, remainder, listdatums, lowerdatums, upperdatums) = match bound {
        PartitionBound::Default => (true, 0, 0, "<>".into(), "<>".into(), "<>".into()),
        PartitionBound::List(values) => (
            false,
            0,
            0,
            format!(
                "({})",
                values
                    .iter()
                    .map(|value| partition_const_node(context, value, key_types.first()))
                    .collect::<Result<Vec<_>, _>>()?
                    .join(" ")
            ),
            "<>".into(),
            "<>".into(),
        ),
        PartitionBound::Range { lower, upper } => (
            false,
            0,
            0,
            "<>".into(),
            range_datum_nodes(context, lower, &key_types)?,
            range_datum_nodes(context, upper, &key_types)?,
        ),
        PartitionBound::Hash { modulus, remainder } => (
            false,
            *modulus,
            *remainder,
            "<>".into(),
            "<>".into(),
            "<>".into(),
        ),
    };
    Ok(format!(
        "{{PARTITIONBOUNDSPEC :strategy {} :is_default {is_default} :modulus {modulus} :remainder {remainder} :listdatums {listdatums} :lowerdatums {lowerdatums} :upperdatums {upperdatums} :location -1}}",
        partition_strategy_code(strategy)
    ))
}

/// Reconstruct a stored key for bound-coercion diagnostics with the caller's current catalog.
pub fn partition_key_expression(
    context: &CatalogContext<'_>,
    expression: &Expr,
) -> Result<String, SQLError> {
    stored_expression_definition(
        Some(&super::CatalogOutput(*context)),
        &context.catalog_read_view(),
        &context.session_execution_view().relation_name_resolution(),
        expression,
        false,
    )
}

/// The partition key as `pg_get_partkeydef` prints it.
pub fn partition_key_definition(
    output: Option<&dyn uqa_sql::expr::EngineHook>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    spec: &PartitionSpec,
) -> Result<String, SQLError> {
    let strategy = match spec.strategy {
        PartitionStrategy::List => "LIST",
        PartitionStrategy::Range => "RANGE",
        PartitionStrategy::Hash => "HASH",
    };
    let keys = partition_key_columns(output, catalog, resolution, spec, false)?;
    Ok(format!("{strategy} ({keys})"))
}

/// The keys of a partition key as `pg_get_partkeydef_columns` prints them: each column by its quoted name, each expression deparsed, in parentheses unless it reads as a function call. `pretty` drops the parentheses that precedence makes redundant, as an error detail prints them.
pub fn partition_key_columns(
    output: Option<&dyn uqa_sql::expr::EngineHook>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    spec: &PartitionSpec,
    pretty: bool,
) -> Result<String, SQLError> {
    spec.keys
        .iter()
        .map(|key| match key {
            Expr::Column(column) => Ok(uqa_sql::expr::quote_ident(column)),
            expression => {
                let text = super::view_definition::stored_expression_definition(
                    output, catalog, resolution, expression, pretty,
                )?;
                Ok(
                    if super::view_definition::stored_expression_prints_as_call(expression) {
                        text
                    } else {
                        format!("({text})")
                    },
                )
            }
        })
        .collect::<Result<Vec<_>, SQLError>>()
        .map(|keys| keys.join(", "))
}

/// The type of each key of the partition key of `table`.
pub fn partition_key_types_for_table(
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    table: &str,
) -> Result<Vec<ColumnType>, SQLError> {
    let table_snapshot = catalog
        .table(resolution, table)?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let hierarchy = &table_snapshot.hierarchy;
    let spec = hierarchy.partition_spec.as_ref().ok_or_else(|| {
        SQLError::Internal(format!("partitioned table `{table}` has no partition key"))
    })?;
    partition_key_types(context, spec, &table_snapshot.columns)
}

fn partition_key_types(
    context: &CatalogContext<'_>,
    spec: &PartitionSpec,
    columns: &[uqa_sql::ast::ColumnDef],
) -> Result<Vec<ColumnType>, SQLError> {
    let schema = crate::RowSchema::with_types(
        columns.iter().map(|column| column.name.clone()).collect(),
        columns
            .iter()
            .map(|column| Some(column.ty.clone()))
            .collect(),
    );
    spec.keys
        .iter()
        .map(|key| {
            if let Expr::Column(name) = key {
                return columns
                    .iter()
                    .find(|column| column.name == *name)
                    .map(|column| column.ty.clone())
                    .ok_or_else(|| SQLError::UnknownColumn(name.to_string()));
            }
            let expression = uqa_sql::plan::ExpressionPlan::lower(key.clone());
            crate::common_context_expression_type(
                &expression.scalar,
                &schema,
                &[],
                Some(context.routines),
            )?
            .ok_or_else(|| match partition_key_expression(context, key) {
                Ok(key) => SQLError::TypeMismatch(format!(
                    "cannot determine partition key type for `{key}`"
                )),
                Err(error) => error,
            })
        })
        .collect()
}

fn default_partition_oid(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    parent: &str,
) -> Result<i64, SQLError> {
    for child in catalog.direct_hierarchy_children(resolution, parent)? {
        let hierarchy = &catalog
            .table(resolution, &child)?
            .ok_or_else(|| SQLError::UnknownTable(child.clone()))?
            .hierarchy;
        if matches!(hierarchy.partition_bound, Some(PartitionBound::Default)) {
            return table_relation_oid_from(catalog, resolution, &child);
        }
    }
    Ok(0)
}

fn partition_strategy_code(strategy: PartitionStrategy) -> &'static str {
    match strategy {
        PartitionStrategy::List => "l",
        PartitionStrategy::Range => "r",
        PartitionStrategy::Hash => "h",
    }
}

fn range_datum_nodes(
    context: &CatalogContext<'_>,
    datums: &[PartitionRangeDatum],
    key_types: &[ColumnType],
) -> Result<String, SQLError> {
    if datums.len() != key_types.len() {
        return Err(SQLError::Internal(format!(
            "partition range bound width {} does not match key width {}",
            datums.len(),
            key_types.len()
        )));
    }
    Ok(format!(
        "({})",
        datums
            .iter()
            .zip(key_types)
            .map(|(datum, ty)| match datum {
                PartitionRangeDatum::MinValue =>
                    Ok("{PARTITIONRANGEDATUM :kind -1 :value <> :location -1}".into(),),
                PartitionRangeDatum::MaxValue =>
                    Ok("{PARTITIONRANGEDATUM :kind 1 :value <> :location -1}".into(),),
                PartitionRangeDatum::Value(value) => Ok(format!(
                    "{{PARTITIONRANGEDATUM :kind 0 :value {} :location -1}}",
                    partition_const_node(context, value, Some(ty))?
                )),
            })
            .collect::<Result<Vec<_>, SQLError>>()?
            .join(" ")
    ))
}

fn partition_const_node(
    context: &CatalogContext<'_>,
    expression: &Expr,
    ty: Option<&ColumnType>,
) -> Result<String, SQLError> {
    let ty = ty.ok_or_else(|| SQLError::Internal("partition bound has no key type".into()))?;
    let value = context.expressions.evaluate(expression)?;
    uqa_sql::catalog::node_tree::constant(&value, ty, None, context.routines.composite_types())
        .map(|node| node.to_string())
}

fn base_type(ty: &ColumnType) -> &ColumnType {
    match ty {
        ColumnType::Domain { base, .. } => base_type(base),
        other => other,
    }
}

fn partition_operator_class(strategy: PartitionStrategy, ty: &ColumnType) -> i64 {
    if strategy != PartitionStrategy::Hash {
        return uqa_sql::catalog::index::operator_classes::default_btree_operator_class(ty)
            .map_or(0, |class| i64::from(class.oid));
    }
    match base_type(ty) {
        ColumnType::Boolean => 10_048,
        ColumnType::Bytea => 10_049,
        ColumnType::InternalChar => 10_008,
        ColumnType::Name => 10_029,
        ColumnType::SmallInteger => 10_019,
        ColumnType::Integer => 10_020,
        ColumnType::BigInteger => 10_021,
        ColumnType::Oid
        | ColumnType::Regclass
        | ColumnType::Regcollation
        | ColumnType::Regnamespace
        | ColumnType::Regrole
        | ColumnType::Regtype => 10_031,
        ColumnType::Text | ColumnType::Varchar(_) => 10_037,
        ColumnType::Bpchar | ColumnType::Character(_) => 10_005,
        ColumnType::Real => 10_013,
        ColumnType::DoublePrecision => 10_014,
        ColumnType::Numeric { .. } => 10_030,
        ColumnType::Date => 10_011,
        ColumnType::Time | ColumnType::TimePrecision(_) => 10_039,
        ColumnType::TimeTz | ColumnType::TimeTzPrecision(_) => 10_042,
        ColumnType::Timestamp | ColumnType::TimestampPrecision(_) => 10_046,
        ColumnType::TimestampTz | ColumnType::TimestampTzPrecision(_) => 10_040,
        ColumnType::Interval | ColumnType::IntervalWithFields { .. } => 10_023,
        ColumnType::Uuid => 10_066,
        ColumnType::JsonB => 10_089,
        ColumnType::Enum(_) => 10_070,
        _ => 0,
    }
}

fn expect_oid(function: &str, value: &Value) -> Result<i64, SQLError> {
    match value {
        Value::Int(value) => Ok(*value),
        other => Err(SQLError::TypeMismatch(format!(
            "{function} relation must be oid, got {other:?}"
        ))),
    }
}
