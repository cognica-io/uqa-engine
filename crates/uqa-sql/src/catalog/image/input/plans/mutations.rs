//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::scalars::References;
use super::{driver::Step, Fields, Input, Kind, Node, PlanMemory, SQLError};
use crate::plan::{CommandPlan, QueryPlan, SourcePlan};

mod attachment;
mod constructors;
mod fields;

macro_rules! input_fields {
    ($input:ident, $read:ident $(, $async:ident)?) => {
        impl $input<'_> {
            #[expect(
                clippy::too_many_lines,
                reason = "four native mutation schemas retain their original ordered fields and separate rewrite arenas"
            )]
            pub(super) $($async)? fn plan_command_fields(
                &mut self,
                command: &mut CommandPlan,
                position: usize,
                count: &mut Option<usize>,
                references: &mut References,
                rule_references: &mut References,
                memory: &mut PlanMemory,
            ) -> Result<Step, SQLError> {
                if position == 0 {
                    *command = $read!(self.plan_command_node(memory))?;
                    return Ok(Step::Advance);
                }
                match command {
                    CommandPlan::Insert(value) => match position {
                        1 => value.table = $read!(self.plan_text(memory))?,
                        2 => value.target_relation_bound = $read!(self.flag())?,
                        3 => value.relations_bound = $read!(self.flag())?,
                        4 => value.statement_privilege_subject = $read!(self.plan_role(memory))?,
                        5 => value.target_privilege_subject = $read!(self.plan_role(memory))?,
                        6 => value.target_qualifier = $read!(self.plan_text(memory))?,
                        7 => value.target_alias = $read!(self.plan_optional_text(memory))?,
                        8 => value.include_descendants = $read!(self.flag())?,
                        9 => value.columns = $read!(self.plan_targets(memory, references))?.take(),
                        10 => value.overriding = $read!(self.plan_overriding())?,
                        11 => {
                            return $read!(self.plan_children(
                                memory,
                                count,
                                &mut value.ctes,
                                Kind::Cte
                            ));
                        }
                        12 => value.rows = $read!(self.plan_rows(memory, references))?.take(),
                        13 => {
                            return $read!(
                                self.plan_optional_box::<QueryPlan>(memory, Kind::Query)
                            );
                        }
                        14 => value.rule_values_input = $read!(self.plan_rule_values(memory))?,
                        15 => {
                            value.on_conflict =
                                $read!(self.plan_conflict(memory, references))?.take()
                        }
                        16 => {
                            value.returning =
                                $read!(self.plan_projections(memory, references))?.take()
                        }
                        17 => {
                            value.returning_aliases = $read!(self.plan_returning_aliases(memory))?
                        }
                        18 => {
                            return $read!(self.plan_children(
                                memory,
                                count,
                                &mut value.subqueries,
                                Kind::Query
                            ));
                        }
                        19 => {
                            value.view_checks =
                                $read!(self.plan_view_checks(memory, references))?.take()
                        }
                        20 => value.view_rule_relations = $read!(self.plan_texts(memory))?,
                        21 => {
                            value.view_rule_insert_plans = $read!(self.plan_rule_inserts(memory))?
                        }
                        22 => {
                            return $read!(self.plan_rule_returning(
                                &mut value.view_rule_returning,
                                count,
                                rule_references,
                                memory
                            ));
                        }
                        _ => return Ok(Step::Done),
                    },
                    CommandPlan::Update(value) => match position {
                        1 => value.table = $read!(self.plan_text(memory))?,
                        2 => value.target_relation_bound = $read!(self.flag())?,
                        3 => value.relations_bound = $read!(self.flag())?,
                        4 => value.statement_privilege_subject = $read!(self.plan_role(memory))?,
                        5 => value.target_privilege_subject = $read!(self.plan_role(memory))?,
                        6 => value.target_qualifier = $read!(self.plan_text(memory))?,
                        7 => value.target_alias = $read!(self.plan_optional_text(memory))?,
                        8 => value.include_descendants = $read!(self.flag())?,
                        9 => {
                            value.assignments =
                                $read!(self.plan_assignments(memory, references))?.take()
                        }
                        10 => {
                            value.predicate =
                                $read!(self.plan_optional_scalar(memory, references))?.take()
                        }
                        11 => {
                            return $read!(self.plan_children(
                                memory,
                                count,
                                &mut value.ctes,
                                Kind::Cte
                            ));
                        }
                        12 => {
                            return $read!(
                                self.plan_optional_box::<SourcePlan>(memory, Kind::Source)
                            );
                        }
                        13 => {
                            value.returning =
                                $read!(self.plan_projections(memory, references))?.take()
                        }
                        14 => {
                            value.returning_aliases = $read!(self.plan_returning_aliases(memory))?
                        }
                        15 => {
                            return $read!(self.plan_children(
                                memory,
                                count,
                                &mut value.subqueries,
                                Kind::Query
                            ));
                        }
                        16 => {
                            value.view_checks =
                                $read!(self.plan_view_checks(memory, references))?.take()
                        }
                        17 => value.view_rule_relations = $read!(self.plan_texts(memory))?,
                        18 => {
                            value.view_rule_update_plans = $read!(self.plan_rule_updates(memory))?
                        }
                        19 => {
                            return $read!(self.plan_rule_returning(
                                &mut value.view_rule_returning,
                                count,
                                rule_references,
                                memory
                            ));
                        }
                        _ => return Ok(Step::Done),
                    },
                    CommandPlan::Delete(value) => match position {
                        1 => value.table = $read!(self.plan_text(memory))?,
                        2 => value.target_relation_bound = $read!(self.flag())?,
                        3 => value.relations_bound = $read!(self.flag())?,
                        4 => value.statement_privilege_subject = $read!(self.plan_role(memory))?,
                        5 => value.target_privilege_subject = $read!(self.plan_role(memory))?,
                        6 => value.target_qualifier = $read!(self.plan_text(memory))?,
                        7 => value.target_alias = $read!(self.plan_optional_text(memory))?,
                        8 => value.include_descendants = $read!(self.flag())?,
                        9 => {
                            value.predicate =
                                $read!(self.plan_optional_scalar(memory, references))?.take()
                        }
                        10 => {
                            return $read!(self.plan_children(
                                memory,
                                count,
                                &mut value.ctes,
                                Kind::Cte
                            ));
                        }
                        11 => {
                            return $read!(
                                self.plan_optional_box::<SourcePlan>(memory, Kind::Source)
                            );
                        }
                        12 => {
                            value.returning =
                                $read!(self.plan_projections(memory, references))?.take()
                        }
                        13 => {
                            value.returning_aliases = $read!(self.plan_returning_aliases(memory))?
                        }
                        14 => {
                            return $read!(self.plan_children(
                                memory,
                                count,
                                &mut value.subqueries,
                                Kind::Query
                            ));
                        }
                        15 => value.view_rule_relations = $read!(self.plan_texts(memory))?,
                        16 => {
                            return $read!(self.plan_rule_returning(
                                &mut value.view_rule_returning,
                                count,
                                rule_references,
                                memory
                            ));
                        }
                        _ => return Ok(Step::Done),
                    },
                    CommandPlan::Merge(value) => match position {
                        1 => {
                            return $read!(self.plan_children(
                                memory,
                                count,
                                &mut value.ctes,
                                Kind::Cte
                            ));
                        }
                        2 => value.target = $read!(self.plan_text(memory))?,
                        3 => value.statement_privilege_subject = $read!(self.plan_role(memory))?,
                        4 => value.target_privilege_subject = $read!(self.plan_role(memory))?,
                        5 => value.target_qualifier = $read!(self.plan_text(memory))?,
                        6 => value.target_alias = $read!(self.plan_optional_text(memory))?,
                        7 => value.include_descendants = $read!(self.flag())?,
                        8 => {
                            value.target_predicate =
                                $read!(self.plan_optional_scalar(memory, references))?.take()
                        }
                        9 => return Ok(Step::Child(Kind::Source)),
                        10 => {
                            value.join_condition =
                                $read!(self.plan_scalar(memory, references))?.take()
                        }
                        11 => {
                            value.when_clauses =
                                $read!(self.plan_when_clauses(memory, references))?.take()
                        }
                        12 => {
                            value.returning =
                                $read!(self.plan_projections(memory, references))?.take()
                        }
                        13 => {
                            value.returning_aliases = $read!(self.plan_returning_aliases(memory))?
                        }
                        14 => {
                            return $read!(self.plan_children(
                                memory,
                                count,
                                &mut value.subqueries,
                                Kind::Query
                            ));
                        }
                        15 => {
                            value.view_checks =
                                $read!(self.plan_view_checks(memory, references))?.take()
                        }
                        _ => return Ok(Step::Done),
                    },
                    _ => unreachable!("query mutation constructor"),
                }
                Ok(Step::Advance)
            }
        }
    };
}

input_fields!(Input, read_field);
input_fields!(Fields, await_field, async);

pub(super) use attachment::attach;
