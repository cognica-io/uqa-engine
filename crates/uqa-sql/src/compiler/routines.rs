//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL/PLpgSQL routine creation, invocation, bodies, and drops.

use super::dispatch::compile_stmt;
use super::{
    compile_expr, compile_qualified_name, extract_string, render_relation_component, Expr, Node,
    NodeEnum, Result, SQLError, Statement,
};

struct CompiledFunctionTypeName {
    name: String,
    reference: Option<crate::ast::RoutineColumnTypeReference>,
}

/// Canonical spelling of a routine `TypeName`. A leading `pg_catalog`
/// qualifier is redundant, while relation qualification on `%TYPE` and
/// schema qualification on named types must survive compilation for catalog
/// resolution by the engine.
fn compile_function_type_name(
    t: &pg_query::protobuf::TypeName,
) -> Result<CompiledFunctionTypeName> {
    let mut components = t
        .names
        .iter()
        .map(extract_string)
        .collect::<Result<Vec<_>>>()?;
    if !t.pct_type
        && components
            .first()
            .is_some_and(|component| component.eq_ignore_ascii_case("pg_catalog"))
    {
        components.remove(0);
    }
    if components.is_empty() {
        return Err(SQLError::Internal(
            "function type has no name components".into(),
        ));
    }
    // `setof` is inspected separately by the caller; the name itself
    // stays scalar.
    let reference = if t.pct_type {
        let reference = match components.as_slice() {
            [relation, column] => {
                crate::ast::RoutineColumnTypeReference::new(None, relation.clone(), column.clone())
            }
            [schema, relation, column] => crate::ast::RoutineColumnTypeReference::new(
                Some(schema.clone()),
                relation.clone(),
                column.clone(),
            ),
            _ => {
                return Err(SQLError::TypeMismatch(
                    "%TYPE requires a relation and column reference".into(),
                ))
            }
        };
        Some(reference)
    } else {
        None
    };
    let mut name = components
        .iter()
        .map(|component| render_relation_component(component))
        .collect::<Vec<_>>()
        .join(".");
    if !t.pct_type && t.array_bounds.is_empty() && components.len() == 1 {
        if let Some(element) = crate::ast::builtin_array_element_name(&components[0]) {
            name = format!("{element}[]");
        }
    }
    if t.pct_type {
        name.push_str("%type");
    }
    for _ in &t.array_bounds {
        name.push_str("[]");
    }
    Ok(CompiledFunctionTypeName { name, reference })
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

fn def_elem_bool(elem: &pg_query::protobuf::DefElem, context: &str) -> Result<bool> {
    match elem
        .arg
        .as_ref()
        .and_then(|argument| argument.node.as_ref())
    {
        Some(NodeEnum::Boolean(value)) => Ok(value.boolval),
        other => Err(SQLError::TypeMismatch(format!(
            "{context} expects a boolean, got {other:?}"
        ))),
    }
}

fn compile_support_name(elem: &pg_query::protobuf::DefElem, context: &str) -> Result<String> {
    let Some(NodeEnum::List(list)) = elem
        .arg
        .as_ref()
        .and_then(|argument| argument.node.as_ref())
    else {
        return Err(SQLError::TypeMismatch(format!(
            "{context} SUPPORT expects a routine name"
        )));
    };
    compile_qualified_name(&list.items, context)
}

fn compile_routine_config_action(
    element: &pg_query::protobuf::DefElem,
    context: &str,
) -> Result<crate::ast::RoutineConfigAction> {
    use crate::ast::RoutineConfigAction;
    use pg_query::protobuf::VariableSetKind;

    let Some(NodeEnum::VariableSetStmt(setting)) = element
        .arg
        .as_ref()
        .and_then(|argument| argument.node.as_ref())
    else {
        return Err(SQLError::TypeMismatch(format!(
            "{context} SET expects a configuration action"
        )));
    };
    match setting.kind() {
        VariableSetKind::VarSetValue => {
            let Statement::SetVariable { name, value } =
                super::administrative::compile_variable_set(setting)?
            else {
                return Err(SQLError::Internal(
                    "routine SET did not compile as a variable assignment".into(),
                ));
            };
            Ok(RoutineConfigAction::Set { name, value })
        }
        VariableSetKind::VarSetDefault => Ok(RoutineConfigAction::Reset {
            name: setting.name.clone(),
        }),
        VariableSetKind::VarSetCurrent => Ok(RoutineConfigAction::FromCurrent {
            name: setting.name.clone(),
        }),
        VariableSetKind::VarReset => Ok(RoutineConfigAction::Reset {
            name: setting.name.clone(),
        }),
        VariableSetKind::VarResetAll => Ok(RoutineConfigAction::ResetAll),
        other => Err(SQLError::Unsupported(format!(
            "{context}: configuration action {other:?} is not supported"
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
            FunctionParameterMode::FuncParamVariadic => FunctionParamMode::Variadic,
            FunctionParameterMode::Undefined => {
                return Err(SQLError::Internal(format!(
                    "{keyword}: parameter mode missing"
                )));
            }
        };
        let compiled_type = fp
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
            // libpg_query has already folded unquoted identifiers while
            // preserving quoted identifiers. Keep that distinction: named
            // argument matching in PostgreSQL is case-sensitive after parse
            // analysis.
            name: fp.name.clone(),
            type_name: compiled_type.name,
            type_reference: compiled_type.reference,
            mode,
            default,
        });
    }

    // Mirror PostgreSQL's parse-time rule: once an input parameter
    // has a DEFAULT, every following input parameter needs one too.
    let mut saw_default = false;
    for p in &params {
        if !matches!(
            p.mode,
            FunctionParamMode::In | FunctionParamMode::InOut | FunctionParamMode::Variadic
        ) {
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

    let (returns, return_type_reference) = if has_table_param {
        (FunctionReturns::Table, None)
    } else {
        match stmt.return_type.as_ref() {
            None => (FunctionReturns::None, None),
            Some(t) => {
                let compiled = compile_function_type_name(t)?;
                let returns = if t.setof {
                    FunctionReturns::SetOf {
                        type_name: compiled.name,
                    }
                } else {
                    FunctionReturns::Scalar {
                        type_name: compiled.name,
                    }
                };
                (returns, compiled.reference)
            }
        }
    };

    let mut language = String::new();
    let mut volatility = FunctionVolatility::Volatile;
    let mut strict = false;
    let mut security_definer = false;
    let mut leakproof = false;
    let mut parallel = crate::ast::FunctionParallel::Unsafe;
    let mut support = None;
    let mut config_actions = Vec::new();
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
                strict = def_elem_bool(elem, &format!("{keyword}: STRICT"))?;
            }
            "security" => {
                security_definer = def_elem_bool(elem, &format!("{keyword}: SECURITY"))?;
            }
            "leakproof" => {
                leakproof = def_elem_bool(elem, &format!("{keyword}: LEAKPROOF"))?;
            }
            "parallel" => {
                parallel = match def_elem_string(elem)?.as_str() {
                    "unsafe" => crate::ast::FunctionParallel::Unsafe,
                    "restricted" => crate::ast::FunctionParallel::Restricted,
                    "safe" => crate::ast::FunctionParallel::Safe,
                    other => {
                        return Err(SQLError::TypeMismatch(format!(
                            "{keyword}: invalid PARALLEL value `{other}`"
                        )))
                    }
                };
            }
            "support" => support = Some(compile_support_name(elem, keyword)?),
            "set" => config_actions.push(compile_routine_config_action(elem, keyword)?),
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
            // Planner / execution hints outside this routine contract: COST and ROWS.
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
        return_type_reference,
        language,
        body,
        creation_search_path: Vec::new(),
        volatility,
        strict,
        owner: String::new(),
        security: crate::ast::RoutineSecurityAttributes {
            security_definer,
            leakproof,
        },
        parallel,
        support,
        config: Vec::new(),
        config_actions,
        execute_acl: None,
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
        values: Vec::new(),
        from: None,
        r#where: None,
        group_by: Vec::new(),
        grouping_sets: Vec::new(),
        group_distinct: false,
        having: None,
        order_by: Vec::new(),
        limit: None,
        with_ties: false,
        offset: None,
        with: Vec::new(),
        set_op: None,
        distinct: false,
        distinct_on: Vec::new(),
        locking: Vec::new(),
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
    crate::expr::validate_named_argument_order(call.args.iter().map(|argument| {
        match argument.node.as_ref() {
            Some(NodeEnum::NamedArgExpr(argument)) => Some(argument.name.as_str()),
            _ => None,
        }
    }))?;
    let mut args = call
        .args
        .iter()
        .map(compile_expr)
        .collect::<Result<Vec<_>>>()?;
    if call.func_variadic {
        let argument = args.pop().ok_or_else(|| {
            SQLError::Internal(format!("VARIADIC invocation of `{name}` has no argument"))
        })?;
        args.push(crate::expr::wrap_variadic_argument(argument));
    }
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
                        Some(NodeEnum::TypeName(t)) => {
                            compile_function_type_name(t).map(|compiled| compiled.name)
                        }
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

pub(super) fn compile_alter_routine(
    stmt: &pg_query::protobuf::AlterFunctionStmt,
) -> Result<crate::ast::AlterRoutineStmt> {
    use crate::ast::{AlterRoutineKind, AlterRoutineStmt, FunctionParallel, FunctionVolatility};
    use pg_query::protobuf::ObjectType;

    let (kind, keyword) = match stmt.objtype() {
        ObjectType::ObjectFunction => (AlterRoutineKind::Function, "ALTER FUNCTION"),
        ObjectType::ObjectProcedure => (AlterRoutineKind::Procedure, "ALTER PROCEDURE"),
        ObjectType::ObjectRoutine => (AlterRoutineKind::Routine, "ALTER ROUTINE"),
        other => {
            return Err(SQLError::Unsupported(format!(
                "ALTER routine target {other:?} is not supported"
            )))
        }
    };
    let target = stmt
        .func
        .as_ref()
        .ok_or_else(|| SQLError::Internal(format!("{keyword} without a target")))?;
    let name = compile_qualified_name(&target.objname, keyword)?;
    let (arg_types, mut arg_type_references) = if target.args_unspecified {
        (None, Vec::new())
    } else {
        let mut arg_types = Vec::with_capacity(target.objargs.len());
        let mut references = Vec::with_capacity(target.objargs.len());
        for argument in &target.objargs {
            let Some(NodeEnum::TypeName(type_name)) = argument.node.as_ref() else {
                return Err(SQLError::Unsupported(format!(
                    "{keyword}: malformed argument type node {:?}",
                    argument.node
                )));
            };
            let compiled = compile_function_type_name(type_name)?;
            arg_types.push(compiled.name);
            references.push(compiled.reference);
        }
        (Some(arg_types), references)
    };
    if arg_type_references.iter().all(Option::is_none) {
        arg_type_references.clear();
    }

    let mut volatility = None;
    let mut strict = None;
    let mut security_definer = None;
    let mut leakproof = None;
    let mut parallel = None;
    let mut support = None;
    let mut config_actions = Vec::new();
    for action in &stmt.actions {
        let Some(NodeEnum::DefElem(element)) = action.node.as_ref() else {
            return Err(SQLError::Unsupported(format!(
                "{keyword}: malformed action node {:?}",
                action.node
            )));
        };
        match element.defname.to_ascii_lowercase().as_str() {
            "volatility" => {
                if volatility.is_some() {
                    return Err(SQLError::Routine {
                        sqlstate: "42601".into(),
                        message: format!("{keyword}: conflicting or redundant volatility option"),
                    });
                }
                volatility = Some(match def_elem_string(element)?.as_str() {
                    "immutable" => FunctionVolatility::Immutable,
                    "stable" => FunctionVolatility::Stable,
                    "volatile" => FunctionVolatility::Volatile,
                    other => {
                        return Err(SQLError::TypeMismatch(format!(
                            "{keyword}: invalid volatility `{other}`"
                        )))
                    }
                });
            }
            "strict" => {
                if strict.is_some() {
                    return Err(SQLError::Routine {
                        sqlstate: "42601".into(),
                        message: format!("{keyword}: conflicting or redundant null-input option"),
                    });
                }
                strict = Some(
                    match element.arg.as_ref().and_then(|arg| arg.node.as_ref()) {
                        Some(NodeEnum::Boolean(value)) => value.boolval,
                        other => {
                            return Err(SQLError::TypeMismatch(format!(
                                "{keyword}: null-input option expects a boolean, got {other:?}"
                            )))
                        }
                    },
                );
            }
            "security" => {
                if security_definer.is_some() {
                    return Err(SQLError::Routine {
                        sqlstate: "42601".into(),
                        message: format!("{keyword}: conflicting or redundant security option"),
                    });
                }
                security_definer = Some(def_elem_bool(element, &format!("{keyword}: SECURITY"))?);
            }
            "leakproof" => {
                if leakproof.is_some() {
                    return Err(SQLError::Routine {
                        sqlstate: "42601".into(),
                        message: format!("{keyword}: conflicting or redundant leakproof option"),
                    });
                }
                leakproof = Some(def_elem_bool(element, &format!("{keyword}: LEAKPROOF"))?);
            }
            "parallel" => {
                if parallel.is_some() {
                    return Err(SQLError::Routine {
                        sqlstate: "42601".into(),
                        message: format!("{keyword}: conflicting or redundant parallel option"),
                    });
                }
                parallel = Some(match def_elem_string(element)?.as_str() {
                    "unsafe" => FunctionParallel::Unsafe,
                    "restricted" => FunctionParallel::Restricted,
                    "safe" => FunctionParallel::Safe,
                    other => {
                        return Err(SQLError::TypeMismatch(format!(
                            "{keyword}: invalid PARALLEL value `{other}`"
                        )))
                    }
                });
            }
            "support" => {
                if support.is_some() {
                    return Err(SQLError::Routine {
                        sqlstate: "42601".into(),
                        message: format!("{keyword}: conflicting or redundant support option"),
                    });
                }
                support = Some(compile_support_name(element, keyword)?);
            }
            "set" => config_actions.push(compile_routine_config_action(element, keyword)?),
            other => {
                return Err(SQLError::Unsupported(format!(
                    "{keyword}: action `{other}` is not supported"
                )))
            }
        }
    }
    if volatility.is_none()
        && strict.is_none()
        && security_definer.is_none()
        && leakproof.is_none()
        && parallel.is_none()
        && support.is_none()
        && config_actions.is_empty()
    {
        return Err(SQLError::Unsupported(format!(
            "{keyword}: no supported action"
        )));
    }
    Ok(AlterRoutineStmt {
        kind,
        name,
        arg_types,
        arg_type_references,
        volatility,
        strict,
        security_definer,
        leakproof,
        parallel,
        support,
        config_actions,
    })
}

fn compile_role_spec(
    role: &pg_query::protobuf::RoleSpec,
    allow_public: bool,
    context: &str,
) -> Result<String> {
    use pg_query::protobuf::RoleSpecType;
    match role.roletype() {
        RoleSpecType::RolespecCstring => Ok(role.rolename.clone()),
        RoleSpecType::RolespecCurrentRole | RoleSpecType::RolespecCurrentUser => {
            Ok("CURRENT_USER".into())
        }
        RoleSpecType::RolespecSessionUser => Ok("SESSION_USER".into()),
        RoleSpecType::RolespecPublic if allow_public => Ok("PUBLIC".into()),
        other => Err(SQLError::Unsupported(format!(
            "{context}: role specification {other:?} is not supported"
        ))),
    }
}

struct CompiledRoutineTarget {
    name: String,
    arg_types: Option<Vec<String>>,
    arg_type_references: Vec<Option<crate::ast::RoutineColumnTypeReference>>,
}

fn compile_object_with_args(
    object: &pg_query::protobuf::ObjectWithArgs,
    context: &str,
) -> Result<CompiledRoutineTarget> {
    let name = compile_qualified_name(&object.objname, context)?;
    if object.args_unspecified {
        return Ok(CompiledRoutineTarget {
            name,
            arg_types: None,
            arg_type_references: Vec::new(),
        });
    }
    let mut types = Vec::with_capacity(object.objargs.len());
    let mut references = Vec::with_capacity(object.objargs.len());
    for argument in &object.objargs {
        let Some(NodeEnum::TypeName(type_name)) = argument.node.as_ref() else {
            return Err(SQLError::Unsupported(format!(
                "{context}: malformed argument type"
            )));
        };
        let compiled = compile_function_type_name(type_name)?;
        types.push(compiled.name);
        references.push(compiled.reference);
    }
    if references.iter().all(Option::is_none) {
        references.clear();
    }
    Ok(CompiledRoutineTarget {
        name,
        arg_types: Some(types),
        arg_type_references: references,
    })
}

pub(super) fn compile_alter_routine_owner(
    statement: &pg_query::protobuf::AlterOwnerStmt,
) -> Result<Statement> {
    use crate::ast::{AlterRoutineKind, AlterRoutineOwnerStmt};
    use pg_query::protobuf::ObjectType;
    let (kind, context) = match statement.object_type() {
        ObjectType::ObjectFunction => (AlterRoutineKind::Function, "ALTER FUNCTION"),
        ObjectType::ObjectProcedure => (AlterRoutineKind::Procedure, "ALTER PROCEDURE"),
        ObjectType::ObjectRoutine => (AlterRoutineKind::Routine, "ALTER ROUTINE"),
        other => {
            return Err(SQLError::Unsupported(format!(
                "ALTER OWNER target {other:?} is not supported"
            )))
        }
    };
    let Some(NodeEnum::ObjectWithArgs(object)) = statement
        .object
        .as_deref()
        .and_then(|object| object.node.as_ref())
    else {
        return Err(SQLError::Internal(format!(
            "{context}: malformed routine target"
        )));
    };
    let CompiledRoutineTarget {
        name,
        arg_types,
        arg_type_references,
    } = compile_object_with_args(object, context)?;
    let owner = statement
        .newowner
        .as_ref()
        .ok_or_else(|| SQLError::Internal(format!("{context}: owner is missing")))?;
    Ok(Statement::AlterRoutineOwner(AlterRoutineOwnerStmt {
        kind,
        name,
        arg_types,
        arg_type_references,
        new_owner: compile_role_spec(owner, false, context)?,
    }))
}

pub(super) fn compile_grant_routine(
    statement: &pg_query::protobuf::GrantStmt,
) -> Result<Statement> {
    use crate::ast::{AlterRoutineKind, GrantRoutineItem, GrantRoutineStmt};
    use pg_query::protobuf::{GrantTargetType, ObjectType};
    if statement.targtype() != GrantTargetType::AclTargetObject {
        return Err(SQLError::Unsupported(
            "routine privileges require explicit object targets".into(),
        ));
    }
    let (kind, context) = match statement.objtype() {
        ObjectType::ObjectFunction => (AlterRoutineKind::Function, "FUNCTION"),
        ObjectType::ObjectProcedure => (AlterRoutineKind::Procedure, "PROCEDURE"),
        ObjectType::ObjectRoutine => (AlterRoutineKind::Routine, "ROUTINE"),
        other => {
            return Err(SQLError::Unsupported(format!(
                "GRANT/REVOKE object type {other:?} is not supported"
            )))
        }
    };
    // PostgreSQL represents ALL [PRIVILEGES] with an empty privilege list.
    // EXECUTE is the sole routine privilege, so it is equivalent here.
    for privilege in &statement.privileges {
        let Some(NodeEnum::AccessPriv(privilege)) = privilege.node.as_ref() else {
            return Err(SQLError::Internal(
                "GRANT/REVOKE contains a malformed privilege".into(),
            ));
        };
        if !privilege.priv_name.eq_ignore_ascii_case("execute") || !privilege.cols.is_empty() {
            return Err(SQLError::Unsupported(format!(
                "only EXECUTE is valid for {context} privileges"
            )));
        }
    }
    let mut items = Vec::with_capacity(statement.objects.len());
    for object in &statement.objects {
        let Some(NodeEnum::ObjectWithArgs(object)) = object.node.as_ref() else {
            return Err(SQLError::Internal(
                "GRANT/REVOKE contains a malformed routine target".into(),
            ));
        };
        let CompiledRoutineTarget {
            name,
            arg_types,
            arg_type_references,
        } = compile_object_with_args(object, context)?;
        if !arg_type_references.is_empty() {
            return Err(SQLError::Unsupported(
                "routine privilege targets using %TYPE are not supported".into(),
            ));
        }
        items.push(GrantRoutineItem { name, arg_types });
    }
    let grantees = statement
        .grantees
        .iter()
        .map(|grantee| {
            let Some(NodeEnum::RoleSpec(role)) = grantee.node.as_ref() else {
                return Err(SQLError::Internal(
                    "GRANT/REVOKE contains a malformed grantee".into(),
                ));
            };
            compile_role_spec(role, true, "GRANT/REVOKE")
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Statement::GrantRoutine(GrantRoutineStmt {
        kind,
        is_grant: statement.is_grant,
        grant_option: statement.grant_option,
        grant_option_only: !statement.is_grant && statement.grant_option,
        items,
        grantees,
    }))
}

fn role_option_bool(element: &pg_query::protobuf::DefElem, context: &str) -> Result<bool> {
    def_elem_bool(element, context)
}

fn role_option_i32(element: &pg_query::protobuf::DefElem, context: &str) -> Result<i32> {
    match element
        .arg
        .as_ref()
        .and_then(|argument| argument.node.as_ref())
    {
        Some(NodeEnum::Integer(value)) => Ok(value.ival),
        other => Err(SQLError::TypeMismatch(format!(
            "{context} expects an integer, got {other:?}"
        ))),
    }
}

pub(super) fn compile_create_role(
    statement: &pg_query::protobuf::CreateRoleStmt,
) -> Result<Statement> {
    use crate::ast::{CreateRoleStmt, RoleAttribute};
    use pg_query::protobuf::RoleStmtType;
    let mut attributes = std::collections::BTreeSet::from([RoleAttribute::Inherit]);
    if statement.stmt_type() == RoleStmtType::RolestmtUser {
        attributes.insert(RoleAttribute::Login);
    }
    let mut role = CreateRoleStmt {
        name: statement.role.clone(),
        attributes,
        connection_limit: -1,
    };
    for option in &statement.options {
        let Some(NodeEnum::DefElem(element)) = option.node.as_ref() else {
            return Err(SQLError::Internal(
                "CREATE ROLE has a malformed option".into(),
            ));
        };
        let (attribute, context) = match element.defname.to_ascii_lowercase().as_str() {
            "superuser" => (RoleAttribute::Superuser, "SUPERUSER"),
            "inherit" => (RoleAttribute::Inherit, "INHERIT"),
            "createrole" => (RoleAttribute::CreateRole, "CREATEROLE"),
            "createdb" => (RoleAttribute::CreateDb, "CREATEDB"),
            "canlogin" => (RoleAttribute::Login, "LOGIN"),
            "isreplication" => (RoleAttribute::Replication, "REPLICATION"),
            "bypassrls" => (RoleAttribute::BypassRls, "BYPASSRLS"),
            "connectionlimit" => {
                role.connection_limit = role_option_i32(element, "CONNECTION LIMIT")?;
                continue;
            }
            other => {
                return Err(SQLError::Unsupported(format!(
                    "CREATE ROLE option `{other}` is not supported"
                )))
            }
        };
        if role_option_bool(element, context)? {
            role.attributes.insert(attribute);
        } else {
            role.attributes.remove(&attribute);
        }
    }
    Ok(Statement::CreateRole(role))
}

pub(super) fn compile_alter_role(
    statement: &pg_query::protobuf::AlterRoleStmt,
) -> Result<Statement> {
    use crate::ast::{AlterRoleStmt, RoleAttribute};
    let role = statement
        .role
        .as_ref()
        .ok_or_else(|| SQLError::Internal("ALTER ROLE has no target".into()))?;
    let mut alter = AlterRoleStmt {
        name: compile_role_spec(role, false, "ALTER ROLE")?,
        attributes: std::collections::BTreeMap::new(),
        connection_limit: None,
    };
    for option in &statement.options {
        let Some(NodeEnum::DefElem(element)) = option.node.as_ref() else {
            return Err(SQLError::Internal(
                "ALTER ROLE has a malformed option".into(),
            ));
        };
        let (attribute, context) = match element.defname.to_ascii_lowercase().as_str() {
            "superuser" => (RoleAttribute::Superuser, "SUPERUSER"),
            "inherit" => (RoleAttribute::Inherit, "INHERIT"),
            "createrole" => (RoleAttribute::CreateRole, "CREATEROLE"),
            "createdb" => (RoleAttribute::CreateDb, "CREATEDB"),
            "canlogin" => (RoleAttribute::Login, "LOGIN"),
            "isreplication" => (RoleAttribute::Replication, "REPLICATION"),
            "bypassrls" => (RoleAttribute::BypassRls, "BYPASSRLS"),
            "connectionlimit" => {
                alter.connection_limit = Some(role_option_i32(element, "CONNECTION LIMIT")?);
                continue;
            }
            other => {
                return Err(SQLError::Unsupported(format!(
                    "ALTER ROLE option `{other}` is not supported"
                )))
            }
        };
        alter
            .attributes
            .insert(attribute, role_option_bool(element, context)?);
    }
    Ok(Statement::AlterRole(alter))
}

pub(super) fn compile_drop_role(statement: &pg_query::protobuf::DropRoleStmt) -> Result<Statement> {
    use crate::ast::DropRoleStmt;
    let names = statement
        .roles
        .iter()
        .map(|role| {
            let Some(NodeEnum::RoleSpec(role)) = role.node.as_ref() else {
                return Err(SQLError::Internal(
                    "DROP ROLE has a malformed target".into(),
                ));
            };
            compile_role_spec(role, false, "DROP ROLE")
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Statement::DropRole(DropRoleStmt {
        names,
        if_exists: statement.missing_ok,
    }))
}
