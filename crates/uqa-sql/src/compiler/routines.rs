//! SQL/PLpgSQL routine creation, invocation, bodies, and drops.

use super::dispatch::compile_stmt;
use super::{
    compile_expr, compile_qualified_name, extract_string, Expr, Node, NodeEnum, Result, SQLError,
    Statement,
};

/// Last non-`pg_catalog` segment of a `TypeName`, lower-cased, with
/// `%TYPE` and array-bound suffixes preserved so the executor can
/// treat them as uncastable (best-effort) types.
pub(super) fn compile_function_type_name(t: &pg_query::protobuf::TypeName) -> Result<String> {
    let mut last = String::new();
    for n in &t.names {
        let name = extract_string(n)?;
        if name != "pg_catalog" {
            last = name;
        }
    }
    if last.is_empty() {
        return Err(SQLError::Internal(
            "function type has no name components".into(),
        ));
    }
    // `setof` is inspected separately by the caller; the name itself
    // stays scalar.
    let mut name = last.trim().to_ascii_lowercase();
    if t.pct_type {
        name.push_str("%type");
    }
    for _ in &t.array_bounds {
        name.push_str("[]");
    }
    Ok(name)
}

/// String payload of a `DefElem` argument.
fn def_elem_string(elem: &pg_query::protobuf::DefElem) -> Result<String> {
    match elem.arg.as_ref().and_then(|a| a.node.as_ref()) {
        Some(NodeEnum::String(s)) => Ok(s.sval.clone()),
        other => Err(SQLError::TypeMismatch(format!(
            "option `{}` expects a string, got {other:?}",
            elem.defname
        ))),
    }
}

pub(super) fn compile_create_function(
    stmt: &pg_query::protobuf::CreateFunctionStmt,
) -> Result<crate::ast::CreateFunction> {
    use crate::ast::{
        CreateFunction, FunctionBody, FunctionParam, FunctionParamMode, FunctionReturns,
        FunctionVolatility,
    };
    use pg_query::protobuf::FunctionParameterMode;

    let keyword = if stmt.is_procedure {
        "CREATE PROCEDURE"
    } else {
        "CREATE FUNCTION"
    };
    let name = compile_qualified_name(&stmt.funcname, keyword)?;

    let mut params: Vec<FunctionParam> = Vec::with_capacity(stmt.parameters.len());
    let mut has_table_param = false;
    for p in &stmt.parameters {
        let Some(NodeEnum::FunctionParameter(fp)) = p.node.as_ref() else {
            return Err(SQLError::Internal(format!(
                "{keyword}: malformed parameter"
            )));
        };
        let mode = match fp.mode() {
            FunctionParameterMode::FuncParamIn | FunctionParameterMode::FuncParamDefault => {
                FunctionParamMode::In
            }
            FunctionParameterMode::FuncParamOut => FunctionParamMode::Out,
            FunctionParameterMode::FuncParamInout => FunctionParamMode::InOut,
            FunctionParameterMode::FuncParamTable => {
                has_table_param = true;
                FunctionParamMode::Table
            }
            FunctionParameterMode::FuncParamVariadic => {
                return Err(SQLError::Unsupported(format!(
                    "{keyword}: VARIADIC parameters"
                )));
            }
            FunctionParameterMode::Undefined => {
                return Err(SQLError::Internal(format!(
                    "{keyword}: parameter mode missing"
                )));
            }
        };
        let type_name = fp
            .arg_type
            .as_ref()
            .map(compile_function_type_name)
            .transpose()?
            .ok_or_else(|| SQLError::Internal(format!("{keyword}: parameter without type")))?;
        let default = match fp.defexpr.as_ref() {
            Some(node) => Some(compile_expr(node)?),
            None => None,
        };
        params.push(FunctionParam {
            name: fp.name.to_ascii_lowercase(),
            type_name,
            mode,
            default,
        });
    }

    // Mirror PostgreSQL's parse-time rule: once an input parameter
    // has a DEFAULT, every following input parameter needs one too.
    let mut saw_default = false;
    for p in &params {
        if !matches!(p.mode, FunctionParamMode::In | FunctionParamMode::InOut) {
            continue;
        }
        if p.default.is_some() {
            saw_default = true;
        } else if saw_default {
            return Err(SQLError::Unsupported(
                "input parameters after one with a default value must also have defaults".into(),
            ));
        }
    }

    let returns = if has_table_param {
        FunctionReturns::Table
    } else {
        match stmt.return_type.as_ref() {
            None => FunctionReturns::None,
            Some(t) => {
                let type_name = compile_function_type_name(t)?;
                if t.setof {
                    FunctionReturns::SetOf { type_name }
                } else {
                    FunctionReturns::Scalar { type_name }
                }
            }
        }
    };

    let mut language = String::new();
    let mut volatility = FunctionVolatility::Volatile;
    let mut strict = false;
    let mut source: Option<String> = None;
    for opt in &stmt.options {
        let Some(NodeEnum::DefElem(elem)) = opt.node.as_ref() else {
            return Err(SQLError::Internal(format!("{keyword}: malformed option")));
        };
        match elem.defname.to_ascii_lowercase().as_str() {
            "language" => {
                language = def_elem_string(elem)?.to_ascii_lowercase();
            }
            "volatility" => {
                volatility = match def_elem_string(elem)?.as_str() {
                    "immutable" => FunctionVolatility::Immutable,
                    "stable" => FunctionVolatility::Stable,
                    "volatile" => FunctionVolatility::Volatile,
                    other => {
                        return Err(SQLError::TypeMismatch(format!(
                            "{keyword}: invalid volatility `{other}`"
                        )));
                    }
                };
            }
            "strict" => {
                strict = match elem.arg.as_ref().and_then(|a| a.node.as_ref()) {
                    Some(NodeEnum::Boolean(value)) => value.boolval,
                    other => {
                        return Err(SQLError::TypeMismatch(format!(
                            "{keyword}: STRICT expects a boolean, got {other:?}"
                        )));
                    }
                };
            }
            "as" => {
                let items: Vec<String> = match elem.arg.as_ref().and_then(|a| a.node.as_ref()) {
                    Some(NodeEnum::List(list)) => list
                        .items
                        .iter()
                        .map(extract_string)
                        .collect::<Result<Vec<_>>>()?,
                    Some(NodeEnum::String(s)) => vec![s.sval.clone()],
                    other => {
                        return Err(SQLError::TypeMismatch(format!(
                            "{keyword}: AS expects a string body, got {other:?}"
                        )));
                    }
                };
                match items.len() {
                    1 => source = items.into_iter().next(),
                    _ => {
                        return Err(SQLError::Unsupported(format!(
                            "{keyword}: AS 'obj_file', 'link_symbol' bodies"
                        )));
                    }
                }
            }
            "window" => {
                return Err(SQLError::Unsupported(format!(
                    "{keyword}: WINDOW functions"
                )));
            }
            // Planner / execution hints without engine semantics:
            // COST, ROWS, PARALLEL, SECURITY, LEAKPROOF, SET, SUPPORT.
            other => {
                return Err(SQLError::Unsupported(format!(
                    "{keyword}: option `{other}` is not supported"
                )));
            }
        }
    }

    let body = match (source, stmt.sql_body.as_deref()) {
        (Some(src), None) => FunctionBody::Source(src),
        (None, Some(node)) => FunctionBody::Statements(compile_sql_standard_body(node)?),
        (Some(_), Some(_)) => {
            return Err(SQLError::Unsupported(format!(
                "{keyword}: both AS body and SQL-standard body"
            )));
        }
        (None, None) => {
            return Err(SQLError::Unsupported(format!(
                "{keyword}: no function body"
            )));
        }
    };
    if language.is_empty() {
        if matches!(body, FunctionBody::Statements(_)) {
            language = "sql".into();
        } else {
            return Err(SQLError::Unsupported(format!(
                "{keyword}: no language specified"
            )));
        }
    }

    Ok(CreateFunction {
        name,
        or_replace: stmt.replace,
        is_procedure: stmt.is_procedure,
        params,
        returns,
        language,
        body,
        volatility,
        strict,
    })
}

/// Compile a SQL-standard function body (`RETURN expr` or
/// `BEGIN ATOMIC stmt; ... END`) into plain statements.
pub(super) fn compile_sql_standard_body(node: &Node) -> Result<Vec<Statement>> {
    let Some(inner) = node.node.as_ref() else {
        return Err(SQLError::Internal("empty SQL function body".into()));
    };
    match inner {
        NodeEnum::ReturnStmt(ret) => {
            let value = ret
                .returnval
                .as_deref()
                .ok_or_else(|| SQLError::Internal("RETURN without a value".into()))?;
            Ok(vec![select_of_expr(compile_expr(value)?)])
        }
        NodeEnum::List(list) => {
            let mut out = Vec::with_capacity(list.items.len());
            for item in &list.items {
                let item_inner = item.node.as_ref().ok_or_else(|| {
                    SQLError::Internal("SQL function body contains an empty statement".into())
                })?;
                match item_inner {
                    // BEGIN ATOMIC wraps each statement in a nested list.
                    NodeEnum::List(stmts) => {
                        for s in &stmts.items {
                            out.push(compile_stmt(s)?);
                        }
                    }
                    NodeEnum::ReturnStmt(ret) => {
                        let value = ret
                            .returnval
                            .as_deref()
                            .ok_or_else(|| SQLError::Internal("RETURN without a value".into()))?;
                        out.push(select_of_expr(compile_expr(value)?));
                    }
                    _ => out.push(compile_stmt(item)?),
                }
            }
            Ok(out)
        }
        other => Err(SQLError::Unsupported(format!(
            "SQL function body node {other:?}"
        ))),
    }
}

/// `SELECT <expr>` statement wrapping a single expression.
fn select_of_expr(expr: Expr) -> Statement {
    Statement::Select(Box::new(crate::ast::SelectStmt {
        projections: vec![crate::ast::Projection { expr, alias: None }],
        from: None,
        r#where: None,
        group_by: Vec::new(),
        grouping_sets: Vec::new(),
        having: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        with: Vec::new(),
        set_op: None,
        distinct: false,
        distinct_on: Vec::new(),
    }))
}

pub(super) fn compile_do(stmt: &pg_query::protobuf::DoStmt) -> Result<Statement> {
    let mut language = "plpgsql".to_string();
    let mut body: Option<String> = None;
    for arg in &stmt.args {
        let Some(NodeEnum::DefElem(elem)) = arg.node.as_ref() else {
            return Err(SQLError::Internal("DO contains a malformed option".into()));
        };
        match elem.defname.to_ascii_lowercase().as_str() {
            "as" => body = Some(def_elem_string(elem)?),
            "language" => {
                language = def_elem_string(elem)?.to_ascii_lowercase();
            }
            other => {
                return Err(SQLError::Unsupported(format!(
                    "DO option `{other}` is not supported"
                )));
            }
        }
    }
    let body = body.ok_or_else(|| SQLError::Internal("DO without a body".into()))?;
    Ok(Statement::DoBlock { language, body })
}

pub(super) fn compile_call(stmt: &pg_query::protobuf::CallStmt) -> Result<Statement> {
    let call = stmt
        .funccall
        .as_ref()
        .ok_or_else(|| SQLError::Internal("CALL without a function".into()))?;
    let name = compile_qualified_name(&call.funcname, "CALL")?;
    let args = call
        .args
        .iter()
        .map(compile_expr)
        .collect::<Result<Vec<_>>>()?;
    Ok(Statement::Call { name, args })
}

pub(super) fn compile_drop_function(
    stmt: &pg_query::protobuf::DropStmt,
    is_procedure: bool,
) -> Result<Statement> {
    use crate::ast::{DropFunctionItem, DropFunctionStmt};
    let mut items = Vec::new();
    for object in &stmt.objects {
        let Some(NodeEnum::ObjectWithArgs(owa)) = object.node.as_ref() else {
            return Err(SQLError::Unsupported(
                "DROP FUNCTION target is not a function signature".into(),
            ));
        };
        let name = compile_qualified_name(
            &owa.objname,
            if is_procedure {
                "DROP PROCEDURE"
            } else {
                "DROP FUNCTION"
            },
        )?;
        let arg_types = if owa.args_unspecified {
            None
        } else {
            Some(
                owa.objargs
                    .iter()
                    .map(|arg| match arg.node.as_ref() {
                        Some(NodeEnum::TypeName(t)) => compile_function_type_name(t),
                        other => Err(SQLError::Unsupported(format!(
                            "DROP FUNCTION argument type node {other:?}"
                        ))),
                    })
                    .collect::<Result<Vec<_>>>()?,
            )
        };
        items.push(DropFunctionItem { name, arg_types });
    }
    if items.is_empty() {
        return Err(SQLError::Internal("DROP FUNCTION without target".into()));
    }
    Ok(Statement::DropFunction(DropFunctionStmt {
        is_procedure,
        if_exists: stmt.missing_ok,
        cascade: matches!(
            stmt.behavior(),
            pg_query::protobuf::DropBehavior::DropCascade
        ),
        items,
    }))
}
