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
use std::sync::{Arc, Weak};
use uqa_sql::SQLError;

mod retained;

pub(super) enum CteProducers {
    Owned(Arc<ProducerList>),
    Borrowed(Weak<ProducerList>),
}

pub(super) struct ProducerList(Mutex<Option<Vec<SharedPrefix>>>);

impl Default for CteProducers {
    fn default() -> Self {
        Self::Owned(Arc::new(ProducerList(Mutex::new(Some(Vec::new())))))
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
    fn list(&self) -> Result<Arc<ProducerList>, SQLError> {
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
        for producer in producers.into_iter().rev() {
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
    pub(crate) fn insert_producer(
        &mut self,
        name: String,
        input: Box<dyn PhysicalOperator>,
        budget: usize,
    ) -> Result<(), SQLError> {
        let (input, producers) = OperatorBuild::run(input, |_| self.producers.list())?;
        let (input, mut list) = OperatorBuild::run(input, |_| {
            let list = producers.0.lock();
            if list.is_some() {
                Ok(list)
            } else {
                Err(SQLError::Internal("CTE producer owner has ended".into()))
            }
        })?;
        let rows = CteRows::from_producer(input, budget);
        let CteRows::Prefix(producer) = &rows else {
            unreachable!("a producer creates a prefix")
        };
        list.as_mut()
            .expect("active producer list")
            .push(producer.clone());
        drop(list);
        self.insert_rows(name, rows);
        Ok(())
    }

    pub(crate) fn finish_cte_producers<T>(
        &self,
        result: Result<T, SQLError>,
    ) -> Result<T, SQLError> {
        self.producers.finish(result)
    }

    pub(crate) fn with_cte_producers<T>(
        &mut self,
        owns_ctes: bool,
        run: impl FnOnce(&mut Self) -> Result<T, SQLError>,
    ) -> Result<T, SQLError> {
        if !owns_ctes {
            return run(self);
        }
        let frame = ProducerScope::new(self);
        let result = run(frame.scope);
        frame.scope.finish_cte_producers(result)
    }

    pub(crate) fn retain_cte_producers<'a>(
        &mut self,
        owns_ctes: bool,
        build: impl FnOnce(&mut Self) -> Result<Option<Box<dyn PhysicalOperator + 'a>>, SQLError>,
    ) -> Result<Option<Box<dyn PhysicalOperator + 'a>>, SQLError> {
        if !owns_ctes {
            return build(self);
        }
        let frame = ProducerScope::new(self);
        match build(frame.scope) {
            Ok(Some(input)) => {
                let producers = std::mem::replace(
                    &mut frame.scope.producers,
                    CteProducers::Borrowed(Weak::new()),
                );
                Ok(Some(Box::new(retained::ProducerQuery::new(
                    input, producers,
                ))))
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
    fn new(scope: &'a mut CteScope<S>) -> Self {
        let previous = Some(std::mem::take(&mut scope.producers));
        Self { scope, previous }
    }
}

impl<S: Clone> Drop for ProducerScope<'_, S> {
    fn drop(&mut self) {
        self.scope.producers = self.previous.take().expect("enclosing producer scope");
    }
}

#[cfg(test)]
mod tests;
