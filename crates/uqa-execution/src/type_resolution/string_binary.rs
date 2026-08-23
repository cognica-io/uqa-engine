//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared `PostgreSQL` overload resolution for string and binary built-ins.

use super::common::base_type;
use super::functions::named_argument_value;
use super::{scalar_type_inner, FunctionTypeResolver, ResolvedFunctionOverload};
use crate::{RowSchema, ScalarExpr};
use uqa_core::Value;
use uqa_sql::ast::{ColumnType, FunctionBinding};
use uqa_sql::{SQLError, SQLParam};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedStringBinaryOverload {
    Builtin(ColumnType),
    User(ResolvedFunctionOverload),
}

#[doc(hidden)]
pub type ResolvedTextByteaOverload = ResolvedStringBinaryOverload;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuiltinResult {
    Argument,
    Integer,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Function {
    name: &'static str,
    result: BuiltinResult,
    bytea: bool,
    bpchar: bool,
}

pub(super) const REVERSE: Function = Function {
    name: "reverse",
    result: BuiltinResult::Argument,
    bytea: true,
    bpchar: false,
};

pub(super) const MD5: Function = Function {
    name: "md5",
    result: BuiltinResult::Text,
    bytea: true,
    bpchar: false,
};

pub(super) const LENGTH: Function = Function {
    name: "length",
    result: BuiltinResult::Integer,
    bytea: true,
    bpchar: true,
};

pub(super) const CHAR_LENGTH: Function = Function {
    name: "char_length",
    result: BuiltinResult::Integer,
    bytea: false,
    bpchar: true,
};

pub(super) const CHARACTER_LENGTH: Function = Function {
    name: "character_length",
    result: BuiltinResult::Integer,
    bytea: false,
    bpchar: true,
};

pub(super) const OCTET_LENGTH: Function = Function {
    name: "octet_length",
    result: BuiltinResult::Integer,
    bytea: true,
    bpchar: true,
};

pub(super) const BIT_LENGTH: Function = Function {
    name: "bit_length",
    result: BuiltinResult::Integer,
    bytea: true,
    bpchar: false,
};

impl Function {
    pub(super) fn matches(self, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        lower.strip_prefix("pg_catalog.").unwrap_or(&lower) == self.name
    }

    fn return_type(self, argument_type: ColumnType) -> ColumnType {
        match self.result {
            BuiltinResult::Argument => argument_type,
            BuiltinResult::Integer => ColumnType::Integer,
            BuiltinResult::Text => ColumnType::Text,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SelectedOverload {
    Builtin(BuiltinOverload),
    User(ResolvedFunctionOverload),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BuiltinOverload {
    argument_type: ColumnType,
    exact_matches: usize,
    preferred_matches: usize,
}

pub(super) fn resolve_type(
    function: Function,
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ColumnType>, SQLError> {
    let argument_names = argument_names(args);
    let argument_types = effective_argument_types(args, argument_types);
    select_signature(
        function,
        name,
        binding,
        &argument_names,
        &argument_types,
        resolver,
    )
    .map(|selected| {
        Some(match selected {
            SelectedOverload::Builtin(overload) => function.return_type(overload.argument_type),
            SelectedOverload::User(overload) => overload.return_type,
        })
    })
}

pub(super) fn resolve_overload(
    function: Function,
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<ResolvedStringBinaryOverload, SQLError> {
    select_signature(
        function,
        name,
        binding,
        argument_names,
        argument_types,
        resolver,
    )
    .map(|selected| match selected {
        SelectedOverload::Builtin(overload) => {
            ResolvedStringBinaryOverload::Builtin(overload.argument_type)
        }
        SelectedOverload::User(overload) => ResolvedStringBinaryOverload::User(overload),
    })
}

pub(super) fn builtin_argument_type(
    function: Function,
    argument_types: &[Option<ColumnType>],
) -> Option<ColumnType> {
    let [argument_type] = argument_types else {
        return None;
    };
    builtin_overload(function, argument_type.as_ref()).map(|overload| overload.argument_type)
}

fn select_signature(
    function: Function,
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<SelectedOverload, SQLError> {
    if let Some(binding) = binding.filter(|binding| binding.builtin) {
        return resolve_bound_builtin(function, binding)
            .map(SelectedOverload::Builtin)
            .ok_or_else(|| undefined_function(name, argument_names, argument_types));
    }
    let builtin = resolve_builtin(function, name, argument_names, argument_types);
    let user = resolve_user(
        name,
        binding,
        argument_names,
        argument_types,
        builtin.as_ref().ok(),
        resolver,
    );
    if binding.is_some() {
        return user?
            .map(SelectedOverload::User)
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "42883".into(),
                message: format!("bound function {name} does not exist"),
            });
    }
    let user = match user {
        Ok(user) => user,
        Err(error) if error.sqlstate() == Some("42883") => None,
        Err(error) if builtin.is_ok() && error.sqlstate() == Some("42725") => None,
        Err(error) => return Err(error),
    };
    match (builtin, user) {
        (Ok(builtin), None) => Ok(SelectedOverload::Builtin(builtin)),
        (Err(_), Some(user)) => Ok(SelectedOverload::User(user)),
        (Err(error), None) => Err(error),
        (Ok(builtin), Some(user)) => {
            rank_builtin_and_user(name, argument_names, argument_types, builtin, user)
        }
    }
}

fn resolve_bound_builtin(function: Function, binding: &FunctionBinding) -> Option<BuiltinOverload> {
    let [argument_type] = binding.argument_types.as_slice() else {
        return None;
    };
    if !function.matches(&binding.name) {
        return None;
    }
    let argument_type = ColumnType::from_sql_name(argument_type).ok()?;
    let overload = builtin_overload(function, Some(&argument_type))?;
    (overload.argument_type == argument_type).then_some(overload)
}

fn resolve_builtin(
    function: Function,
    name: &str,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
) -> Result<BuiltinOverload, SQLError> {
    if !function.matches(name) {
        return Err(undefined_function(name, argument_names, argument_types));
    }
    let [argument_name] = argument_names else {
        return Err(undefined_function(name, argument_names, argument_types));
    };
    if argument_name.is_some() {
        return Err(undefined_function(name, argument_names, argument_types));
    }
    builtin_overload(function, argument_types.first().and_then(Option::as_ref))
        .ok_or_else(|| undefined_function(name, argument_names, argument_types))
}

fn builtin_overload(
    function: Function,
    argument_type: Option<&ColumnType>,
) -> Option<BuiltinOverload> {
    let Some(argument_type) = argument_type else {
        return Some(BuiltinOverload {
            argument_type: ColumnType::Text,
            exact_matches: 0,
            preferred_matches: 1,
        });
    };
    let base = base_type(argument_type);
    let exact = usize::from(argument_type == base);
    match base {
        ColumnType::Bytea if function.bytea => Some(BuiltinOverload {
            argument_type: ColumnType::Bytea,
            exact_matches: exact,
            preferred_matches: 0,
        }),
        ColumnType::Text => Some(BuiltinOverload {
            argument_type: ColumnType::Text,
            exact_matches: exact,
            preferred_matches: usize::from(exact == 0),
        }),
        ColumnType::Bpchar | ColumnType::Character(_) if function.bpchar => Some(BuiltinOverload {
            argument_type: ColumnType::Bpchar,
            exact_matches: exact,
            preferred_matches: 0,
        }),
        ColumnType::Name
        | ColumnType::Varchar(_)
        | ColumnType::Bpchar
        | ColumnType::Character(_)
        | ColumnType::InternalChar => Some(BuiltinOverload {
            argument_type: ColumnType::Text,
            exact_matches: 0,
            preferred_matches: 1,
        }),
        _ => None,
    }
}

fn resolve_user(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    builtin: Option<&BuiltinOverload>,
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
    if binding.is_none() && name.to_ascii_lowercase().starts_with("pg_catalog.") {
        return Ok(None);
    }
    let Some(resolver) = resolver else {
        return Ok(None);
    };
    let mut lookup_types = argument_types.to_vec();
    if matches!(builtin, Some(overload) if overload.argument_type == ColumnType::Text)
        && matches!(lookup_types.as_slice(), [None])
        && argument_names == [None]
    {
        lookup_types[0] = Some(ColumnType::Text);
    }
    resolver.resolve_function_overload(name, binding, argument_names, &lookup_types)
}

fn rank_builtin_and_user(
    name: &str,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    builtin: BuiltinOverload,
    user: ResolvedFunctionOverload,
) -> Result<SelectedOverload, SQLError> {
    let builtin_signature = builtin.argument_type.sql_name();
    if user.binding.argument_types.as_slice() == [builtin_signature.as_str()] {
        return Ok(if user.precedes_pg_catalog {
            SelectedOverload::User(user)
        } else {
            SelectedOverload::Builtin(builtin)
        });
    }
    match user.exact_matches.cmp(&builtin.exact_matches) {
        std::cmp::Ordering::Greater => return Ok(SelectedOverload::User(user)),
        std::cmp::Ordering::Less => return Ok(SelectedOverload::Builtin(builtin)),
        std::cmp::Ordering::Equal => {}
    }
    match user.preferred_matches.cmp(&builtin.preferred_matches) {
        std::cmp::Ordering::Greater => Ok(SelectedOverload::User(user)),
        std::cmp::Ordering::Less => Ok(SelectedOverload::Builtin(builtin)),
        std::cmp::Ordering::Equal => Err(ambiguous_function(name, argument_names, argument_types)),
    }
}

fn effective_argument_types(
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
) -> Vec<Option<ColumnType>> {
    args.iter()
        .zip(argument_types)
        .map(|(argument, argument_type)| {
            if matches!(
                named_argument_value(argument),
                ScalarExpr::Literal(Value::Str(_) | Value::Null)
            ) {
                None
            } else {
                argument_type.clone()
            }
        })
        .collect()
}

pub(super) fn bind_call(
    function: Function,
    name: String,
    binding: &mut Option<FunctionBinding>,
    args: &mut [ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> String {
    if binding.is_some() || !function.matches(&name) {
        return name;
    }
    let argument_types = args
        .iter()
        .map(|argument| scalar_type_inner(named_argument_value(argument), schema, params, resolver))
        .collect::<Result<Vec<_>, _>>();
    let source_type = argument_types
        .as_ref()
        .ok()
        .and_then(|types| types.first())
        .and_then(Clone::clone);
    let argument_names = argument_names(args);
    let selected = argument_types.and_then(|types| {
        let types = effective_argument_types(args, &types);
        select_signature(function, &name, None, &argument_names, &types, resolver)
    });
    match selected {
        Ok(SelectedOverload::User(overload)) => *binding = Some(overload.binding),
        Ok(SelectedOverload::Builtin(overload)) => {
            if !same_runtime_signature(source_type.as_ref(), &overload.argument_type) {
                if let Some(argument) = args.first_mut() {
                    *argument = ScalarExpr::Cast {
                        expr: Box::new(std::mem::replace(
                            argument,
                            ScalarExpr::Literal(Value::Null),
                        )),
                        ty: overload.argument_type.sql_name(),
                    };
                }
            }
        }
        Err(_) => {}
    }
    name
}

fn same_runtime_signature(source: Option<&ColumnType>, target: &ColumnType) -> bool {
    let Some(source) = source.map(base_type) else {
        return false;
    };
    match (source, target) {
        (
            ColumnType::Bpchar | ColumnType::Character(_),
            ColumnType::Bpchar | ColumnType::Character(_),
        ) => true,
        _ => source == target,
    }
}

fn argument_names(args: &[ScalarExpr]) -> Vec<Option<String>> {
    args.iter()
        .map(|argument| named_argument_name(argument).map(str::to_string))
        .collect()
}

fn named_argument_name(expression: &ScalarExpr) -> Option<&str> {
    let ScalarExpr::Func { name, args, .. } = expression else {
        return None;
    };
    if name != uqa_sql::expr::NAMED_ARG_FUNCTION {
        return None;
    }
    match args.first() {
        Some(ScalarExpr::Literal(Value::Str(name))) => Some(name),
        _ => None,
    }
}

fn undefined_function(
    name: &str,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
) -> SQLError {
    function_resolution_error(
        "42883",
        "does not exist",
        name,
        argument_names,
        argument_types,
    )
}

fn ambiguous_function(
    name: &str,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
) -> SQLError {
    function_resolution_error(
        "42725",
        "is not unique",
        name,
        argument_names,
        argument_types,
    )
}

fn function_resolution_error(
    sqlstate: &str,
    description: &str,
    name: &str,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
) -> SQLError {
    let signature = argument_names
        .iter()
        .zip(argument_types)
        .map(|(argument_name, argument_type)| {
            let argument_type = argument_type
                .as_ref()
                .map_or_else(|| "unknown".into(), ColumnType::regtype_name);
            argument_name
                .as_ref()
                .map_or(argument_type.clone(), |argument_name| {
                    format!("{argument_name} => {argument_type}")
                })
        })
        .collect::<Vec<_>>()
        .join(", ");
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: format!("function {name}({signature}) {description}"),
    }
}
