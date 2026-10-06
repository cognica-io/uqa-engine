//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar SQL RETURN bodies over the existing typed expression deparser.

use super::{expression, invalid, list, ExpressionNames, Field, SQLError};

/// Print a built-in routine's single scalar SELECT RETURN query without discarding query clauses.
pub fn return_body(value: &Field, names: &dyn ExpressionNames) -> Result<String, SQLError> {
    let Field::Node(query) = value else {
        return Err(invalid("expected a SQL RETURN query"));
    };
    if query.kind != "QUERY"
        || query.integer("commandType")? != 1
        || !query.boolean("isReturn")?
        || query.integer("resultRelation")? != 0
    {
        return Err(invalid("expected a scalar SELECT RETURN query"));
    }
    for field in [
        "cteList",
        "rtable",
        "groupClause",
        "groupingSets",
        "windowClause",
        "distinctClause",
        "sortClause",
        "rowMarks",
        "returningList",
    ] {
        if !list(query, field)?.is_empty() {
            return Err(invalid(format!(
                "unsupported {field} in scalar RETURN query"
            )));
        }
    }
    for field in [
        "utilityStmt",
        "onConflict",
        "havingQual",
        "limitOffset",
        "limitCount",
        "setOperations",
    ] {
        if query.field(field)? != &Field::Null {
            return Err(invalid(format!(
                "unsupported {field} in scalar RETURN query"
            )));
        }
    }
    let Field::Node(from) = query.field("jointree")? else {
        return Err(invalid("expected an empty FROMEXPR in scalar RETURN query"));
    };
    if from.kind != "FROMEXPR"
        || !list(from, "fromlist")?.is_empty()
        || from.field("quals")? != &Field::Null
    {
        return Err(invalid("expected an empty FROMEXPR in scalar RETURN query"));
    }
    let [Field::Node(target)] = list(query, "targetList")? else {
        return Err(invalid("expected one target in scalar RETURN query"));
    };
    if target.kind != "TARGETENTRY" || target.boolean("resjunk")? || target.integer("resno")? != 1 {
        return Err(invalid("expected one non-junk scalar RETURN target"));
    }
    let target_expression @ Field::Node(_) = target.field("expr")? else {
        return Err(invalid("expected a scalar RETURN expression node"));
    };
    expression(target_expression, names, false).map(|expression| format!("RETURN {expression}"))
}
