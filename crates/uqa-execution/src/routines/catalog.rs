//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine registry snapshots, retained write guards, and durable publication.

use std::ops::DerefMut;
use uqa_sql::{routines::lifecycle::RoutineRegistry, SQLError};

pub type RoutineRegistryWrite<'a> = Box<dyn DerefMut<Target = RoutineRegistry> + 'a>;
pub trait RoutineRegistryState {
    fn routine_snapshot(&self) -> RoutineRegistry;
    fn routines_write(&self) -> RoutineRegistryWrite<'_>;
}
pub trait RoutineRegistryPublication {
    fn persist_routine_definitions(&self, registry: &RoutineRegistry) -> Result<(), SQLError>;
}
