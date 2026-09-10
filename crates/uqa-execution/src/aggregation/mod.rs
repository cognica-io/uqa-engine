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

use crate::{eval_scalar, ScalarEvalContext, ScalarExpr, ScalarOrder, SpillBuffer};
use uqa_core::{DecimalValue, Value};
use uqa_sql::expr::{cast_value, value_to_json_text};
use uqa_sql::plan::QueryBlockPlan;
use uqa_sql::{SQLError, SQLParam};

use crate::functions::{SQLAggregateFunction, SQLAggregateState};

use crate::scalar::plan::{PlanSubqueryArena, QueryExpressionContext};
use uqa_sql::expr::core_value_to_json;

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
mod string_agg;
mod value_buffer;

pub use accumulator::*;
pub use analysis::*;
pub use distinct::*;
pub use executor::PhysicalAggregateExecutor;
pub use finalize::*;
pub use registered_buffer::*;
pub use rewrite::*;
pub use value_buffer::*;

#[cfg(test)]
mod tests;
