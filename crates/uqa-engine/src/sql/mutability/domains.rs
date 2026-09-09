//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Domain coercions inherit the effects of their catalog-bound checks.

use uqa_sql::ast::ColumnType;

use super::{BTreeSet, Engine, MutabilityClassification, SQLError};

pub(super) fn named_type_coercion_may_mutate(
    engine: &Engine,
    name: &str,
    visiting_views: &mut BTreeSet<String>,
    visiting_routines: &mut BTreeSet<String>,
    classification: MutabilityClassification,
) -> Result<bool, SQLError> {
    match crate::sql::resolve_catalog_column_type(engine, name) {
        Some(ty) => domain_cast_may_mutate(
            engine,
            &ty,
            visiting_views,
            visiting_routines,
            classification,
        ),
        None => Ok(false),
    }
}

pub(super) fn routine_coercions_may_mutate(
    engine: &Engine,
    definition: &uqa_sql::ast::CreateFunction,
    visiting_views: &mut BTreeSet<String>,
    visiting_routines: &mut BTreeSet<String>,
    classification: MutabilityClassification,
) -> Result<bool, SQLError> {
    let return_type = match &definition.returns {
        uqa_sql::ast::FunctionReturns::Scalar { type_name }
        | uqa_sql::ast::FunctionReturns::SetOf { type_name } => Some(type_name.as_str()),
        uqa_sql::ast::FunctionReturns::None | uqa_sql::ast::FunctionReturns::Table => None,
    };
    for name in definition
        .params
        .iter()
        .map(|parameter| parameter.type_name.as_str())
        .chain(return_type)
    {
        if named_type_coercion_may_mutate(
            engine,
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
    engine: &Engine,
    ty: &ColumnType,
    visiting_views: &mut BTreeSet<String>,
    visiting_routines: &mut BTreeSet<String>,
    classification: MutabilityClassification,
) -> Result<bool, SQLError> {
    let ColumnType::Domain { oid, base, .. } = ty else {
        return match ty {
            ColumnType::Array(element) => domain_cast_may_mutate(
                engine,
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
            engine,
            base,
            visiting_views,
            visiting_routines,
            classification,
        )? {
            return Ok(true);
        }
        let Some(domain) = engine.domain_by_oid(*oid) else {
            return Ok(false);
        };
        for check in domain.definition.checks {
            let plan = uqa_planner::ExpressionPlan::lower(check.expression);
            let mut result = Ok(false);
            plan.scalar.visit(&mut |expression| {
                if matches!(result, Ok(false)) {
                    result = super::scalar_node_may_mutate_engine(
                        engine,
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
