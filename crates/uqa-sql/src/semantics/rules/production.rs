//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rewrite validation observes retained declarations and owns only the pending relation names and traversal buffers it constructs.

use super::{RuleCatalog, RuleEvent, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl},
    RelationIdentity, RelationName, RelationReference,
};

mod ctes;
mod insert;
pub use ctes::validate_rule_cte_contract_with_control;
pub use insert::validate_insert_rule_query_contract_with_control;

pub(super) fn copy_relation(
    relation: &RelationIdentity,
    control: &ProductionControl<'_>,
) -> Result<Produced<RelationIdentity>, SQLError> {
    let schema = control.copy_text(&relation.schema)?;
    let name = control.copy_text(&relation.name)?;
    let (schema, schema_memory) = schema.into_parts();
    let (name, name_memory) = name.into_parts();
    Ok(control.finish(
        RelationIdentity { schema, name },
        control.combine(schema_memory, name_memory),
    )?)
}

fn qualified_name(
    relation: &RelationIdentity,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    Ok(RelationReference {
        schema: Some(RelationName::decoded(&relation.schema)),
        name: RelationName::decoded(&relation.name),
    }
    .render_with_control(control)?)
}

pub fn validate_rule_returning_contract_with_control(
    catalog: &dyn RuleCatalog,
    table: &str,
    event: RuleEvent,
    requested: bool,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    if !requested {
        return Ok(());
    }
    let relation = catalog.resolve_rule_relation_with_control(table, control)?;
    let table = qualified_name(&relation, control)?;
    let mut instead = false;
    let mut providers = 0;
    catalog.visit_rules_with_control(&table, event, false, control, &mut |rule| {
        instead |= rule.definition.instead;
        for action in &rule.definition.actions {
            control.check()?;
            providers += usize::from(super::statement_has_returning(action));
        }
        Ok(())
    })?;
    if !instead || providers == 1 {
        return Ok(());
    }
    if providers > 1 {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "cannot have RETURNING lists in multiple rules".into(),
        });
    }
    let event = super::rule_event_name(event);
    Err(SQLError::Diagnostic {
        sqlstate: "0A000".into(),
        message: format!(
            "cannot perform {event} RETURNING on relation \"{}\"",
            relation.name
        ),
        detail: None,
        hint: Some(format!(
            "You need an unconditional ON {event} DO INSTEAD rule with a RETURNING clause."
        )),
    })
}
