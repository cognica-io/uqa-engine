//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A deferred shared substitution is copied before mutation; callbacks still observe the original analyzed syntax identities.

use super::{
    replacements::Replacements, DeferredOuterRewrite, ExpressionPlan, OuterReplacement, SQLError,
    ScalarExpr,
};
use crate::plan::{OwnedScalarExpr, QueryPlan, ScalarNodeTransform};
use std::ptr::NonNull;
use uqa_core::memory::{Produced, ProductionControl, ProductionMap, ProductionVec};

impl DeferredOuterRewrite {
    pub(crate) fn transform_scalar_nodes(
        &mut self,
        control: &ProductionControl<'_>,
        transform: &mut ScalarNodeTransform<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        if self.replacements.is_empty() {
            return Ok(());
        }
        let mut output = ProductionVec::new(*control);
        for (identity, replacement) in self.replacements.iter() {
            let identity = crate::ColumnIdentity::from_parts_with_control(
                identity.qualifier(),
                identity.column(),
                control,
            )?;
            let qualifier = control.copy_text(&replacement.qualifier)?;
            let scalar =
                scalar(&replacement.expression.scalar, control, transform)?.finish(control)?;
            let mut queries = ProductionVec::new(*control);
            queries.reserve(replacement.expression.subqueries.len())?;
            for source in &replacement.expression.subqueries {
                queries.push_produced(query(source, control, transform)?)?;
            }
            let queries = queries.finish()?;
            let (identity, identity_memory) = identity.into_parts();
            let (qualifier, qualifier_memory) = qualifier.into_parts();
            let (scalar, scalar_memory) = scalar.into_parts();
            let (subqueries, queries_memory) = queries.into_parts();
            let replacement = OuterReplacement {
                expression: ExpressionPlan { scalar, subqueries },
                qualifier,
                memory: control.combine(
                    identity_memory,
                    control.combine(
                        qualifier_memory,
                        control.combine(scalar_memory, queries_memory),
                    ),
                ),
            };
            let entry = control
                .finish((identity, replacement), control.empty_reservation())?
                .into_shared(control)?;
            output.push_produced(control.finish(entry, control.empty_reservation())?)?;
        }
        self.replacements = Replacements::Controlled(output.finish()?.into_shared(control)?);
        Ok(())
    }
}

fn scalar(
    source: &ScalarExpr,
    control: &ProductionControl<'_>,
    transform: &mut ScalarNodeTransform<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    let output = source.copy_for_rewrite(control)?;
    let mut originals = ProductionVec::new(*control);
    source.try_visit(&mut |node| {
        originals.push_copy(NonNull::from(node))?;
        Ok::<_, SQLError>(true)
    })?;
    let original_root = originals[0];
    let mut index = 0;
    let mut identities = ProductionMap::new(*control);
    output.try_visit(&mut |node| {
        if index != 0 {
            identities.insert_copy(NonNull::from(node), originals[index])?;
        }
        index += 1;
        Ok::<_, SQLError>(true)
    })?;
    assert_eq!(index, originals.len(), "scalar copy preserves node layout");
    let mut remaining = index;
    let output = output.transform_nodes(control, &mut |identity, expression| {
        remaining -= 1;
        let original = if remaining == 0 {
            original_root
        } else {
            *identities
                .get(&identity)
                .expect("copied scalar child identity")
        };
        transform(original, expression)
    })?;
    assert_eq!(
        remaining, 0,
        "scalar transformation visits every original node once"
    );
    Ok(output)
}

fn query(
    source: &QueryPlan,
    control: &ProductionControl<'_>,
    transform: &mut ScalarNodeTransform<'_>,
) -> Result<Produced<QueryPlan>, SQLError> {
    let output = source.copy_for_rewrite(control)?;
    let mut originals = ProductionVec::new(*control);
    let mut status = Ok(());
    source.visit_scalar_expressions(&mut |expression| {
        if status.is_ok() {
            status = expression.try_visit(&mut |node| {
                originals.push_copy(NonNull::from(node))?;
                Ok::<_, SQLError>(true)
            });
        }
    });
    status?;
    let mut status = Ok(());
    let mut identities = ProductionMap::new(*control);
    let mut index = 0;
    output.visit_scalar_expressions(&mut |expression| {
        if status.is_ok() {
            status = expression.try_visit(&mut |node| {
                identities.insert_copy(NonNull::from(node), originals[index])?;
                index += 1;
                Ok::<_, SQLError>(true)
            });
        }
    });
    status?;
    assert_eq!(index, originals.len(), "query copy preserves scalar layout");
    output
        .transform_scalar_nodes(control, &mut |identity, expression| {
            let original = *identities
                .get(&identity)
                .expect("copied query scalar identity");
            transform(original, expression)
        })?
        .finish(control)
}
