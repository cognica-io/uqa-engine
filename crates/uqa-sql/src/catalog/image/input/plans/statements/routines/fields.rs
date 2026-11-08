//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::{invalid, Body, Empty, Field, Input, PlanMemory, SQLError, Step};
use crate::ast::{
    CreateFunction, FunctionBody, FunctionParallel, FunctionParam, FunctionParamMode,
    FunctionReturns, FunctionVolatility, ObjectAclEntry, RoutineAttributeClauses,
    RoutineColumnTypeReference, RoutineDefaultType, RoutineSecurityAttributes, SQLBodyForm,
};
use uqa_core::catalog_role::RoleIdentity;

empty! {
    u32 => 0, f32 => 0.0, [u8; 16] => [0; 16], RoleIdentity => RoleIdentity::BOOTSTRAP,
    FunctionParamMode => FunctionParamMode::In, FunctionReturns => FunctionReturns::None,
    FunctionVolatility => FunctionVolatility::Volatile, FunctionParallel => FunctionParallel::Unsafe,
    SQLBodyForm => SQLBodyForm::Return, RoutineSecurityAttributes => RoutineSecurityAttributes::default(),
    RoutineAttributeClauses => RoutineAttributeClauses::default(),
}
leaf!(u32, |input, _memory| Ok::<_, SQLError>(u32::from_le_bytes(
    input.binary.array()?
)));
leaf!(f32, |input, _memory| Ok::<_, SQLError>(f32::from_bits(
    u32::from_le_bytes(input.binary.array()?)
)));
leaf!([u8; 16], |input, _memory| input.object_id());
leaf!(RoleIdentity, |input, _memory| input.identity());
leaf!(FunctionParamMode, |input, _memory| Ok::<_, SQLError>(
    match input.binary.array::<1>()?[0] {
        0 => Self::In,
        1 => Self::Out,
        2 => Self::InOut,
        3 => Self::Variadic,
        4 => Self::Table,
        _ => return Err(invalid("routine parameter mode")),
    }
));
leaf!(FunctionReturns, |input, memory| Ok::<_, SQLError>(
    match input.binary.array::<1>()?[0] {
        0 => Self::None,
        1 => Self::Scalar {
            type_name: input.plan_text(memory)?
        },
        2 => Self::SetOf {
            type_name: input.plan_text(memory)?
        },
        3 => Self::Table,
        _ => return Err(invalid("routine result shape")),
    }
));
leaf!(FunctionVolatility, |input, _memory| Ok::<_, SQLError>(
    match input.binary.array::<1>()?[0] {
        0 => Self::Immutable,
        1 => Self::Stable,
        2 => Self::Volatile,
        _ => return Err(invalid("routine volatility")),
    }
));
leaf!(FunctionParallel, |input, _memory| Ok::<_, SQLError>(
    match input.binary.array::<1>()?[0] {
        0 => Self::Unsafe,
        1 => Self::Restricted,
        2 => Self::Safe,
        _ => return Err(invalid("routine parallel safety")),
    }
));
leaf!(SQLBodyForm, |input, _memory| Ok::<_, SQLError>(
    match input.binary.array::<1>()?[0] {
        0 => Self::Return,
        1 => Self::Atomic,
        _ => return Err(invalid("routine SQL body form")),
    }
));
leaf!(
    RoutineSecurityAttributes,
    |input, _memory| Ok::<_, SQLError>(Self {
        security_definer: input.flag()?,
        leakproof: input.flag()?
    })
);
leaf!(Option<RoutineColumnTypeReference>, |input, memory| input
    .flag()?
    .then(|| Ok::<_, SQLError>(RoutineColumnTypeReference {
        schema: input.plan_optional_text(memory)?,
        relation: input.plan_text(memory)?,
        column: input.plan_text(memory)?,
    }))
    .transpose());
leaf!(Option<RoutineDefaultType>, |input, memory| input
    .flag()?
    .then(|| Ok::<_, SQLError>(match input.binary.array::<1>()?[0] {
        0 => {
            let (value, lease) = input.column_type()?.into_parts();
            memory.absorb(lease);
            RoutineDefaultType::Concrete(value)
        }
        1 => RoutineDefaultType::Polymorphic(input.plan_text(memory)?),
        _ => return Err(invalid("routine default type")),
    }))
    .transpose());
leaf!(Vec<(String, String)>, |input, memory| input.plan_vector(
    memory,
    |input, memory| Ok((input.plan_text(memory)?, input.plan_text(memory)?))
));
leaf!(Vec<ObjectAclEntry>, |input, memory| {
    let (value, lease) = input.object_acl()?.into_parts();
    memory.absorb(lease);
    Ok::<_, SQLError>(value)
});

syntax_struct!(CreateFunction {
    object_id, catalog_revision, catalog_oid, name, or_replace, is_procedure, params, returns,
    return_type_reference, return_written_type, language, body, sql_body_form, creation_search_path,
    volatility, strict, owner, security, parallel, support, cost, rows, config, execute_acl
} skip { body_origin, config_actions, attribute_clauses });
syntax_struct!(FunctionParam {
    name,
    type_name,
    type_reference,
    written_type,
    mode,
    default,
    default_type
});
syntax_enum!(FunctionBody, FunctionBody::Source(String::new()); 0 => Source(value), 1 => Statements(value));
