//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine bodies a session compiled at their first call, as each `PostgreSQL` backend compiles a function the first time it calls it and keeps it until the function's definition changes.

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use uqa_sql::routines::{CompiledFunctionBody, SQLUserFunction};

/// A compiled body and the definition it was compiled from, whose weak handle holds the definition's address for it.
type RetainedBody = (Weak<SQLUserFunction>, Arc<CompiledFunctionBody>);

/// The bodies this session compiled, each kept for exactly the definition it was compiled from: a replaced or altered definition is a new one, which its next call compiles again.
#[derive(Default)]
pub(crate) struct CompiledRoutineBodies {
    /// Keyed by the address of the definition, so an entry never describes another definition at the same address.
    bodies: Mutex<HashMap<usize, RetainedBody>>,
}

impl CompiledRoutineBodies {
    pub(crate) fn get(&self, function: &Arc<SQLUserFunction>) -> Option<Arc<CompiledFunctionBody>> {
        self.bodies
            .lock()
            .get(&definition_key(function))
            .map(|(_, body)| Arc::clone(body))
    }

    /// Keep `body` for `function`, and forget the bodies of definitions nothing holds any longer.
    pub(crate) fn retain(&self, function: &Arc<SQLUserFunction>, body: Arc<CompiledFunctionBody>) {
        let mut bodies = self.bodies.lock();
        bodies.retain(|_, (definition, _)| definition.strong_count() > 0);
        bodies.insert(definition_key(function), (Arc::downgrade(function), body));
    }
}

fn definition_key(function: &Arc<SQLUserFunction>) -> usize {
    Arc::as_ptr(function) as usize
}
