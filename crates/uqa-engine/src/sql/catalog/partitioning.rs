//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 declarative-partition catalog rendering and helpers.

use super::expression_text::schema_expr_text;
use super::helpers::rows::{catalog_usize, int_value, row, str_value};
use super::helpers::type_metadata::{
    pg_type_by_value, pg_type_collation_oid, pg_type_len, pg_type_modifier, pg_type_oid,
};
use super::pg_catalog::table_relation_oid_from;
use crate::engine_capabilities::{CatalogReadView, RelationNameResolution};
use crate::Engine;
use uqa_core::Value;
use uqa_sql::ast::{
    ColumnType, Expr, PartitionBound, PartitionRangeDatum, PartitionSpec, PartitionStrategy,
};
use uqa_sql::{ResultRow, SQLError};

pub(super) fn build_pg_partitioned_table(
    engine: &Engine,
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
        let key_types = partition_key_types(engine, spec, columns)?;
        let attributes = spec
            .keys
            .iter()
            .map(|key| match key {
                Expr::Column(name) => columns
                    .iter()
                    .position(|column| column.name == *name)
                    .map_or(Ok(0), |position| {
                        let ordinal = position.checked_add(1).ok_or_else(|| {
                            SQLError::Internal("partition key ordinal overflow".into())
                        })?;
                        catalog_usize(ordinal, "pg_partitioned_table.partattrs")
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
            .map(schema_expr_text)
            .collect::<Vec<_>>();
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
                Value::List(attributes.into_iter().map(Value::Int).collect()),
            ),
            ("partclass", Value::List(operator_classes)),
            ("partcollation", Value::List(collations)),
            (
                "partexprs",
                if expressions.is_empty() {
                    Value::Null
                } else {
                    str_value(format!(
                        "({})",
                        expressions
                            .iter()
                            .map(|expression| format!("{{UQAEXPR :sql {expression}}}"))
                            .collect::<Vec<_>>()
                            .join(" ")
                    ))
                },
            ),
        ]));
    }
    Ok(rows)
}

pub(in crate::sql) fn pg_get_expr_value(
    engine: &Engine,
    args: &[Value],
) -> Result<Value, SQLError> {
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
    let catalog = engine.catalog_read_view();
    let resolution = engine.session_execution_view().relation_name_resolution();
    for table in catalog.table_names() {
        if table_relation_oid_from(&catalog, &resolution, &table)? != relation_oid {
            continue;
        }
        let hierarchy = &catalog
            .table(&resolution, &table)?
            .ok_or_else(|| SQLError::UnknownTable(table.clone()))?
            .hierarchy;
        let Some(bound) = hierarchy.partition_bound.as_ref() else {
            return Ok(str_value(node.clone()));
        };
        let rendered_node = partition_bound_node(engine, &catalog, &resolution, &table, bound)?;
        if node == &rendered_node {
            return Ok(str_value(partition_bound_expression(bound)));
        }
        return Ok(str_value(node.clone()));
    }
    Ok(str_value(node.clone()))
}

pub(in crate::sql) fn pg_get_partkeydef_value(
    engine: &Engine,
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
    let catalog = engine.catalog_read_view();
    let resolution = engine.session_execution_view().relation_name_resolution();
    for table in catalog.table_names() {
        if table_relation_oid_from(&catalog, &resolution, &table)? != relation_oid {
            continue;
        }
        let hierarchy = &catalog
            .table(&resolution, &table)?
            .ok_or_else(|| SQLError::UnknownTable(table.clone()))?
            .hierarchy;
        return Ok(hierarchy
            .partition_spec
            .as_ref()
            .map_or(Value::Null, |spec| {
                str_value(partition_key_definition(spec))
            }));
    }
    Ok(Value::Null)
}

pub(super) fn partition_bound_node(
    engine: &Engine,
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
                .clone()
                .partition_spec
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
        .map(|parent| partition_key_types_for_table(engine, catalog, resolution, parent))
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
                    .map(|value| partition_const_node(engine, value, key_types.first()))
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
            range_datum_nodes(engine, lower, &key_types)?,
            range_datum_nodes(engine, upper, &key_types)?,
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

pub(super) fn partition_bound_expression(bound: &PartitionBound) -> String {
    match bound {
        PartitionBound::Default => "DEFAULT".into(),
        PartitionBound::List(values) => format!(
            "FOR VALUES IN ({})",
            values
                .iter()
                .map(schema_expr_text)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        PartitionBound::Range { lower, upper } => format!(
            "FOR VALUES FROM ({}) TO ({})",
            lower
                .iter()
                .map(partition_range_datum_text)
                .collect::<Vec<_>>()
                .join(", "),
            upper
                .iter()
                .map(partition_range_datum_text)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        PartitionBound::Hash { modulus, remainder } => {
            format!("FOR VALUES WITH (modulus {modulus}, remainder {remainder})")
        }
    }
}

pub(super) fn partition_key_definition(spec: &PartitionSpec) -> String {
    let strategy = match spec.strategy {
        PartitionStrategy::List => "LIST",
        PartitionStrategy::Range => "RANGE",
        PartitionStrategy::Hash => "HASH",
    };
    let keys = spec
        .keys
        .iter()
        .map(|key| match key {
            Expr::Column(column) => uqa_sql::expr::quote_ident(column),
            expression => schema_expr_text(expression),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{strategy} ({keys})")
}

fn partition_key_types_for_table(
    engine: &Engine,
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
    partition_key_types(engine, spec, &table_snapshot.columns)
}

fn partition_key_types(
    engine: &Engine,
    spec: &PartitionSpec,
    columns: &[uqa_sql::ast::ColumnDef],
) -> Result<Vec<ColumnType>, SQLError> {
    let schema = uqa_execution::RowSchema::with_types(
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
                    .ok_or_else(|| SQLError::UnknownColumn(name.clone()));
            }
            let expression = uqa_planner::ExpressionPlan::lower(key.clone());
            uqa_execution::common_context_expression_type(
                &expression.scalar,
                &schema,
                &[],
                Some(engine),
            )?
            .ok_or_else(|| {
                SQLError::TypeMismatch(format!(
                    "cannot determine partition key type for `{}`",
                    schema_expr_text(key)
                ))
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

fn partition_range_datum_text(datum: &PartitionRangeDatum) -> String {
    match datum {
        PartitionRangeDatum::MinValue => "MINVALUE".into(),
        PartitionRangeDatum::Value(value) => schema_expr_text(value),
        PartitionRangeDatum::MaxValue => "MAXVALUE".into(),
    }
}

fn range_datum_nodes(
    engine: &Engine,
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
                    partition_const_node(engine, value, Some(ty))?
                )),
            })
            .collect::<Result<Vec<_>, SQLError>>()?
            .join(" ")
    ))
}

fn partition_const_node(
    engine: &Engine,
    expression: &Expr,
    ty: Option<&ColumnType>,
) -> Result<String, SQLError> {
    let ty = ty.cloned().unwrap_or(ColumnType::Text);
    let value = crate::sql::scalar::eval_lowered_expression(engine, expression, None, &[])?;
    let type_oid = pg_type_oid(&ty);
    let type_modifier = pg_type_modifier(&ty);
    let collation_oid = pg_type_collation_oid(&ty);
    let length = pg_type_len(&ty);
    let by_value = pg_type_by_value(&ty);
    let const_value = pg_const_value(&value, &ty);
    Ok(format!(
        "{{CONST :consttype {type_oid} :consttypmod {type_modifier} :constcollid {collation_oid} :constlen {length} :constbyval {by_value} :constisnull {} :location -1 :constvalue {const_value}}}",
        matches!(value, Value::Null)
    ))
}

fn pg_const_value(value: &Value, ty: &ColumnType) -> String {
    if matches!(value, Value::Null) {
        return "<>".into();
    }
    let mut bytes = match (value, base_type(ty)) {
        (Value::Bool(value), ColumnType::Boolean) => vec![u8::from(*value)],
        (Value::Int(value), ColumnType::SmallInteger) => {
            i64::from(*value as i16).to_le_bytes().to_vec()
        }
        (Value::Int(value), ColumnType::Integer) => i64::from(*value as i32).to_le_bytes().to_vec(),
        (Value::Int(value), ColumnType::Oid | ColumnType::Xid) => {
            u64::from(*value as u32).to_le_bytes().to_vec()
        }
        (Value::Int(value), ColumnType::BigInteger) => value.to_le_bytes().to_vec(),
        (Value::Float(value), ColumnType::Real) => (*value as f32).to_le_bytes().to_vec(),
        (Value::Float(value), ColumnType::DoublePrecision) => value.to_le_bytes().to_vec(),
        (Value::Str(value) | Value::FixedChar(value), _) => varlena_bytes(value.as_bytes()),
        (Value::Bytes(value), ColumnType::Bytea) => varlena_bytes(value),
        _ => return format!("<{}>", schema_expr_text(&Expr::Literal(value.clone()))),
    };
    if pg_type_by_value(ty) {
        bytes.resize(8, 0);
        let width = pg_type_len(ty);
        format!(
            "{width} [ {} ]",
            bytes
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(" ")
        )
    } else {
        format!(
            "{} [ {} ]",
            bytes.len(),
            bytes
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(" ")
        )
    }
}

fn varlena_bytes(payload: &[u8]) -> Vec<u8> {
    let total = payload.len().saturating_add(4);
    let header = u32::try_from(total)
        .unwrap_or(u32::MAX)
        .saturating_mul(4)
        .to_le_bytes();
    header.into_iter().chain(payload.iter().copied()).collect()
}

fn base_type(ty: &ColumnType) -> &ColumnType {
    match ty {
        ColumnType::Domain { base, .. } => base_type(base),
        other => other,
    }
}

fn partition_operator_class(strategy: PartitionStrategy, ty: &ColumnType) -> i64 {
    let hash = strategy == PartitionStrategy::Hash;
    match base_type(ty) {
        ColumnType::Boolean => {
            if hash {
                10_048
            } else {
                10_003
            }
        }
        ColumnType::Bytea => {
            if hash {
                10_049
            } else {
                10_006
            }
        }
        ColumnType::InternalChar => {
            if hash {
                10_008
            } else {
                10_007
            }
        }
        ColumnType::Name => {
            if hash {
                10_029
            } else {
                10_028
            }
        }
        ColumnType::SmallInteger => {
            if hash {
                10_019
            } else {
                1_979
            }
        }
        ColumnType::Integer => {
            if hash {
                10_020
            } else {
                1_978
            }
        }
        ColumnType::BigInteger => {
            if hash {
                10_021
            } else {
                3_124
            }
        }
        ColumnType::Oid | ColumnType::Regclass | ColumnType::Regnamespace | ColumnType::Regtype => {
            if hash {
                10_031
            } else {
                1_981
            }
        }
        ColumnType::Text | ColumnType::Varchar(_) => {
            if hash {
                10_037
            } else {
                3_126
            }
        }
        ColumnType::Bpchar | ColumnType::Character(_) => {
            if hash {
                10_005
            } else {
                10_004
            }
        }
        ColumnType::Real => {
            if hash {
                10_013
            } else {
                10_012
            }
        }
        ColumnType::DoublePrecision => {
            if hash {
                10_014
            } else {
                3_123
            }
        }
        ColumnType::Numeric { .. } => {
            if hash {
                10_030
            } else {
                3_125
            }
        }
        ColumnType::Date => {
            if hash {
                10_011
            } else {
                3_122
            }
        }
        ColumnType::Time => {
            if hash {
                10_039
            } else {
                10_038
            }
        }
        ColumnType::TimeTz => {
            if hash {
                10_042
            } else {
                10_041
            }
        }
        ColumnType::Timestamp => {
            if hash {
                10_046
            } else {
                3_128
            }
        }
        ColumnType::TimestampTz => {
            if hash {
                10_040
            } else {
                3_127
            }
        }
        ColumnType::Interval => {
            if hash {
                10_023
            } else {
                10_022
            }
        }
        ColumnType::Uuid => {
            if hash {
                10_066
            } else {
                10_065
            }
        }
        ColumnType::JsonB => {
            if hash {
                10_089
            } else {
                10_088
            }
        }
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
