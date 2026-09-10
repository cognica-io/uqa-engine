//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind INSERT SELECT preparation to the source query's read generation.
use super::InsertSelectConsumer;
use crate::query::{consumer::QueryRowConsumer, statement::consumer::QueryConsumerFactory};
use std::rc::Rc;
use uqa_sql::SQLError;

/// The session owner retains a live or frozen generation for the lifetime of one bound sink.
pub trait InsertSelectBinding<S: Clone + 'static> {
    fn bind<'a>(
        &'a self,
        snapshot: Option<&S>,
        consumer: Rc<InsertSelectConsumer<S>>,
    ) -> Result<Rc<dyn QueryRowConsumer + 'a>, SQLError>;
}
pub struct InsertSelectOutput<'a, S: Clone + 'static> {
    pub binding: &'a dyn InsertSelectBinding<S>,
    pub consumer: Rc<InsertSelectConsumer<S>>,
}
impl<'consumer, S: Clone + 'static> QueryConsumerFactory<'consumer, S>
    for InsertSelectOutput<'consumer, S>
{
    fn bind(
        self: Rc<Self>,
        generation: Option<&S>,
    ) -> Result<Rc<dyn QueryRowConsumer + 'consumer>, SQLError> {
        self.binding.bind(generation, Rc::clone(&self.consumer))
    }
}
