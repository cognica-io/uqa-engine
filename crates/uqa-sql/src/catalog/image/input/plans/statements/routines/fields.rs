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
leaf!(u32, |input, _memory, read| Ok::<_, SQLError>(
    u32::from_le_bytes(read!(input.binary.array())?)
));
leaf!(f32, |input, _memory, read| Ok::<_, SQLError>(
    f32::from_bits(u32::from_le_bytes(read!(input.binary.array())?))
));
leaf!([u8; 16], |input, _memory, read| read!(input.object_id()));
leaf!(RoleIdentity, |input, _memory, read| read!(input.identity()));
leaf!(FunctionParamMode, |input, _memory, read| Ok::<_, SQLError>(
    match read!(input.binary.array::<1>())?[0] {
        0 => Self::In,
        1 => Self::Out,
        2 => Self::InOut,
        3 => Self::Variadic,
        4 => Self::Table,
        _ => return Err(invalid("routine parameter mode")),
    }
));
leaf!(FunctionReturns, |input, memory, read| Ok::<_, SQLError>(
    match read!(input.binary.array::<1>())?[0] {
        0 => Self::None,
        1 => Self::Scalar {
            type_name: read!(input.plan_text(memory))?
        },
        2 => Self::SetOf {
            type_name: read!(input.plan_text(memory))?
        },
        3 => Self::Table,
        _ => return Err(invalid("routine result shape")),
    }
));
leaf!(
    FunctionVolatility,
    |input, _memory, read| Ok::<_, SQLError>(match read!(input.binary.array::<1>())?[0] {
        0 => Self::Immutable,
        1 => Self::Stable,
        2 => Self::Volatile,
        _ => return Err(invalid("routine volatility")),
    })
);
leaf!(FunctionParallel, |input, _memory, read| Ok::<_, SQLError>(
    match read!(input.binary.array::<1>())?[0] {
        0 => Self::Unsafe,
        1 => Self::Restricted,
        2 => Self::Safe,
        _ => return Err(invalid("routine parallel safety")),
    }
));
leaf!(SQLBodyForm, |input, _memory, read| Ok::<_, SQLError>(
    match read!(input.binary.array::<1>())?[0] {
        0 => Self::Return,
        1 => Self::Atomic,
        _ => return Err(invalid("routine SQL body form")),
    }
));
leaf!(RoutineSecurityAttributes, |input, _memory, read| Ok::<
    _,
    SQLError,
>(Self {
    security_definer: read!(input.flag())?,
    leakproof: read!(input.flag())?,
}));
leaf!(Option<RoutineColumnTypeReference>, |input, memory, read| {
    Ok::<_, SQLError>(if read!(input.flag())? {
        Some(RoutineColumnTypeReference {
            schema: read!(input.plan_optional_text(memory))?,
            relation: read!(input.plan_text(memory))?,
            column: read!(input.plan_text(memory))?,
        })
    } else {
        None
    })
});
leaf!(Option<RoutineDefaultType>, |input, memory, read| {
    Ok::<_, SQLError>(if read!(input.flag())? {
        Some(match read!(input.binary.array::<1>())?[0] {
            0 => {
                let (value, lease) = read!(input.column_type())?.into_parts();
                memory.absorb(lease);
                RoutineDefaultType::Concrete(value)
            }
            1 => RoutineDefaultType::Polymorphic(read!(input.plan_text(memory))?),
            _ => return Err(invalid("routine default type")),
        })
    } else {
        None
    })
});
leaf!(Vec<(String, String)>, |input, memory, read| input_list!(
    read,
    input,
    memory,
    |input, memory| {
        Ok((
            read!(input.plan_text(memory))?,
            read!(input.plan_text(memory))?,
        ))
    }
));
leaf!(Vec<ObjectAclEntry>, |input, memory, read| {
    let (value, lease) = read!(input.object_acl())?.into_parts();
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
