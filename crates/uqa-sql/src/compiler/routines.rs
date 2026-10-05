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
use crate::ast::SQLBodyForm;

mod attributes;
mod roles;

pub(super) use roles::{
    compile_acl_role_specification, compile_alter_role, compile_alter_routine_owner,
    compile_create_role, compile_drop_role, compile_grant, compile_grant_role,
    compile_object_with_args, compile_role_specification, CompiledRoutineTarget,
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
            let Statement::SetVariable { name, value, .. } =
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

#[expect(
    clippy::too_many_lines,
    reason = "ordered PostgreSQL lowering preserves syntax and error precedence"
)]
pub(super) fn compile_create_function(
    stmt: &pg_query::protobuf::CreateFunctionStmt,
) -> Result<crate::ast::CreateFunction> {
    use crate::ast::{
        CreateFunction, FunctionBody, FunctionParam, FunctionParamMode, FunctionReturns,
        FunctionVolatility, RoutineAttributeClause,
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
        let arg_type = fp
            .arg_type
            .as_ref()
            .ok_or_else(|| SQLError::Internal(format!("{keyword}: parameter without type")))?;
        let compiled_type = compile_function_type_name(arg_type)?;
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
            written_type: Some(attributes::written_type_name(arg_type)?),
            mode,
            default,
        });
    }

    let (returns, return_type_reference, return_written_type) = if has_table_param {
        (FunctionReturns::Table, None, None)
    } else {
        match stmt.return_type.as_ref() {
            None => (FunctionReturns::None, None, None),
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
                (
                    returns,
                    compiled.reference,
                    Some(attributes::written_type_name(t)?),
                )
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
    let mut cost = None;
    let mut rows = None;
    let mut config_actions = Vec::new();
    let mut as_items: Option<Vec<String>> = None;
    let mut attribute_clauses = crate::ast::RoutineAttributeClauses::default();
    for opt in &stmt.options {
        let Some(NodeEnum::DefElem(elem)) = opt.node.as_ref() else {
            return Err(SQLError::Internal(format!("{keyword}: malformed option")));
        };
        let clause = attributes::routine_attribute_clause(elem, keyword)?;
        let first = !attribute_clauses.clauses.contains(&clause);
        attribute_clauses.clauses.push(clause);
        // Registration rejects a repeated clause once the routine's schema accepts the statement.
        if !first && !clause.repeatable() {
            continue;
        }
        match clause {
            RoutineAttributeClause::Language => {
                language = def_elem_string(elem)?.to_ascii_lowercase();
            }
            RoutineAttributeClause::Volatility => {
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
            RoutineAttributeClause::Strict => {
                strict = def_elem_bool(elem, &format!("{keyword}: STRICT"))?;
            }
            RoutineAttributeClause::Security => {
                security_definer = def_elem_bool(elem, &format!("{keyword}: SECURITY"))?;
            }
            RoutineAttributeClause::Leakproof => {
                leakproof = def_elem_bool(elem, &format!("{keyword}: LEAKPROOF"))?;
            }
            RoutineAttributeClause::Parallel => match attributes::compile_parallel(elem)? {
                Ok(value) => parallel = value,
                Err(value) => attribute_clauses.invalid_parallel = Some(value),
            },
            RoutineAttributeClause::Support => support = Some(compile_support_name(elem, keyword)?),
            RoutineAttributeClause::Set => {
                config_actions.push(compile_routine_config_action(elem, keyword)?);
            }
            RoutineAttributeClause::Cost => {
                cost = Some(attributes::def_elem_float4(
                    elem,
                    &format!("{keyword}: COST"),
                )?);
            }
            RoutineAttributeClause::Rows => {
                rows = Some(attributes::def_elem_float4(
                    elem,
                    &format!("{keyword}: ROWS"),
                )?);
            }
            RoutineAttributeClause::As => {
                as_items = Some(attributes::compile_as_items(elem, keyword)?);
            }
            RoutineAttributeClause::Transform => {
                attribute_clauses.transform_types =
                    attributes::compile_transform_types(elem, keyword)?;
            }
            // Registration rejects a window function where PostgreSQL would create one.
            RoutineAttributeClause::Window => {}
        }
    }

    let (body, sql_body_form) = match (as_items, stmt.sql_body.as_deref()) {
        (Some(items), None) => {
            if items.len() != 1 {
                attribute_clauses.body_error = Some(crate::ast::RoutineBodyError::ExtraAsItems);
            }
            (
                FunctionBody::Source(items.into_iter().next().unwrap_or_default()),
                None,
            )
        }
        (None, Some(node)) => {
            let (statements, form) = compile_sql_standard_body(node)?;
            (FunctionBody::Statements(statements), Some(form))
        }
        (Some(items), Some(_)) => {
            attribute_clauses.body_error = Some(crate::ast::RoutineBodyError::Duplicate);
            (
                FunctionBody::Source(items.into_iter().next().unwrap_or_default()),
                None,
            )
        }
        (None, None) => {
            attribute_clauses.body_error = Some(crate::ast::RoutineBodyError::Missing);
            (FunctionBody::Source(String::new()), None)
        }
    };
    // A SQL-standard body implies LANGUAGE sql; without either, registration reports that no language is specified.
    if language.is_empty() && stmt.sql_body.is_some() {
        language = "sql".into();
    }

    Ok(CreateFunction {
        object_id: None,
        catalog_revision: None,
        catalog_oid: None,
        name,
        or_replace: stmt.replace,
        is_procedure: stmt.is_procedure,
        params,
        returns,
        return_type_reference,
        return_written_type,
        language,
        body,
        sql_body_form,
        creation_search_path: Vec::new(),
        volatility,
        strict,
        owner: None,
        security: crate::ast::RoutineSecurityAttributes {
            security_definer,
            leakproof,
        },
        parallel,
        support,
        cost,
        rows,
        config: Vec::new(),
        config_actions,
        attribute_clauses,
        execute_acl: None,
    })
}

/// Compile a SQL-standard function body (`RETURN expr` or
/// `BEGIN ATOMIC stmt; ... END`) into plain statements and its written form.
pub(super) fn compile_sql_standard_body(node: &Node) -> Result<(Vec<Statement>, SQLBodyForm)> {
    let Some(inner) = node.node.as_ref() else {
        return Err(SQLError::Internal("empty SQL function body".into()));
    };
    match inner {
        NodeEnum::ReturnStmt(ret) => {
            let value = ret
                .returnval
                .as_deref()
                .ok_or_else(|| SQLError::Internal("RETURN without a value".into()))?;
            Ok((
                vec![select_of_expr(compile_expr(value)?)],
                SQLBodyForm::Return,
            ))
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
            Ok((out, SQLBodyForm::Atomic))
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

#[expect(
    clippy::too_many_lines,
    reason = "ordered PostgreSQL lowering preserves syntax and error precedence"
)]
pub(super) fn compile_alter_routine(
    stmt: &pg_query::protobuf::AlterFunctionStmt,
) -> Result<crate::ast::AlterRoutineStmt> {
    use crate::ast::{
        AlterRoutineKind, AlterRoutineStmt, FunctionVolatility, RoutineAttributeClause,
    };
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
    let mut cost = None;
    let mut rows = None;
    let mut config_actions = Vec::new();
    let mut attribute_clauses = crate::ast::RoutineAttributeClauses::default();
    for action in &stmt.actions {
        let Some(NodeEnum::DefElem(element)) = action.node.as_ref() else {
            return Err(SQLError::Unsupported(format!(
                "{keyword}: malformed action node {:?}",
                action.node
            )));
        };
        let clause = attributes::routine_attribute_clause(element, keyword)?;
        let first = !attribute_clauses.clauses.contains(&clause);
        attribute_clauses.clauses.push(clause);
        // `AlterFunction` rejects a repeated action once it has found the routine.
        if !first && !clause.repeatable() {
            continue;
        }
        match clause {
            RoutineAttributeClause::Volatility => {
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
            RoutineAttributeClause::Strict => {
                strict = Some(def_elem_bool(element, &format!("{keyword}: STRICT"))?);
            }
            RoutineAttributeClause::Security => {
                security_definer = Some(def_elem_bool(element, &format!("{keyword}: SECURITY"))?);
            }
            RoutineAttributeClause::Leakproof => {
                leakproof = Some(def_elem_bool(element, &format!("{keyword}: LEAKPROOF"))?);
            }
            RoutineAttributeClause::Parallel => match attributes::compile_parallel(element)? {
                Ok(value) => parallel = Some(value),
                Err(value) => attribute_clauses.invalid_parallel = Some(value),
            },
            RoutineAttributeClause::Support => {
                support = Some(compile_support_name(element, keyword)?);
            }
            RoutineAttributeClause::Set => {
                config_actions.push(compile_routine_config_action(element, keyword)?);
            }
            RoutineAttributeClause::Cost => {
                cost = Some(attributes::def_elem_float4(
                    element,
                    &format!("{keyword}: COST"),
                )?);
            }
            RoutineAttributeClause::Rows => {
                rows = Some(attributes::def_elem_float4(
                    element,
                    &format!("{keyword}: ROWS"),
                )?);
            }
            RoutineAttributeClause::As
            | RoutineAttributeClause::Language
            | RoutineAttributeClause::Transform
            | RoutineAttributeClause::Window => {
                return Err(SQLError::Internal(format!(
                    "{keyword}: action `{}` is not an ALTER action",
                    element.defname
                )))
            }
        }
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
        cost,
        rows,
        config_actions,
        attribute_clauses,
    })
}
