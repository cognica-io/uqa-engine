//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declared result identities shared by typed SQL reconstruction.

use super::{invalid, Field, Node, SQLError};

pub(super) fn result_type(node: &Node) -> Result<u32, SQLError> {
    let field = match node.kind.as_str() {
        "VAR" => "vartype",
        "PARAM" => "paramtype",
        "CONST" => "consttype",
        "COERCETODOMAINVALUE" | "CASETESTEXPR" => "typeId",
        "ROWEXPR" => "row_typeid",
        "FIELDSELECT" | "RELABELTYPE" | "COERCEVIAIO" | "COERCETODOMAIN" | "ARRAYCOERCEEXPR" => {
            "resulttype"
        }
        "SUBSCRIPTINGREF" => "refrestype",
        "FUNCEXPR" => "funcresulttype",
        "OPEXPR" | "NULLIFEXPR" => "opresulttype",
        "ARRAYEXPR" => "array_typeid",
        "BOOLEXPR" | "BOOLEANTEST" | "NULLTEST" | "DISTINCTEXPR" | "SCALARARRAYOPEXPR"
        | "ROWCOMPAREEXPR" => return Ok(16),
        "CASEEXPR" => "casetype",
        "COALESCEEXPR" => "coalescetype",
        "MINMAXEXPR" => "minmaxtype",
        "COLLATEEXPR" => {
            let Field::Node(inner) = node.field("arg")? else {
                return Err(invalid("invalid collated expression"));
            };
            return result_type(inner);
        }
        _ => return Err(invalid("unknown expression result type")),
    };
    u32::try_from(node.integer(field)?)
        .ok()
        .filter(|oid| *oid != 0)
        .ok_or_else(|| invalid("invalid expression result identity"))
}
