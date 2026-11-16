//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native SQL constructors retain independent admitted futures. Only the driver polls them, so a suspended parent never adds a recursive poll frame or owns its child's future.

use std::{
    future::{poll_fn, Future},
    pin::Pin,
    sync::{Arc, Mutex, Weak},
    task::Poll,
};
use uqa_core::{
    memory::{BudgetedVec, MemoryReservation, Produced, ProductionControl},
    ValueRetentionError,
};

type Result<T> = std::result::Result<T, ValueRetentionError>;
type Shared<T> = Arc<Produced<Mutex<T>>>;
pub(crate) type Output<T> = Shared<Option<T>>;

pub(crate) struct RetainedFuture<F: Future + ?Sized> {
    future: Pin<Box<F>>,
    memory: Option<MemoryReservation>,
}

pub(crate) type Read<'a, T> = RetainedFuture<dyn Future<Output = T> + Send + 'a>;

pub(crate) fn retain<F: Future>(
    control: ProductionControl<'_>,
    future: F,
) -> Result<RetainedFuture<F>> {
    let memory = control.reserve(size_of_val(&future))?;
    Ok(RetainedFuture {
        future: Box::pin(future),
        memory,
    })
}

impl<'a, T> Read<'a, T> {
    pub(crate) fn new<F: Future<Output = T> + Send + 'a>(
        control: ProductionControl<'_>,
        future: F,
    ) -> Result<Self> {
        let future = retain(control, future)?;
        Ok(Self {
            future: future.future,
            memory: future.memory,
        })
    }
}

impl<F: Future + ?Sized> Future for RetainedFuture<F> {
    type Output = F::Output;
    fn poll(self: Pin<&mut Self>, context: &mut std::task::Context<'_>) -> Poll<F::Output> {
        self.get_mut().future.as_mut().poll(context)
    }
}

type Task<'a> = Read<'a, ()>;

struct Queue<'a> {
    child: Option<Task<'a>>,
    checkpoint: bool,
}

fn shared<T>(value: T, control: ProductionControl<'_>) -> Result<Shared<T>> {
    control
        .finish(Mutex::new(value), control.empty_reservation())?
        .into_shared(&control)
}

#[derive(Clone)]
pub(crate) struct Work<'a> {
    queue: Weak<Produced<Mutex<Queue<'a>>>>,
    control: ProductionControl<'a>,
}

impl<'a> Work<'a> {
    pub(crate) fn submit<T, F>(&mut self, constructor: impl FnOnce(Self) -> F) -> Result<Output<T>>
    where
        T: Send + 'a,
        F: Future<Output = T> + Send + 'a,
    {
        self.control.check()?;
        let output = shared(None, self.control)?;
        let returned = Arc::clone(&output);
        let work = self.clone();
        let constructor = constructor(work);
        let future = async move {
            let value = constructor.await;
            *output.lock().expect("private constructor output") = Some(value);
        };
        let task = Read::new(self.control, future)?;
        let queue = self.queue.upgrade().expect("active constructor driver");
        let mut queue = queue.lock().expect("private constructor queue");
        assert!(
            queue.child.is_none() && !queue.checkpoint,
            "one pending constructor operation"
        );
        queue.child = Some(task);
        Ok(returned)
    }

    pub(crate) async fn checkpoint(&mut self) {
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

pub(crate) struct Driver<'a> {
    tasks: Tasks<'a>,
    queue: Shared<Queue<'a>>,
    control: ProductionControl<'a>,
}

impl<'a> Driver<'a> {
    pub(crate) fn new(control: ProductionControl<'a>) -> Result<Self> {
        Ok(Self {
            tasks: Tasks::new(control),
            queue: shared(
                Queue {
                    child: None,
                    checkpoint: false,
                },
                control,
            )?,
            control,
        })
    }

    pub(crate) fn work(&self) -> Work<'a> {
        Work {
            queue: Arc::downgrade(&self.queue),
            control: self.control,
        }
    }

    /// Return at the next work boundary, or finish after every constructor has returned. The enclosing native input owns suspension, cancellation and operation publication.
    pub(crate) async fn advance(&mut self) -> Result<bool> {
        self.control.check()?;
        let step = poll_fn(|context| {
            if let Some(child) = self
                .queue
                .lock()
                .expect("private constructor queue")
                .child
                .take()
            {
                return Poll::Ready(Step::Child(child));
            }
            let Some(task) = self.tasks.last_mut() else {
                return Poll::Ready(Step::Done);
            };
            let ready = task.future.as_mut().poll(context).is_ready();
            let mut queue = self.queue.lock().expect("private constructor queue");
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
            Step::Child(child) => {
                self.tasks.push(child)?;
                Ok(true)
            }
            Step::Checkpoint => Ok(true),
            Step::Complete => {
                self.tasks.pop();
                Ok(true)
            }
            Step::Done => Ok(false),
        }
    }
}

pub(crate) async fn receive<T>(output: Output<T>) -> T {
    poll_fn(
        |_| match output.lock().expect("private constructor output").take() {
            Some(value) => Poll::Ready(value),
            None => Poll::Pending,
        },
    )
    .await
}

#[cfg(test)]
mod tests;
