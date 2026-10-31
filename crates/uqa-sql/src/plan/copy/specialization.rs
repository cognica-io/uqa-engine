//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Concrete routine declarations choose their replacement types before copying any destination field.

use super::{syntax::SyntaxCopy, Copying, Result};
use crate::ast::{CreateFunction, FunctionParam, FunctionReturns, RoutineInvocationBinding};
use uqa_core::memory::{Produced, ProductionControl};

pub(crate) fn copy_specialized_function(
    definition: &CreateFunction,
    invocation: &RoutineInvocationBinding,
    control: &ProductionControl<'_>,
) -> Result<Produced<CreateFunction>> {
    control.check()?;
    let mut copying = Copying::new(*control).expect("controlled concrete declaration");
    let parameters = copying.map(
        definition.params.iter().zip(&invocation.parameter_types),
        |copying, (parameter, type_name)| {
            Ok(FunctionParam {
                name: parameter.name.copy_syntax(copying)?,
                type_name: type_name.copy_syntax(copying)?,
                type_reference: parameter.type_reference.copy_syntax(copying)?,
                written_type: parameter.written_type.copy_syntax(copying)?,
                mode: parameter.mode,
                default: parameter.default.copy_syntax(copying)?,
                default_type: parameter.default_type.copy_syntax(copying)?,
            })
        },
    )?;
    let returns = match (&definition.returns, invocation.return_type.as_ref()) {
        (FunctionReturns::Scalar { .. }, Some(name)) => FunctionReturns::Scalar {
            type_name: name.copy_syntax(&mut copying)?,
        },
        (FunctionReturns::SetOf { .. }, Some(name)) => FunctionReturns::SetOf {
            type_name: name.copy_syntax(&mut copying)?,
        },
        (returns, _) => returns.copy_syntax(&mut copying)?,
    };
    let result = CreateFunction {
        object_id: definition.object_id,
        catalog_revision: definition.catalog_revision,
        catalog_oid: definition.catalog_oid,
        name: definition.name.copy_syntax(&mut copying)?,
        or_replace: definition.or_replace,
        is_procedure: definition.is_procedure,
        params: parameters,
        returns,
        return_type_reference: definition.return_type_reference.copy_syntax(&mut copying)?,
        return_written_type: definition.return_written_type.copy_syntax(&mut copying)?,
        language: definition.language.copy_syntax(&mut copying)?,
        body: definition.body.copy_syntax(&mut copying)?,
        body_origin: definition.body_origin.copy_syntax(&mut copying)?,
        sql_body_form: definition.sql_body_form,
        creation_search_path: definition.creation_search_path.copy_syntax(&mut copying)?,
        volatility: definition.volatility,
        strict: definition.strict,
        owner: definition.owner,
        security: definition.security.copy_syntax(&mut copying)?,
        parallel: definition.parallel.copy_syntax(&mut copying)?,
        support: definition.support.copy_syntax(&mut copying)?,
        cost: definition.cost,
        rows: definition.rows,
        config: definition.config.copy_syntax(&mut copying)?,
        config_actions: definition.config_actions.copy_syntax(&mut copying)?,
        attribute_clauses: definition.attribute_clauses.copy_syntax(&mut copying)?,
        execute_acl: definition.execute_acl.copy_syntax(&mut copying)?,
    };
    copying.finish(result)
}
