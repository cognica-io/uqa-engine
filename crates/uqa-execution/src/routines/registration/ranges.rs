//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated internal constructor routines share the normal catalog, overloads, OID allocation and dependencies.

use super::{register, BoundRoutine, RoutineBody, RoutineRegistrationContext};
use uqa_sql::{
    ast::{
        CreateFunction, FunctionBody, FunctionParallel, FunctionParam, FunctionParamMode,
        FunctionReturns, FunctionVolatility, RoutineAttributeClauses, RoutineSecurityAttributes,
    },
    catalog::range_type::StoredRange,
    ColumnType, SQLError,
};

pub(crate) fn register_range_constructors(
    context: &RoutineRegistrationContext<'_>,
    definition: &StoredRange,
) -> Result<[u32; 5], SQLError> {
    let range = definition.column_type();
    let multi = definition.multirange_type();
    let array = ColumnType::Array(Box::new(range.clone()));
    let subtype = &definition.subtype;
    let signatures: [(&str, &[&ColumnType]); 5] = [
        ("range_constructor2", &[subtype, subtype]),
        ("range_constructor3", &[subtype, subtype, &ColumnType::Text]),
        ("multirange_constructor0", &[]),
        ("multirange_constructor1", &[&range]),
        ("multirange_constructor2", &[&array]),
    ];
    let mut identities = [0; 5];
    for (index, (source, arguments)) in signatures.into_iter().enumerate() {
        let multirange = index >= 2;
        let (identity, result) = if multirange {
            (&definition.multirange.identity, &multi)
        } else {
            (&definition.identity, &range)
        };
        let params = arguments
            .iter()
            .map(|ty| FunctionParam {
                name: String::new(),
                type_name: ty.catalog_name(),
                type_reference: None,
                written_type: None,
                mode: if index == 4 {
                    FunctionParamMode::Variadic
                } else {
                    FunctionParamMode::In
                },
                default: None,
                default_type: None,
            })
            .collect();
        let routine = CreateFunction {
            object_id: None,
            catalog_revision: None,
            catalog_oid: None,
            name: identity.qualified_name(),
            or_replace: false,
            is_procedure: false,
            params,
            returns: FunctionReturns::Scalar {
                type_name: result.catalog_name(),
            },
            return_type_reference: None,
            return_written_type: None,
            language: "internal".into(),
            body: FunctionBody::Source(source.into()),
            body_origin: None,
            sql_body_form: None,
            creation_search_path: Vec::new(),
            volatility: FunctionVolatility::Immutable,
            strict: multirange,
            owner: Some(uqa_core::catalog_role::RoleIdentity::BOOTSTRAP),
            security: RoutineSecurityAttributes::default(),
            parallel: FunctionParallel::Safe,
            support: None,
            cost: Some(1.0),
            rows: None,
            config: Vec::new(),
            config_actions: Vec::new(),
            attribute_clauses: RoutineAttributeClauses::default(),
            execute_acl: None,
        };
        let published = register(
            context,
            routine,
            Some(BoundRoutine {
                body: RoutineBody::Source,
                validated: None,
                changed: false,
            }),
        )?;
        identities[index] = published
            .def
            .catalog_oid
            .ok_or_else(|| SQLError::Internal("generated constructor has no catalog OID".into()))?;
    }
    Ok(identities)
}
