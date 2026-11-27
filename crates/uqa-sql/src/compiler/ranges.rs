//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve range declaration options until their ordered catalog lookup.

use super::{
    domains::qualified_name, types::compile_pg_type_reference, NodeEnum, Result, SQLError,
};
use crate::ast::{CreateRange, RangeOption, RangeOptionValue};
use pg_query::protobuf::CreateRangeStmt;

pub(super) fn compile_create_range(statement: &CreateRangeStmt) -> Result<CreateRange> {
    let mut options = Vec::with_capacity(statement.params.len());
    for node in &statement.params {
        let Some(NodeEnum::DefElem(option)) = &node.node else {
            return Err(SQLError::Internal(
                "invalid range declaration option".into(),
            ));
        };
        let argument = option.arg.as_deref().and_then(|node| node.node.as_ref());
        let value = match (option.defname.as_str(), argument) {
            ("subtype", Some(NodeEnum::TypeName(ty))) => {
                RangeOptionValue::Type(compile_pg_type_reference(ty, "range subtype")?)
            }
            ("subtype", Some(NodeEnum::String(name))) => {
                RangeOptionValue::Type(crate::ColumnType::Named(name.sval.clone()))
            }
            (_, Some(NodeEnum::TypeName(ty))) => RangeOptionValue::Name(qualified_name(&ty.names)?),
            (_, Some(NodeEnum::List(list))) => RangeOptionValue::Name(qualified_name(&list.items)?),
            (_, Some(NodeEnum::String(name))) => {
                RangeOptionValue::Name(super::render_relation_component(&name.sval))
            }
            _ => RangeOptionValue::Invalid,
        };
        options.push(RangeOption {
            name: option.defname.clone(),
            value,
            position: super::source_positions::position(option.location),
        });
    }
    Ok(CreateRange {
        name: qualified_name(&statement.type_name)?,
        options,
    })
}
