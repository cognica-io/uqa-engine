//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rewrite-rule action `RETURNING` namespace binding.

use std::collections::BTreeSet;

use uqa_sql::ast::{Expr, Projection, ReturningAliases, RuleEvent};
use uqa_sql::plpgsql::{ResolvedVariable, VariableResolver};
use uqa_sql::SQLError;

pub(super) fn bind_rule_action_returning(
    returning: &[Projection],
    action_columns: &BTreeSet<String>,
    aliases: &ReturningAliases,
    action_event: RuleEvent,
    event_resolver: &mut dyn VariableResolver,
) -> Result<Vec<Projection>, SQLError> {
    super::bind_projections(
        returning,
        &mut RuleActionReturningResolver {
            action_columns,
            aliases,
            action_event,
            event_resolver,
        },
        &BTreeSet::new(),
    )
}

struct RuleActionReturningResolver<'a, 'resolver> {
    action_columns: &'a BTreeSet<String>,
    aliases: &'a ReturningAliases,
    action_event: RuleEvent,
    event_resolver: &'resolver mut dyn VariableResolver,
}

impl RuleActionReturningResolver<'_, '_> {
    fn is_action_image_alias(&self, qualifier: &str) -> bool {
        let old = qualifier.eq_ignore_ascii_case(&self.aliases.old);
        let new = qualifier.eq_ignore_ascii_case(&self.aliases.new);
        match self.action_event {
            RuleEvent::Insert => old || new,
            RuleEvent::Update | RuleEvent::Delete => {
                old && self.aliases.old_explicit || new && self.aliases.new_explicit
            }
            RuleEvent::Select => false,
        }
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
    fn resolve_name(&mut self, _name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
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

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
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
}
