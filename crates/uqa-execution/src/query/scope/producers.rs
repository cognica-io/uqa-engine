//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query ownership of shared CTE producers, independent of lexical reader handles.

use super::CteScope;
use crate::physical::{construction::OperatorBuild, physical_exec_error, with_cleanup};
use crate::query::cte::rows::{CteRows, SharedPrefix};
use crate::{ExecResult, PhysicalOperator};
use parking_lot::Mutex;
use std::cmp::Reverse;
use std::sync::{Arc, Weak};
use uqa_core::memory::{OwnedMap, Produced, ProductionControl};
use uqa_sql::SQLError;

mod production;
mod retained;

pub(super) enum CteProducers {
    Owned(Arc<Produced<ProducerList>>),
    Borrowed(Weak<Produced<ProducerList>>),
}

pub(super) struct ProducerList(Mutex<Option<OwnedMap<Reverse<usize>, SharedPrefix>>>);

impl Default for CteProducers {
    fn default() -> Self {
        Self::with_control(&ProductionControl::uncontrolled())
            .expect("uncontrolled CTE producer construction")
    }
}

// Retained inputs contain cloned lexical scopes. A strong reference here would make the producer own its own lifetime through those inputs.
impl Clone for CteProducers {
    fn clone(&self) -> Self {
        Self::Borrowed(match self {
            Self::Owned(producers) => Arc::downgrade(producers),
            Self::Borrowed(producers) => producers.clone(),
        })
    }
}

impl CteProducers {
    fn list(&self) -> Result<Arc<Produced<ProducerList>>, SQLError> {
        match self {
            Self::Owned(producers) => Some(Arc::clone(producers)),
            Self::Borrowed(producers) => producers.upgrade(),
        }
        .ok_or_else(|| SQLError::Internal("CTE producer owner has ended".into()))
    }

    fn close(&self) -> ExecResult<()> {
        let Self::Owned(producers) = self else {
            return Ok(());
        };
        // Retire the list before invoking callbacks and release its lock. Closing a producer may recursively close its own query and nested producers.
        let producers = producers.0.lock().take().unwrap_or_default();
        let mut result = Ok(());
        for (_, producer) in producers {
            result = with_cleanup(result, producer.close(), "close CTE producer");
        }
        result
    }

    fn finish<T>(&self, result: Result<T, SQLError>) -> Result<T, SQLError> {
        with_cleanup(
            result.map_err(crate::ExecError::SQL),
            self.close(),
            "close query CTE producers",
        )
        .map_err(physical_exec_error)
    }
}

impl Drop for CteProducers {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

impl<S: Clone> CteScope<S> {
    /// Publish only newly created producers here. Copying or restoring a lexical binding must not transfer its parent's ownership.
    #[cfg(test)]
    pub(crate) fn insert_producer(
        &mut self,
        name: String,
        input: Box<dyn PhysicalOperator>,
        budget: usize,
    ) -> Result<(), SQLError> {
        self.insert_producer_with_control(&name, input, budget, &ProductionControl::uncontrolled())
    }

    pub(crate) fn insert_producer_with_control(
        &mut self,
        name: &str,
        input: Box<dyn PhysicalOperator>,
        budget: usize,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        let (input, producers) = OperatorBuild::run(input, |_| self.producers.list())?;
        let rows = CteRows::from_producer_with_control(input, budget, control)?;
        let CteRows::Prefix(producer) = &rows else {
            unreachable!("a producer creates a prefix")
        };
        let result = (|| -> Result<(), SQLError> {
            let mut pending_rows = super::CteRowsMap::new();
            if !self.rows.contains_key(name) {
                pending_rows.insert_with_control(name, rows.clone(), control)?;
            }
            // The private row owners precede the lock guard, so failed admission releases the lock before closing an input that may run callbacks.
            let mut guard = producers.0.lock();
            let list = guard
                .as_mut()
                .ok_or_else(|| SQLError::Internal("CTE producer owner has ended".into()))?;
            let mut pending_producer = OwnedMap::new();
            let key = Reverse(list.len());
            if let Some(budget) = control.budget() {
                pending_producer.insert_budgeted(key, producer.clone(), budget)?;
            } else {
                pending_producer.insert(key, producer.clone());
            }
            control.check()?;
            list.append(pending_producer);
            drop(guard);
            if let Some(previous) = self.rows.get_mut(name) {
                *previous = rows.clone();
            } else {
                self.rows.append(pending_rows);
            }
            self.deferred_ctes.remove(name);
            self.definition_scopes.remove(name);
            self.folded_ctes.remove(name);
            self.non_returning_ctes.remove(name);
            Ok(())
        })();
        match result {
            Ok(()) => Ok(()),
            Err(error) => with_cleanup(
                Err(crate::ExecError::SQL(error)),
                producer.close(),
                "close rejected CTE producer",
            )
            .map_err(physical_exec_error),
        }
    }

    pub(crate) fn finish_cte_producers<T>(
        &self,
        result: Result<T, SQLError>,
    ) -> Result<T, SQLError> {
        self.producers.finish(result)
    }

    #[cfg(test)]
    pub(crate) fn with_cte_producers<T>(
        &mut self,
        owns_ctes: bool,
        run: impl FnOnce(&mut Self) -> Result<T, SQLError>,
    ) -> Result<T, SQLError> {
        self.with_cte_producers_with_control(owns_ctes, &ProductionControl::uncontrolled(), run)
    }

    pub(crate) fn with_cte_producers_with_control<T>(
        &mut self,
        owns_ctes: bool,
        control: &ProductionControl<'_>,
        run: impl FnOnce(&mut Self) -> Result<T, SQLError>,
    ) -> Result<T, SQLError> {
        if !owns_ctes {
            return run(self);
        }
        let frame = ProducerScope::with_control(self, control)?;
        let result = run(frame.scope);
        frame.scope.finish_cte_producers(result)
    }

    #[cfg(test)]
    pub(crate) fn retain_cte_producers<'a>(
        &mut self,
        owns_ctes: bool,
        build: impl FnOnce(&mut Self) -> Result<Option<Box<dyn PhysicalOperator + 'a>>, SQLError>,
    ) -> Result<Option<Box<dyn PhysicalOperator + 'a>>, SQLError> {
        self.retain_cte_producers_with_control(owns_ctes, &ProductionControl::uncontrolled(), build)
    }

    pub(crate) fn retain_cte_producers_with_control<'a>(
        &mut self,
        owns_ctes: bool,
        control: &ProductionControl<'_>,
        build: impl FnOnce(&mut Self) -> Result<Option<Box<dyn PhysicalOperator + 'a>>, SQLError>,
    ) -> Result<Option<Box<dyn PhysicalOperator + 'a>>, SQLError> {
        if !owns_ctes {
            return build(self);
        }
        let frame = ProducerScope::with_control(self, control)?;
        match build(frame.scope) {
            Ok(Some(input)) => {
                let producers = std::mem::replace(
                    &mut frame.scope.producers,
                    CteProducers::Borrowed(Weak::new()),
                );
                Ok(Some(retained::ProducerQuery::with_control(
                    input, producers, control,
                )?))
            }
            result => frame.scope.finish_cte_producers(result),
        }
    }
}

struct ProducerScope<'a, S: Clone> {
    scope: &'a mut CteScope<S>,
    previous: Option<CteProducers>,
}

impl<'a, S: Clone> ProducerScope<'a, S> {
    fn with_control(
        scope: &'a mut CteScope<S>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let producers = CteProducers::with_control(control)?;
        let previous = Some(std::mem::replace(&mut scope.producers, producers));
        Ok(Self { scope, previous })
    }
}

impl<S: Clone> Drop for ProducerScope<'_, S> {
    fn drop(&mut self) {
        self.scope.producers = self.previous.take().expect("enclosing producer scope");
    }
}

#[cfg(test)]
mod tests;
