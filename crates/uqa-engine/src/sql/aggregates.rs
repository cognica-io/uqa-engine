//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL aggregate planning, bounded execution, and finalization.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Seek, SeekFrom, Write};
use std::sync::Arc;

use uqa_core::{DecimalValue, Value};
use uqa_execution::{eval_scalar, ScalarEvalContext, ScalarExpr, ScalarOrder, SpillBuffer};
use uqa_planner::{ProjectionPlan, QueryBlockPlan};
use uqa_sql::expr::{cast_value, value_to_json_text};
use uqa_sql::{ResultRow, SQLError, SQLParam};

use crate::{Engine, SQLAggregateFunction, SQLAggregateState};

use super::scalar::PlanSubqueryArena;
use super::{core_value_to_json, CteScope, ScopedEngineHook};

const AGGREGATE_MERGE_FAN_IN: usize = 16;

mod accumulator;
mod adaptive;
mod analysis;
mod distinct;
mod executor;
mod finalize;
mod output;
mod partial_state;
mod projected;
mod projected_input;
mod registered_buffer;
mod rewrite;
mod sort_fallback;
mod value_buffer;

pub(in crate::sql) use accumulator::*;
pub(in crate::sql) use analysis::*;
pub(in crate::sql) use distinct::*;
pub(in crate::sql) use executor::PhysicalAggregateExecutor;
pub(in crate::sql) use finalize::*;
pub(in crate::sql) use registered_buffer::*;
pub(in crate::sql) use rewrite::*;
pub(in crate::sql) use value_buffer::*;

#[cfg(test)]
mod tests;
