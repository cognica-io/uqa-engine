//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::cell::Cell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rusqlite::Connection;

use super::{StorageUpgradeEvent, StorageUpgradeObserver, StorageUpgradeStep};

/// How many instructions `SQLite` runs between two looks at the clock.
const INSTRUCTIONS: i32 = 1 << 20;
/// A step that is at work tells so at most this often.
const INTERVAL: Duration = Duration::from_secs(1);

/// Tells the observer of an open what the upgrade of the native mapping
/// does.
///
/// An open without an observer, and every initialization that is no open,
/// uses [`UpgradeReport::silent`]: it tells nothing and installs nothing on
/// the connection.
pub(crate) struct UpgradeReport {
    observer: Option<Arc<dyn StorageUpgradeObserver>>,
    /// When the upgrade began. `None` for a database that is current.
    started: Cell<Option<Instant>>,
    instructions: i32,
    interval: Duration,
}

/// Removes the progress handler of a step when the step ends, however it
/// ends.
struct Advancing<'connection>(&'connection Connection);

impl Drop for Advancing<'_> {
    fn drop(&mut self) {
        let _ = self.0.progress_handler(0, None::<fn() -> bool>);
    }
}

impl UpgradeReport {
    pub(crate) const fn silent() -> Self {
        Self::to(None)
    }

    pub(crate) const fn to(observer: Option<Arc<dyn StorageUpgradeObserver>>) -> Self {
        Self::every(observer, INSTRUCTIONS, INTERVAL)
    }

    /// A report whose steps look at the clock every `instructions` and tell that they are
    /// at work at most once in `interval`.
    const fn every(
        observer: Option<Arc<dyn StorageUpgradeObserver>>,
        instructions: i32,
        interval: Duration,
    ) -> Self {
        Self {
            observer,
            started: Cell::new(None),
            instructions,
            interval,
        }
    }

    /// The database is stored in the mapping `from`, and its upgrade to `to`
    /// begins.
    pub(crate) fn started(&self, from: u32, to: u32) {
        self.started.set(Some(Instant::now()));
        self.tell(&StorageUpgradeEvent::Started { from, to });
    }

    /// Runs `work`, a step that writes `rows` rows again, and tells the
    /// observer that it began, that it is at work and that it completed. A
    /// step that fails is not told as complete: its error ends the open.
    ///
    /// The connection of an initialization has no progress handler of its
    /// own, so the step's handler replaces none and is removed with the
    /// step.
    pub(crate) fn step<R, E>(
        &self,
        connection: &Connection,
        step: StorageUpgradeStep,
        rows: Option<u64>,
        work: impl FnOnce() -> Result<R, E>,
    ) -> Result<R, E> {
        let Some(observer) = &self.observer else {
            return work();
        };
        observer.observe(&StorageUpgradeEvent::StepStarted { step, rows });
        let began = Instant::now();
        let advancing = Arc::clone(observer);
        let (every, interval) = (self.instructions, self.interval);
        let mut instructions = 0_u64;
        let mut told = began;
        // A connection that refuses the handler still runs the step; its
        // observer is then told the beginning and the end alone.
        let _ = connection.progress_handler(
            every,
            Some(move || {
                instructions = instructions.saturating_add(every.unsigned_abs().into());
                if told.elapsed() >= interval {
                    told = Instant::now();
                    advancing.observe(&StorageUpgradeEvent::StepAdvanced { step, instructions });
                }
                // Returning true would interrupt the statement.
                false
            }),
        );
        let removed = Advancing(connection);
        let result = work();
        drop(removed);
        if result.is_ok() {
            observer.observe(&StorageUpgradeEvent::StepFinished {
                step,
                duration: began.elapsed(),
            });
        }
        result
    }

    /// The transaction that holds the upgrade is about to be committed. Tells
    /// nothing for a database that was current.
    pub(crate) fn committing(&self) {
        if self.started.get().is_some() {
            self.tell(&StorageUpgradeEvent::Committing);
        }
    }

    /// The transaction is committed. Tells nothing for a database that was
    /// current.
    pub(crate) fn finished(&self) {
        if let Some(started) = self.started.take() {
            self.tell(&StorageUpgradeEvent::Finished {
                duration: started.elapsed(),
            });
        }
    }

    fn tell(&self, event: &StorageUpgradeEvent) {
        if let Some(observer) = &self.observer {
            observer.observe(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct Told(Mutex<Vec<StorageUpgradeEvent>>);

    impl StorageUpgradeObserver for Told {
        fn observe(&self, event: &StorageUpgradeEvent) {
            self.0.lock().unwrap().push(event.clone());
        }
    }

    impl Told {
        fn events(&self) -> Vec<StorageUpgradeEvent> {
            self.0.lock().unwrap().clone()
        }
    }

    /// A statement that runs some hundred thousand instructions and reads no table.
    const WORK: &str = "WITH RECURSIVE counted(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM counted WHERE n < 20000) SELECT count(*) FROM counted";

    fn work(connection: &Connection) -> rusqlite::Result<i64> {
        connection.query_row(WORK, [], |row| row.get(0))
    }

    #[test]
    fn a_step_is_told_as_it_begins_while_it_works_and_when_it_is_complete() {
        let connection = Connection::open_in_memory().unwrap();
        let told = Arc::new(Told::default());
        let report = UpgradeReport::every(Some(told.clone()), 64, Duration::ZERO);
        let step = StorageUpgradeStep::LookupKeys;
        let counted = report
            .step(&connection, step, Some(20_000), || work(&connection))
            .unwrap();
        assert_eq!(counted, 20_000);
        let events = told.events();
        assert_eq!(
            events.first(),
            Some(&StorageUpgradeEvent::StepStarted {
                step,
                rows: Some(20_000)
            })
        );
        assert!(matches!(
            events.last(),
            Some(StorageUpgradeEvent::StepFinished { step: finished, .. }) if *finished == step
        ));
        let advanced: Vec<u64> = events[1..events.len() - 1]
            .iter()
            .map(|event| match event {
                StorageUpgradeEvent::StepAdvanced {
                    step: advanced,
                    instructions,
                } if *advanced == step => *instructions,
                other => panic!("{other:?} between the beginning and the end of a step"),
            })
            .collect();
        // The statement runs far more than a thousand instructions, and each look at the
        // clock is told because the interval is nothing.
        assert!(advanced.len() > 16, "{}", advanced.len());
        assert!(advanced.windows(2).all(|pair| pair[1] == pair[0] + 64));
        assert_eq!(advanced[0], 64);

        // The handler leaves with the step: the same work afterwards tells nothing.
        work(&connection).unwrap();
        assert_eq!(told.events().len(), events.len());
    }

    #[test]
    fn a_step_tells_that_it_is_at_work_at_most_once_in_its_interval() {
        let connection = Connection::open_in_memory().unwrap();
        let told = Arc::new(Told::default());
        let report = UpgradeReport::every(Some(told.clone()), 64, Duration::from_secs(3_600));
        report
            .step(&connection, StorageUpgradeStep::LookupKeys, None, || {
                work(&connection)
            })
            .unwrap();
        assert_eq!(told.events().len(), 2, "{:?}", told.events());
    }

    #[test]
    fn a_step_that_fails_is_not_told_as_complete_and_leaves_no_handler() {
        let connection = Connection::open_in_memory().unwrap();
        let told = Arc::new(Told::default());
        let report = UpgradeReport::every(Some(told.clone()), 64, Duration::ZERO);
        let step = StorageUpgradeStep::SequenceValues;
        let failed: Result<(), &str> =
            report.step(&connection, step, None, || Err("the disk is full"));
        assert_eq!(failed, Err("the disk is full"));
        assert_eq!(
            told.events(),
            [StorageUpgradeEvent::StepStarted { step, rows: None }]
        );
        work(&connection).unwrap();
        assert_eq!(told.events().len(), 1);
    }

    #[test]
    fn a_commit_is_told_only_for_a_database_that_was_upgraded() {
        let told = Arc::new(Told::default());
        let report = UpgradeReport::to(Some(told.clone()));
        // A database that is current: nothing began, so its commit is nobody's news.
        report.committing();
        report.finished();
        assert_eq!(told.events(), []);

        report.started(17, 18);
        report.committing();
        report.finished();
        // The end is told once, whoever commits.
        report.finished();
        let events = told.events();
        assert_eq!(events.len(), 3, "{events:?}");
        assert_eq!(
            events[..2],
            [
                StorageUpgradeEvent::Started { from: 17, to: 18 },
                StorageUpgradeEvent::Committing
            ]
        );
        assert!(matches!(events[2], StorageUpgradeEvent::Finished { .. }));
    }

    #[test]
    fn a_report_that_nobody_observes_runs_the_step_and_installs_nothing() {
        let connection = Connection::open_in_memory().unwrap();
        let report = UpgradeReport::silent();
        report.started(17, 18);
        let counted = report
            .step(&connection, StorageUpgradeStep::LookupKeys, None, || {
                work(&connection)
            })
            .unwrap();
        assert_eq!(counted, 20_000);
        report.committing();
        report.finished();
    }
}
