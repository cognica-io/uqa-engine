//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    Engine, SQLError, TransactionCharacteristicsState, TransactionFrame, TransactionIntent,
};
use uqa_sql::ast::{TransactionCharacteristics, TransactionIsolationLevel};

fn session_value(
    values: &std::collections::BTreeMap<String, String>,
    name: &str,
) -> Option<String> {
    values.get(name).cloned().or_else(|| {
        values
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    })
}

fn parse_boolean_parameter(name: &str, value: &str) -> Result<bool, SQLError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "yes" | "1" => Ok(true),
        "off" | "false" | "no" | "0" => Ok(false),
        _ => Err(SQLError::Routine {
            sqlstate: "22023".into(),
            message: format!("parameter \"{name}\" requires a Boolean value"),
        }),
    }
}

fn parse_isolation_parameter(
    name: &str,
    value: &str,
) -> Result<TransactionIsolationLevel, SQLError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "read uncommitted" => Ok(TransactionIsolationLevel::ReadUncommitted),
        "read committed" => Ok(TransactionIsolationLevel::ReadCommitted),
        "repeatable read" => Ok(TransactionIsolationLevel::RepeatableRead),
        "serializable" => Ok(TransactionIsolationLevel::Serializable),
        _ => Err(SQLError::Routine {
            sqlstate: "22023".into(),
            message: format!("invalid value for parameter \"{name}\": \"{value}\""),
        }),
    }
}

fn transaction_error(message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: "25001".into(),
        message: message.into(),
    }
}

impl TransactionCharacteristicsState {
    fn with_options(mut self, options: TransactionCharacteristics) -> Self {
        if let Some(isolation) = options.isolation {
            self.isolation = isolation;
        }
        if let Some(read_only) = options.read_only {
            self.read_only = read_only;
        }
        if let Some(deferrable) = options.deferrable {
            self.deferrable = deferrable;
        }
        self
    }
}

impl Engine {
    pub(super) fn default_transaction_characteristics(&self) -> TransactionCharacteristicsState {
        let session = self.session.state.read();
        let isolation = session_value(&session.session_vars, "default_transaction_isolation")
            .and_then(|value| {
                parse_isolation_parameter("default_transaction_isolation", &value).ok()
            })
            .unwrap_or(TransactionIsolationLevel::ReadCommitted);
        let read_only = session_value(&session.session_vars, "default_transaction_read_only")
            .and_then(|value| parse_boolean_parameter("default_transaction_read_only", &value).ok())
            .unwrap_or(false);
        let deferrable = session_value(&session.session_vars, "default_transaction_deferrable")
            .and_then(|value| {
                parse_boolean_parameter("default_transaction_deferrable", &value).ok()
            })
            .unwrap_or(false);
        TransactionCharacteristicsState {
            isolation,
            read_only,
            deferrable,
        }
    }

    pub(super) fn transaction_characteristics_for_begin(
        &self,
        stack: &[TransactionFrame],
        options: TransactionCharacteristics,
    ) -> TransactionCharacteristicsState {
        stack
            .last()
            .map_or_else(
                || self.default_transaction_characteristics(),
                |frame| frame.characteristics,
            )
            .with_options(options)
    }

    pub(super) fn apply_transaction_characteristics(
        stack: &mut [TransactionFrame],
        options: TransactionCharacteristics,
    ) -> Result<(), SQLError> {
        let nested_frame = stack.len() > 1;
        let enclosing_read_only = stack
            .get(..stack.len().saturating_sub(1))
            .and_then(|parents| parents.last())
            .is_some_and(|frame| frame.characteristics.read_only);
        let Some(frame) = stack.last_mut() else {
            // A standalone SET TRANSACTION runs in PostgreSQL's implicit
            // transaction and vanishes at statement end.
            return Ok(());
        };
        let is_subtransaction = nested_frame || !frame.savepoints.is_empty();
        if let Some(isolation) = options.isolation {
            if isolation != frame.characteristics.isolation {
                if frame.first_snapshot_set {
                    return Err(transaction_error(
                        "SET TRANSACTION ISOLATION LEVEL must be called before any query",
                    ));
                }
                if is_subtransaction {
                    return Err(transaction_error(
                        "SET TRANSACTION ISOLATION LEVEL must not be called in a subtransaction",
                    ));
                }
                frame.characteristics.isolation = isolation;
            }
        }
        if let Some(read_only) = options.read_only {
            if read_only != frame.characteristics.read_only {
                if !read_only && enclosing_read_only {
                    return Err(transaction_error(
                        "cannot set transaction read-write mode inside a read-only transaction",
                    ));
                }
                if !read_only && is_subtransaction && frame.characteristics.read_only {
                    return Err(transaction_error(
                        "cannot set transaction read-write mode inside a read-only transaction",
                    ));
                }
                if !read_only && frame.first_snapshot_set {
                    return Err(transaction_error(
                        "transaction read-write mode must be set before any query",
                    ));
                }
                frame.characteristics.read_only = read_only;
                if !read_only {
                    frame.intent = TransactionIntent::ReadWrite;
                }
            }
        }
        if let Some(deferrable) = options.deferrable {
            if is_subtransaction {
                return Err(transaction_error(
                    "SET TRANSACTION [NOT] DEFERRABLE cannot be called within a subtransaction",
                ));
            }
            if frame.first_snapshot_set {
                return Err(transaction_error(
                    "SET TRANSACTION [NOT] DEFERRABLE must be called before any query",
                ));
            }
            frame.characteristics.deferrable = deferrable;
        }
        Ok(())
    }

    pub(super) fn set_session_transaction_characteristics(
        &self,
        options: TransactionCharacteristics,
    ) {
        let mut session = self.session.state.write();
        if let Some(isolation) = options.isolation {
            session.session_vars.insert(
                "default_transaction_isolation".into(),
                isolation.as_str().into(),
            );
        }
        if let Some(read_only) = options.read_only {
            session.session_vars.insert(
                "default_transaction_read_only".into(),
                if read_only { "on" } else { "off" }.into(),
            );
        }
        if let Some(deferrable) = options.deferrable {
            session.session_vars.insert(
                "default_transaction_deferrable".into(),
                if deferrable { "on" } else { "off" }.into(),
            );
        }
    }

    pub(crate) fn set_transaction_parameter(
        &self,
        name: &str,
        value: &str,
    ) -> Result<(), SQLError> {
        let options = if name.eq_ignore_ascii_case("transaction_isolation") {
            TransactionCharacteristics {
                isolation: Some(parse_isolation_parameter(name, value)?),
                ..TransactionCharacteristics::default()
            }
        } else if name.eq_ignore_ascii_case("transaction_read_only") {
            TransactionCharacteristics {
                read_only: Some(parse_boolean_parameter(name, value)?),
                ..TransactionCharacteristics::default()
            }
        } else if name.eq_ignore_ascii_case("transaction_deferrable") {
            TransactionCharacteristics {
                deferrable: Some(parse_boolean_parameter(name, value)?),
                ..TransactionCharacteristics::default()
            }
        } else {
            return Err(SQLError::Internal(format!(
                "set_transaction_parameter called for {name:?}"
            )));
        };
        let _statement = self.runtime.statement_gate.lock();
        let mut stack = self.session.transactions.lock();
        Self::apply_transaction_characteristics(&mut stack, options)
    }

    pub(crate) fn validate_default_transaction_parameter(
        name: &str,
        value: &str,
    ) -> Result<String, SQLError> {
        if name.eq_ignore_ascii_case("default_transaction_isolation") {
            return parse_isolation_parameter(name, value).map(|value| value.as_str().into());
        }
        if name.eq_ignore_ascii_case("default_transaction_read_only")
            || name.eq_ignore_ascii_case("default_transaction_deferrable")
        {
            return parse_boolean_parameter(name, value)
                .map(|value| if value { "on" } else { "off" }.into());
        }
        Ok(value.to_string())
    }

    pub(crate) fn transaction_parameter_value(&self, name: &str) -> Option<String> {
        let current = self.session.transactions.lock().last().map_or_else(
            || self.default_transaction_characteristics(),
            |frame| frame.characteristics,
        );
        if name.eq_ignore_ascii_case("transaction_isolation") {
            return Some(current.isolation.as_str().into());
        }
        if name.eq_ignore_ascii_case("transaction_read_only") {
            return Some(if current.read_only { "on" } else { "off" }.into());
        }
        if name.eq_ignore_ascii_case("transaction_deferrable") {
            return Some(if current.deferrable { "on" } else { "off" }.into());
        }
        None
    }

    pub(crate) fn current_transaction_is_read_only(&self) -> bool {
        self.session.transactions.lock().last().map_or_else(
            || self.default_transaction_characteristics().read_only,
            |frame| frame.characteristics.read_only,
        )
    }

    pub(crate) fn current_transaction_uses_fixed_snapshot(&self) -> bool {
        self.session
            .transactions
            .lock()
            .last()
            .is_some_and(|frame| {
                matches!(
                    frame.characteristics.isolation,
                    TransactionIsolationLevel::RepeatableRead
                        | TransactionIsolationLevel::Serializable
                )
            })
    }

    pub(crate) fn mark_transaction_snapshot_set(&self) {
        // PostgreSQL's FirstSnapshotSet belongs to the top transaction. A nested Engine frame models a subtransaction and must not acquire a newer snapshot merely because the first query happened below it.
        if let Some(frame) = self.session.transactions.lock().first_mut() {
            frame.first_snapshot_set = true;
        }
    }

    pub(super) fn set_transaction_snapshot(
        stack: &mut [TransactionFrame],
        snapshot: &str,
    ) -> Result<(), SQLError> {
        let is_subtransaction = stack.len() > 1;
        let Some(frame) = stack.last() else {
            return Err(transaction_error(
                "SET TRANSACTION SNAPSHOT must be called within a transaction",
            ));
        };
        if is_subtransaction || !frame.savepoints.is_empty() {
            return Err(transaction_error(
                "SET TRANSACTION SNAPSHOT must be called before any query",
            ));
        }
        if frame.first_snapshot_set {
            return Err(transaction_error(
                "SET TRANSACTION SNAPSHOT must be called before any query",
            ));
        }
        if !matches!(
            frame.characteristics.isolation,
            TransactionIsolationLevel::RepeatableRead | TransactionIsolationLevel::Serializable
        ) {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "a snapshot-importing transaction must have isolation level SERIALIZABLE or REPEATABLE READ".into(),
            });
        }
        let mut parts = snapshot.split('-');
        let valid = parts.next().is_some_and(|part| {
            !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) && parts.next().is_some_and(|part| {
            !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) && parts.next().is_some_and(|part| {
            !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) && parts.next().is_none();
        if !valid {
            return Err(SQLError::Routine {
                sqlstate: "22023".into(),
                message: format!("invalid snapshot identifier: \"{snapshot}\""),
            });
        }
        Err(SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("snapshot \"{snapshot}\" does not exist"),
        })
    }
}
