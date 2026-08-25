//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL inheritance and declarative-partitioning lowering.

use super::{compile_expr, range_var_name, Node, NodeEnum, Result, SQLError};
use crate::ast::{
    PartitionBound, PartitionRangeDatum, PartitionSpec, PartitionStrategy, TableHierarchy,
};

pub(in crate::compiler) fn compile_table_hierarchy(
    statement: &pg_query::protobuf::CreateStmt,
) -> Result<TableHierarchy> {
    let parents = statement
        .inh_relations
        .iter()
        .map(compile_parent)
        .collect::<Result<Vec<_>>>()?;
    let partition_spec = statement
        .partspec
        .as_ref()
        .map(compile_partition_spec)
        .transpose()?;
    let partition_bound = statement
        .partbound
        .as_ref()
        .map(compile_partition_bound)
        .transpose()?;
    if partition_bound.is_some() && parents.len() != 1 {
        return Err(SQLError::Internal(
            "PARTITION OF must identify exactly one parent relation".into(),
        ));
    }
    Ok(TableHierarchy {
        parents,
        partition_spec,
        partition_bound,
    })
}

fn compile_parent(node: &Node) -> Result<String> {
    match node.node.as_ref() {
        Some(NodeEnum::RangeVar(relation)) => Ok(range_var_name(relation)),
        other => Err(SQLError::Internal(format!(
            "CREATE TABLE inheritance parent has unexpected node {other:?}"
        ))),
    }
}

fn compile_partition_spec(raw: &pg_query::protobuf::PartitionSpec) -> Result<PartitionSpec> {
    let strategy = match raw.strategy() {
        pg_query::protobuf::PartitionStrategy::List => PartitionStrategy::List,
        pg_query::protobuf::PartitionStrategy::Range => PartitionStrategy::Range,
        pg_query::protobuf::PartitionStrategy::Hash => PartitionStrategy::Hash,
        other => {
            return Err(SQLError::Internal(format!(
                "CREATE TABLE has invalid partition strategy {other:?}"
            )))
        }
    };
    let keys = raw
        .part_params
        .iter()
        .map(|node| match node.node.as_ref() {
            Some(NodeEnum::PartitionElem(element)) if !element.name.is_empty() => {
                Ok(crate::ast::Expr::Column(element.name.clone()))
            }
            Some(NodeEnum::PartitionElem(element)) => element
                .expr
                .as_deref()
                .ok_or_else(|| {
                    SQLError::Internal("partition key has neither a column nor expression".into())
                })
                .and_then(compile_expr),
            other => Err(SQLError::Internal(format!(
                "partition key has unexpected node {other:?}"
            ))),
        })
        .collect::<Result<Vec<_>>>()?;
    if keys.is_empty() {
        return Err(SQLError::Internal(
            "partitioned table has no partition key".into(),
        ));
    }
    Ok(PartitionSpec { strategy, keys })
}

fn compile_partition_bound(raw: &pg_query::protobuf::PartitionBoundSpec) -> Result<PartitionBound> {
    if raw.is_default {
        return Ok(PartitionBound::Default);
    }
    match raw.strategy.as_str() {
        "l" => Ok(PartitionBound::List(
            raw.listdatums
                .iter()
                .map(compile_expr)
                .collect::<Result<Vec<_>>>()?,
        )),
        "r" => Ok(PartitionBound::Range {
            lower: raw
                .lowerdatums
                .iter()
                .map(compile_range_datum)
                .collect::<Result<Vec<_>>>()?,
            upper: raw
                .upperdatums
                .iter()
                .map(compile_range_datum)
                .collect::<Result<Vec<_>>>()?,
        }),
        "h" => {
            if raw.modulus <= 0 || raw.remainder < 0 || raw.remainder >= raw.modulus {
                return Err(SQLError::Internal(format!(
                    "invalid hash partition bound modulus {} remainder {}",
                    raw.modulus, raw.remainder
                )));
            }
            Ok(PartitionBound::Hash {
                modulus: raw.modulus,
                remainder: raw.remainder,
            })
        }
        other => Err(SQLError::Internal(format!(
            "partition bound has invalid strategy `{other}`"
        ))),
    }
}

fn compile_range_datum(node: &Node) -> Result<PartitionRangeDatum> {
    match node.node.as_ref() {
        Some(NodeEnum::PartitionRangeDatum(datum)) => match datum.kind() {
            pg_query::protobuf::PartitionRangeDatumKind::PartitionRangeDatumMinvalue => {
                Ok(PartitionRangeDatum::MinValue)
            }
            pg_query::protobuf::PartitionRangeDatumKind::PartitionRangeDatumMaxvalue => {
                Ok(PartitionRangeDatum::MaxValue)
            }
            pg_query::protobuf::PartitionRangeDatumKind::PartitionRangeDatumValue => datum
                .value
                .as_deref()
                .ok_or_else(|| SQLError::Internal("range partition VALUE is empty".into()))
                .and_then(compile_expr)
                .map(PartitionRangeDatum::Value),
            other => Err(SQLError::Internal(format!(
                "range partition datum has invalid kind {other:?}"
            ))),
        },
        Some(NodeEnum::ColumnRef(reference)) if reference.fields.len() == 1 => {
            let keyword = reference.fields[0]
                .node
                .as_ref()
                .and_then(|field| match field {
                    NodeEnum::String(value) => Some(value.sval.as_str()),
                    _ => None,
                });
            match keyword {
                Some(value) if value.eq_ignore_ascii_case("minvalue") => {
                    Ok(PartitionRangeDatum::MinValue)
                }
                Some(value) if value.eq_ignore_ascii_case("maxvalue") => {
                    Ok(PartitionRangeDatum::MaxValue)
                }
                _ => compile_expr(node).map(PartitionRangeDatum::Value),
            }
        }
        Some(_) => compile_expr(node).map(PartitionRangeDatum::Value),
        None => Err(SQLError::Internal(
            "range partition bound contains an empty node".into(),
        )),
    }
}
