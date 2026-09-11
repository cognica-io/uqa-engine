//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! FROM/JOIN row assembly, table functions, and projection intercepts.

use uqa_planner::QueryPlan;
use uqa_sql::{SQLError, SQLParam};

use crate::Engine;

use super::select::{CteScope, QueryOutput};

mod lateral;

pub(in crate::sql) use lateral::*;
