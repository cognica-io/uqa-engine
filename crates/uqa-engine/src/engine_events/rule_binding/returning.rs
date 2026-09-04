//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rewrite-rule action `RETURNING` namespace binding.

use std::collections::BTreeSet;

use uqa_sql::ast::{ColumnType, Expr, Projection, ReturningAliases, RuleEvent, Statement};
use uqa_sql::plpgsql::{ResolvedVariable, VariableResolver};
use uqa_sql::SQLError;

pub(super) fn bind_rule_action_returning(
    returning: &[Projection],
    action_columns: &BTreeSet<String>,
    aliases: &ReturningAliases,
    action_event: RuleEvent,
    event_resolver: &mut dyn VariableResolver,
    context: &super::RuleBindingContext<'_>,
) -> Result<Vec<Projection>, SQLError> {
    super::bind_projections(
        returning,
        &mut RuleActionReturningResolver {
            action_columns,
            aliases,
            action_event,
            event_resolver,
        },
        &super::RuleBindingScope::default(),
        context,
    )
}

pub(crate) fn expand_rule_action_returning_stars(
    action: &Statement,
    action_columns: &[(String, ColumnType)],
) -> Statement {
    let mut expanded = action.clone();
    let (action_event, target_qualifier, aliases, returning) = match &mut expanded {
        Statement::Insert(statement) => (
            RuleEvent::Insert,
            statement.target_qualifier.clone(),
            statement.returning_aliases.clone(),
            &mut statement.returning,
        ),
        Statement::Update(statement) => (
            RuleEvent::Update,
            statement.target_qualifier.clone(),
            statement.returning_aliases.clone(),
            &mut statement.returning,
        ),
        Statement::Delete(statement) => (
            RuleEvent::Delete,
            statement.target_qualifier.clone(),
            statement.returning_aliases.clone(),
            &mut statement.returning,
        ),
        _ => return expanded,
    };
    let mut projections = Vec::with_capacity(returning.len());
    for projection in returning.iter() {
        let qualifier = match &projection.expr {
            Expr::Star => Some(target_qualifier.as_str()),
            Expr::QualifiedStar(qualifier)
                if qualifier.eq_ignore_ascii_case(&target_qualifier)
                    || is_action_image_alias(action_event, &aliases, qualifier) =>
            {
                Some(qualifier.as_str())
            }
            _ => None,
        };
        if let Some(qualifier) = qualifier {
            projections.extend(action_columns.iter().map(|(column, _)| Projection {
                expr: Expr::qualified_column(qualifier, column),
                alias: projection.alias.clone().or_else(|| Some(column.clone())),
            }));
        } else {
            projections.push(projection.clone());
        }
    }
    *returning = projections;
    expanded
}

pub(crate) fn rule_action_returning_references_target_column(
    engine: &crate::Engine,
    action: &Statement,
    column: &str,
) -> Result<bool, SQLError> {
    let Some((action_event, target_qualifier, aliases, returning)) = action_returning(action)
    else {
        return Ok(false);
    };
    let mut resolver = RuleActionTargetColumnResolver {
        action_event,
        target_qualifier,
        aliases,
        from: column,
        to: None,
        referenced: false,
    };
    super::bind_projections(
        returning,
        &mut resolver,
        &super::RuleBindingScope::default(),
        &super::RuleBindingContext::with_engine(engine),
    )?;
    Ok(resolver.referenced)
}

pub(crate) fn rename_rule_action_returning_target_column(
    engine: &crate::Engine,
    action: &mut Statement,
    from: &str,
    to: &str,
) -> Result<bool, SQLError> {
    let Some((action_event, target_qualifier, aliases, _)) = action_returning(action) else {
        return Ok(false);
    };
    let target_qualifier = target_qualifier.to_string();
    let aliases = aliases.clone();
    let returning = match action {
        Statement::Insert(statement) => &mut statement.returning,
        Statement::Update(statement) => &mut statement.returning,
        Statement::Delete(statement) => &mut statement.returning,
        _ => unreachable!("validated rule action changed statement kind"),
    };
    let mut resolver = RuleActionTargetColumnResolver {
        action_event,
        target_qualifier: &target_qualifier,
        aliases: &aliases,
        from,
        to: Some(to),
        referenced: false,
    };
    *returning = super::bind_projections(
        returning,
        &mut resolver,
        &super::RuleBindingScope::default(),
        &super::RuleBindingContext::with_engine(engine),
    )?;
    Ok(resolver.referenced)
}

struct RuleActionTargetColumnResolver<'a> {
    action_event: RuleEvent,
    target_qualifier: &'a str,
    aliases: &'a ReturningAliases,
    from: &'a str,
    to: Option<&'a str>,
    referenced: bool,
}

impl RuleActionTargetColumnResolver<'_> {
    fn owns_qualifier(&self, qualifier: &str) -> bool {
        qualifier.eq_ignore_ascii_case(self.target_qualifier)
            || is_action_image_alias(self.action_event, self.aliases, qualifier)
    }
}

impl VariableResolver for RuleActionTargetColumnResolver<'_> {
    fn resolve_name(&mut self, _name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        _qualifier: &str,
        _column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn rewrite_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<Expr>, SQLError> {
        if column != self.from || !self.owns_qualifier(qualifier) {
            return Ok(None);
        }
        self.referenced = true;
        Ok(self.to.map(|to| Expr::qualified_column(qualifier, to)))
    }
}

fn action_returning(
    action: &Statement,
) -> Option<(RuleEvent, &str, &ReturningAliases, &[Projection])> {
    match action {
        Statement::Insert(statement) => Some((
            RuleEvent::Insert,
            &statement.target_qualifier,
            &statement.returning_aliases,
            &statement.returning,
        )),
        Statement::Update(statement) => Some((
            RuleEvent::Update,
            &statement.target_qualifier,
            &statement.returning_aliases,
            &statement.returning,
        )),
        Statement::Delete(statement) => Some((
            RuleEvent::Delete,
            &statement.target_qualifier,
            &statement.returning_aliases,
            &statement.returning,
        )),
        _ => None,
    }
}

fn is_action_image_alias(
    action_event: RuleEvent,
    aliases: &ReturningAliases,
    qualifier: &str,
) -> bool {
    let old = qualifier.eq_ignore_ascii_case(&aliases.old);
    let new = qualifier.eq_ignore_ascii_case(&aliases.new);
    match action_event {
        RuleEvent::Insert => old || new,
        RuleEvent::Update | RuleEvent::Delete => {
            old && aliases.old_explicit || new && aliases.new_explicit
        }
        RuleEvent::Select => false,
    }
}

struct RuleActionReturningResolver<'a, 'resolver> {
    action_columns: &'a BTreeSet<String>,
    aliases: &'a ReturningAliases,
    action_event: RuleEvent,
    event_resolver: &'resolver mut dyn VariableResolver,
}

impl RuleActionReturningResolver<'_, '_> {
    fn is_action_image_alias(&self, qualifier: &str) -> bool {
        is_action_image_alias(self.action_event, self.aliases, qualifier)
    }

    fn reject_inaccessible_insert_event_row(&self, qualifier: &str) -> Result<(), SQLError> {
        if self.action_event == RuleEvent::Insert
            && !self.is_action_image_alias(qualifier)
            && (qualifier.eq_ignore_ascii_case("old") || qualifier.eq_ignore_ascii_case("new"))
        {
            return Err(super::super::validation::invalid_rule_action_reference(
                qualifier,
            ));
        }
        Ok(())
    }
}

impl VariableResolver for RuleActionReturningResolver<'_, '_> {
    fn resolve_name(&mut self, name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        self.reject_inaccessible_insert_event_row(name)?;
        if self.is_action_image_alias(name) {
            Ok(None)
        } else {
            self.event_resolver.resolve_name(name)
        }
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        self.reject_inaccessible_insert_event_row(qualifier)?;
        if self.is_action_image_alias(qualifier) && !self.action_columns.contains(column) {
            return Err(SQLError::UnknownColumn(format!("{qualifier}.{column}")));
        }
        if self.is_action_image_alias(qualifier) {
            Ok(None)
        } else {
            self.event_resolver.resolve_qualified(qualifier, column)
        }
    }

    fn resolve_param(&mut self, index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        self.event_resolver.resolve_param(index)
    }

    fn rewrite_name(&mut self, name: &str) -> Result<Option<Expr>, SQLError> {
        self.reject_inaccessible_insert_event_row(name)?;
        if self.is_action_image_alias(name) {
            Ok(None)
        } else {
            self.event_resolver.rewrite_name(name)
        }
    }

    fn rewrite_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<Expr>, SQLError> {
        self.reject_inaccessible_insert_event_row(qualifier)?;
        if self.is_action_image_alias(qualifier) {
            self.resolve_qualified(qualifier, column)?;
            Ok(None)
        } else {
            self.event_resolver.rewrite_qualified(qualifier, column)
        }
    }

    fn rewrite_qualified_star(&mut self, qualifier: &str) -> Result<Option<Vec<Expr>>, SQLError> {
        self.reject_inaccessible_insert_event_row(qualifier)?;
        if self.is_action_image_alias(qualifier) {
            Ok(None)
        } else {
            self.event_resolver.rewrite_qualified_star(qualifier)
        }
    }

    fn rewrite_qualified_whole_row(&mut self, qualifier: &str) -> Result<Option<Expr>, SQLError> {
        self.reject_inaccessible_insert_event_row(qualifier)?;
        if self.is_action_image_alias(qualifier) {
            Ok(None)
        } else {
            self.event_resolver.rewrite_qualified_whole_row(qualifier)
        }
    }

    fn rewrite_param(&mut self, index: usize) -> Result<Option<Expr>, SQLError> {
        self.event_resolver.rewrite_param(index)
    }

    fn rewrite_internal(
        &mut self,
        column: uqa_sql::ast::InternalColumnRef,
    ) -> Result<Option<Expr>, SQLError> {
        self.event_resolver.rewrite_internal(column)
    }
}
