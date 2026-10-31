//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Construct CURRENT, OLD and NEW images from borrowed descriptors before replacing a provider's RETURNING list.

use super::{BTreeSet, RuleEvent, RuleReturningImages, RuleReturningRequest, SQLError, Statement};
use crate::{
    ast::{Expr, Projection, ReturningAliases},
    plan::copy::{SyntaxProduction, SyntaxVector},
    plpgsql::{ResolvedVariable, RewriteReference, VariableResolver},
};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

pub fn augment_rule_returning_action(
    statement: &mut Statement,
    source_index: Option<Expr>,
    event_width: usize,
    request: RuleReturningRequest,
    target_columns: &BTreeSet<String>,
) -> Result<(), SQLError> {
    if let Some(output) = produce_rule_returning(
        statement,
        source_index.as_ref(),
        event_width,
        request,
        target_columns,
        &ProductionControl::uncontrolled(),
    )? {
        *super::super::action_binding::returning_mut(statement).expect("DML RETURNING provider") =
            output.into_parts().0;
    }
    Ok(())
}

pub(in crate::semantics::rules) fn produce_rule_returning(
    statement: &Statement,
    source_index: Option<&Expr>,
    event_width: usize,
    request: RuleReturningRequest,
    target_columns: &dyn crate::semantics::rules::action_binding::ActionColumns,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<Projection>>>, SQLError> {
    control.check()?;
    let (target_qualifier, aliases, returning, event) = match statement {
        Statement::Insert(action) => (
            &action.target_qualifier,
            &action.returning_aliases,
            &action.returning,
            RuleEvent::Insert,
        ),
        Statement::Update(action) => (
            &action.target_qualifier,
            &action.returning_aliases,
            &action.returning,
            RuleEvent::Update,
        ),
        Statement::Delete(action) => (
            &action.target_qualifier,
            &action.returning_aliases,
            &action.returning,
            RuleEvent::Delete,
        ),
        _ => return Ok(None),
    };
    if returning.is_empty() {
        return Ok(None);
    }
    let production = SyntaxProduction::new(*control);
    let mut output = production.vector(0)?;
    if request.images.contains(RuleReturningImages::CURRENT) {
        for projection in returning {
            output.push(production.copy(projection)?)?;
        }
    } else {
        append_nulls(&mut output, event_width)?;
    }
    let scope = ImageScope {
        target_qualifier,
        target_columns,
        aliases,
    };
    for (image, allowed, qualifier) in [
        (
            RuleReturningImages::OLD,
            event != RuleEvent::Insert,
            aliases.old.as_str(),
        ),
        (
            RuleReturningImages::NEW,
            event != RuleEvent::Delete,
            aliases.new.as_str(),
        ),
    ] {
        if request.images.contains(image) && allowed {
            let mut resolver = ReturningImageResolver {
                scope,
                image_qualifier: qualifier,
            };
            for projection in returning {
                output.push(Projection {
                    expr: rewrite_image(&projection.expr, &mut resolver, &production)?,
                    alias: production.copy(&projection.alias)?,
                })?;
            }
        } else {
            append_nulls(&mut output, event_width)?;
        }
    }
    if let Some(expression) = source_index {
        output.push(Projection {
            expr: production.copy(expression)?,
            alias: None,
        })?;
    }
    let output = output.finish()?;
    Ok(Some(production.finish(output)?))
}

fn append_nulls(
    output: &mut SyntaxVector<'_, '_, Projection>,
    width: usize,
) -> Result<(), SQLError> {
    for _ in 0..width {
        output.push(Projection {
            expr: Expr::Literal(Value::Null),
            alias: None,
        })?;
    }
    Ok(())
}

fn rewrite_image(
    expr: &Expr,
    resolver: &mut ReturningImageResolver<'_>,
    production: &SyntaxProduction<'_>,
) -> Result<Expr, SQLError> {
    match expr {
        Expr::Star => Ok(Expr::QualifiedStar(
            production.text(resolver.image_qualifier)?.into(),
        )),
        Expr::QualifiedStar(qualifier) if resolver.retargets_qualifier(qualifier) => {
            Ok(Expr::QualifiedStar(
                crate::ast::ReferenceName::from(production.text(resolver.image_qualifier)?)
                    .with_position(qualifier.position()),
            ))
        }
        expr => Ok(production.take(
            super::super::action_binding::bind_rule_expr_scoped_with_control(
                expr,
                resolver,
                &BTreeSet::new(),
                &production.control(),
            )?,
        )),
    }
}

#[derive(Clone, Copy)]
struct ImageScope<'a> {
    target_qualifier: &'a str,
    target_columns: &'a dyn crate::semantics::rules::action_binding::ActionColumns,
    aliases: &'a ReturningAliases,
}

struct ReturningImageResolver<'a> {
    scope: ImageScope<'a>,
    image_qualifier: &'a str,
}

impl ReturningImageResolver<'_> {
    fn retargets_qualifier(&self, qualifier: &str) -> bool {
        qualifier.eq_ignore_ascii_case(self.scope.target_qualifier)
            || qualifier.eq_ignore_ascii_case(&self.scope.aliases.old)
            || qualifier.eq_ignore_ascii_case(&self.scope.aliases.new)
    }
}

impl VariableResolver for ReturningImageResolver<'_> {
    fn resolve_name(&mut self, _: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }
    fn resolve_qualified(
        &mut self,
        _: &str,
        _: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }
    fn resolve_param(&mut self, _: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn rewrite_with_control(
        &mut self,
        reference: RewriteReference<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Expr>>, SQLError> {
        control.check()?;
        let column = match reference {
            RewriteReference::Name(name)
                if self.scope.target_columns.contains(name)
                    || name == crate::semantics::DOC_ID_COLUMN =>
            {
                name
            }
            RewriteReference::Qualified { qualifier, column }
                if self.retargets_qualifier(qualifier) =>
            {
                column
            }
            _ => return Ok(None),
        };
        let production = SyntaxProduction::new(*control);
        let value = Expr::QualifiedColumn {
            qualifier: production.text(self.image_qualifier)?,
            column: production.text(column)?.into(),
        };
        Ok(Some(production.finish(value)?))
    }
}
