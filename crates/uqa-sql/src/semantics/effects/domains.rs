//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Domain coercions inherit the effects of their context-bound checks.

use crate::ast::ColumnType;

use super::{BTreeSet, MutabilityClassification, QueryEffectContext, SQLError};

pub(super) fn named_type_coercion_may_mutate(
    context: &QueryEffectContext<'_>,
    name: &str,
    visiting_views: &mut BTreeSet<String>,
    visiting_routines: &mut BTreeSet<String>,
    classification: MutabilityClassification,
) -> Result<bool, SQLError> {
    match crate::expr::EngineHook::resolve_type_name(context.catalog, name)
        .ok()
        .flatten()
    {
        Some(ty) => domain_cast_may_mutate(
            context,
            &ty,
            visiting_views,
            visiting_routines,
            classification,
        ),
        None => Ok(false),
    }
}

pub(super) fn routine_coercions_may_mutate(
    context: &QueryEffectContext<'_>,
    definition: &crate::ast::CreateFunction,
    visiting_views: &mut BTreeSet<String>,
    visiting_routines: &mut BTreeSet<String>,
    classification: MutabilityClassification,
) -> Result<bool, SQLError> {
    let return_type = match &definition.returns {
        crate::ast::FunctionReturns::Scalar { type_name }
        | crate::ast::FunctionReturns::SetOf { type_name } => Some(type_name.as_str()),
        crate::ast::FunctionReturns::None | crate::ast::FunctionReturns::Table => None,
    };
    for name in definition
        .params
        .iter()
        .map(|parameter| parameter.type_name.as_str())
        .chain(return_type)
    {
        if named_type_coercion_may_mutate(
            context,
            name,
            visiting_views,
            visiting_routines,
            classification,
        )? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn domain_cast_may_mutate(
    context: &QueryEffectContext<'_>,
    ty: &ColumnType,
    visiting_views: &mut BTreeSet<String>,
    visiting_routines: &mut BTreeSet<String>,
    classification: MutabilityClassification,
) -> Result<bool, SQLError> {
    let ColumnType::Domain { oid, base, .. } = ty else {
        return match ty {
            ColumnType::Array(element) => domain_cast_may_mutate(
                context,
                element,
                visiting_views,
                visiting_routines,
                classification,
            ),
            _ => Ok(false),
        };
    };
    let key = format!("domain:{oid}");
    if !visiting_routines.insert(key.clone()) {
        return Ok(false);
    }
    let result = (|| {
        if domain_cast_may_mutate(
            context,
            base,
            visiting_views,
            visiting_routines,
            classification,
        )? {
            return Ok(true);
        }
        let Some(domain) = context.catalog.domain_by_oid(*oid) else {
            return Ok(false);
        };
        for check in domain.definition.checks {
            let plan = crate::plan::ExpressionPlan::lower(check.expression);
            let mut result = Ok(false);
            plan.scalar.visit(&mut |expression| {
                if matches!(result, Ok(false)) {
                    result = super::scalar_node_may_mutate_engine(
                        context,
                        expression,
                        visiting_views,
                        visiting_routines,
                        classification,
                    );
                }
            });
            if result? {
                return Ok(true);
            }
        }
        Ok(false)
    })();
    visiting_routines.remove(&key);
    result
}
