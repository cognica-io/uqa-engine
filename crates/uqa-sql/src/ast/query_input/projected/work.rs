//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The constructor owner polls one admitted task at a time. Children own their inputs and result channels; suspended parents never poll or own another task's future.

use super::{frame, Input, JsonReadError, JsonReadWork, Native, Projected, Read, Result};
use std::{
    future::{poll_fn, Future},
    pin::Pin,
    sync::{Arc, Mutex, Weak},
    task::Poll,
};
use uqa_core::memory::{BudgetedVec, MemoryReservation, Produced, ProductionControl};

struct Task<'a> {
    future: Pin<Box<dyn Future<Output = ()> + Send + 'a>>,
    _memory: Option<MemoryReservation>,
}

struct Queue<'a> {
    child: Option<Task<'a>>,
    checkpoint: bool,
}

type Shared<T> = Arc<Produced<Mutex<T>>>;
type Output<T> = Shared<Option<Result<Native<T>>>>;

/// Only the enclosing driver owns the queue. Task handles cannot retain a cycle or keep an abandoned constructor alive.
#[derive(Clone)]
pub(in crate::ast::query_input) struct Work<'a> {
    queue: Weak<Produced<Mutex<Queue<'a>>>>,
    control: ProductionControl<'a>,
}

fn shared<T>(value: T, control: ProductionControl<'_>) -> Result<Shared<T>> {
    Ok(control
        .finish(Mutex::new(value), control.empty_reservation())
        .map_err(JsonReadError::from)?
        .into_shared(&control)
        .map_err(JsonReadError::from)?)
}

impl<'a> Work<'a> {
    pub(super) fn submit<T: Projected>(&mut self, input: Input<'a>) -> Result<Output<T>> {
        let output = shared(None, self.control)?;
        let returned = Arc::clone(&output);
        let mut work = self.clone();
        let future = async move {
            let result = async {
                if T::BUFFERED && !input.buffered {
                    let prepared = input.buffer(&mut work).await?;
                    let result = input
                        .buffered_value(prepared.as_ref())
                        .run::<T, _>(&mut work)?
                        .await;
                    result
                } else {
                    T::read_value(input, &mut work)?.await
                }
            }
            .await;
            *output.lock().expect("private constructor output") = Some(result);
        };
        let memory = self
            .control
            .reserve(size_of_val(&future))
            .map_err(JsonReadError::from)?;
        let task = Task {
            future: Box::pin(future),
            _memory: memory,
        };
        let queue = self.queue.upgrade().expect("active constructor driver");
        let mut queue = queue.lock().expect("private constructor queue");
        assert!(
            queue.child.is_none() && !queue.checkpoint,
            "one pending constructor operation"
        );
        queue.child = Some(task);
        Ok(returned)
    }
}

impl JsonReadWork for Work<'_> {
    type Error = JsonReadError;

    async fn checkpoint(&mut self) -> std::result::Result<(), Self::Error> {
        {
            let queue = self.queue.upgrade().expect("active constructor driver");
            let mut queue = queue.lock().expect("private constructor queue");
            assert!(
                queue.child.is_none() && !queue.checkpoint,
                "one pending constructor operation"
            );
            queue.checkpoint = true;
        }
        let mut suspended = false;
        poll_fn(|_| {
            if suspended {
                Poll::Ready(())
            } else {
                suspended = true;
                Poll::Pending
            }
        })
        .await;
        Ok(())
    }
}

enum Tasks<'a> {
    Ordinary(Vec<Task<'a>>),
    Admitted(BudgetedVec<Task<'a>>),
}

impl<'a> Tasks<'a> {
    fn new(control: ProductionControl<'_>) -> Self {
        control.budget().map_or_else(
            || Self::Ordinary(Vec::new()),
            |budget| Self::Admitted(BudgetedVec::new(budget)),
        )
    }
    fn push(&mut self, task: Task<'a>) -> Result<()> {
        match self {
            Self::Ordinary(tasks) => {
                tasks
                    .try_reserve(1)
                    .map_err(uqa_core::memory::MemoryError::from)?;
                tasks.push(task);
            }
            Self::Admitted(tasks) => tasks.push(task)?,
        }
        Ok(())
    }
    fn last_mut(&mut self) -> Option<&mut Task<'a>> {
        match self {
            Self::Ordinary(tasks) => tasks.last_mut(),
            Self::Admitted(tasks) => tasks.last_mut(),
        }
    }
    fn pop(&mut self) {
        match self {
            Self::Ordinary(tasks) => {
                tasks.pop();
            }
            Self::Admitted(tasks) => {
                tasks.pop();
            }
        }
    }
}

enum Step<'a> {
    Child(Task<'a>),
    Checkpoint,
    Complete,
    Done,
}

pub(super) fn run<'a, 'w, T: Projected, W: JsonReadWork<Error = JsonReadError> + 'w>(
    input: Input<'a>,
    work: &'w mut W,
) -> Result<Read<'w, T>>
where
    'a: 'w,
{
    frame(input.control, async move {
        let queue = shared(
            Queue {
                child: None,
                checkpoint: false,
            },
            input.control,
        )?;
        let mut owner = Work {
            queue: Arc::downgrade(&queue),
            control: input.control,
        };
        let output = owner.submit::<T>(input)?;
        let mut tasks = Tasks::new(input.control);
        let first = queue
            .lock()
            .expect("private constructor queue")
            .child
            .take()
            .expect("initial constructor");
        tasks.push(first)?;
        loop {
            input.control.check().map_err(JsonReadError::from)?;
            let step = poll_fn(|context| {
                let Some(task) = tasks.last_mut() else {
                    return Poll::Ready(Step::Done);
                };
                let ready = task.future.as_mut().poll(context).is_ready();
                let mut queue = queue.lock().expect("private constructor queue");
                if let Some(child) = queue.child.take() {
                    assert!(!ready, "parent awaits its child");
                    Poll::Ready(Step::Child(child))
                } else if std::mem::take(&mut queue.checkpoint) {
                    assert!(!ready, "constructor awaits its work owner");
                    Poll::Ready(Step::Checkpoint)
                } else if ready {
                    Poll::Ready(Step::Complete)
                } else {
                    Poll::Pending
                }
            })
            .await;
            match step {
                Step::Child(child) => tasks.push(child)?,
                Step::Checkpoint => work.checkpoint().await?,
                Step::Complete => tasks.pop(),
                Step::Done => break,
            }
        }
        let result = output
            .lock()
            .expect("private constructor output")
            .take()
            .expect("completed constructor");
        result
    })
}

pub(super) async fn receive<T>(output: Output<T>) -> Result<Native<T>> {
    poll_fn(
        |_| match output.lock().expect("private constructor output").take() {
            Some(value) => Poll::Ready(value),
            None => Poll::Pending,
        },
    )
    .await
}
