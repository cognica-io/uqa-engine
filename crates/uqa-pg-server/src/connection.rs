//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;
use std::io;
use std::net::TcpStream;
use std::sync::atomic::Ordering;

use uqa_engine::Engine;
use uqa_pg_wire::{
    decode_frontend_with_max, decode_startup_with_max, Authentication, BackendKeyData,
    BackendMessage, CancelKey, ErrorOrNotice, FrontendMessage, NoticeSeverity,
    NotificationResponse, ProtocolVersion, StartupFrame, TransactionStatus,
};
use uqa_sql::SQLError;

use crate::results::{send_notices, send_result, sql_error};
use crate::server::{Client, ServerConfig, Shared};
use crate::startup::{self, REPORTED_PARAMETERS};
use crate::transport::Transport;
use crate::ServerError;

struct Session<'a> {
    engine: Engine,
    shared: &'a Shared,
    process_id: i32,
    reported: BTreeMap<String, String>,
}

impl Drop for Session<'_> {
    fn drop(&mut self) {
        self.shared.clients.lock().remove(&self.process_id);
        self.engine.reset_cancellation();
        let _ = self.engine.close();
    }
}

pub(crate) fn serve(
    socket: TcpStream,
    database: &Engine,
    config: &ServerConfig,
    shared: &Shared,
) -> Result<(), ServerError> {
    let mut transport = Transport::new(socket);
    let result = run_connection(&mut transport, database, config, shared);
    if let Err(ServerError::Protocol(error)) = &result {
        let mut response = ErrorOrNotice::error("08P01", error.to_string());
        response.severity = NoticeSeverity::Fatal;
        let _ = transport.send(&BackendMessage::ErrorResponse(response));
    }
    result
}

fn run_connection(
    transport: &mut Transport,
    database: &Engine,
    config: &ServerConfig,
    shared: &Shared,
) -> Result<(), ServerError> {
    let Some(mut session) = startup(transport, database, config, shared)? else {
        return Ok(());
    };
    let mut discard_until_sync = false;
    loop {
        let message = match transport
            .read(|bytes| decode_frontend_with_max(bytes, config.max_message_bytes))
        {
            Ok(Some(message)) => message,
            Ok(None) => return Ok(()),
            Err(error) if is_read_timeout(&error) => {
                send_notifications(transport, &session.engine)?;
                continue;
            }
            Err(error) => return Err(error),
        };
        if matches!(message, FrontendMessage::Terminate) {
            return Ok(());
        }
        if discard_until_sync && !matches!(message, FrontendMessage::Sync) {
            continue;
        }
        match message {
            FrontendMessage::Query(query) => execute_query(transport, &mut session, &query)?,
            FrontendMessage::Sync => {
                discard_until_sync = false;
                ready(transport, &session.engine)?;
            }
            FrontendMessage::Flush => {}
            _ => {
                transport.send(&BackendMessage::ErrorResponse(ErrorOrNotice::error(
                    "0A000",
                    "extended query execution is not implemented",
                )))?;
                discard_until_sync = true;
            }
        }
    }
}

fn startup<'a>(
    transport: &mut Transport,
    database: &Engine,
    config: &ServerConfig,
    shared: &'a Shared,
) -> Result<Option<Session<'a>>, ServerError> {
    loop {
        let startup = match transport
            .read(|bytes| decode_startup_with_max(bytes, config.max_message_bytes))
        {
            Ok(Some(startup)) => startup,
            Ok(None) => return Ok(None),
            Err(error) if is_read_timeout(&error) => continue,
            Err(error) => return Err(error),
        };
        match startup {
            StartupFrame::SSLRequest | StartupFrame::GSSEncRequest => {
                transport.reject_encryption()?;
            }
            StartupFrame::CancelRequest {
                process_id,
                secret_key,
            } => {
                let clients = shared.clients.lock();
                if let Some(client) = clients.get(&process_id) {
                    if client.executing && client.secret == secret_key {
                        client.cancellation.cancel();
                    }
                }
                return Ok(None);
            }
            StartupFrame::Startup(startup) => {
                let negotiation = startup.negotiate_with_max(config.max_protocol_version, &[])?;
                transport.version = negotiation.negotiated_version;
                if let Some(response) = negotiation.response() {
                    transport.send(&response)?;
                }
                let user = startup.user().unwrap_or_default();
                if user.is_empty() {
                    return fatal(
                        transport,
                        ErrorOrNotice::error(
                            "28000",
                            "no PostgreSQL user name specified in startup packet",
                        ),
                    );
                }
                transport.send(&BackendMessage::Authentication(Authentication::Ok))?;
                let engine = match database.new_session_for_user(user) {
                    Ok(engine) => engine,
                    Err(error) => return fatal(transport, sql_error(&error)),
                };
                let database_name = startup
                    .database()
                    .filter(|name| !name.is_empty())
                    .unwrap_or(user);
                if database_name != "uqa" {
                    return fatal(
                        transport,
                        ErrorOrNotice::error(
                            "3D000",
                            format!("database \"{database_name}\" does not exist"),
                        ),
                    );
                }
                if let Err(error) = startup::configure(&engine, &startup) {
                    return fatal(transport, sql_error(&error));
                }
                let process_id = engine.backend_process_id();
                let mut secret = vec![
                    0;
                    if transport.version >= ProtocolVersion::V3_2 {
                        32
                    } else {
                        4
                    }
                ];
                getrandom::fill(&mut secret)
                    .map_err(|error| io::Error::other(error.to_string()))?;
                let secret = CancelKey::new(secret)?;
                shared.clients.lock().insert(
                    process_id,
                    Client {
                        secret: secret.clone(),
                        cancellation: engine.cancellation_token(),
                        executing: false,
                    },
                );
                let mut session = Session {
                    engine,
                    shared,
                    process_id,
                    reported: BTreeMap::new(),
                };
                send_parameters(transport, &mut session)?;
                transport.send(&BackendMessage::BackendKeyData(BackendKeyData {
                    process_id,
                    secret_key: secret,
                }))?;
                ready(transport, &session.engine)?;
                return Ok(Some(session));
            }
        }
    }
}

fn fatal<T>(transport: &mut Transport, mut error: ErrorOrNotice) -> Result<Option<T>, ServerError> {
    error.severity = NoticeSeverity::Fatal;
    transport.send(&BackendMessage::ErrorResponse(error))?;
    Ok(None)
}

fn execute_query(
    transport: &mut Transport,
    session: &mut Session<'_>,
    query: &str,
) -> Result<(), ServerError> {
    {
        let mut clients = session.shared.clients.lock();
        if session.shared.stopping.load(Ordering::Acquire) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "server is stopping").into());
        }
        let client = clients
            .get_mut(&session.process_id)
            .expect("registered session");
        client.cancellation.reset();
        client.executing = true;
    }
    let mut delivery_error = None;
    let result = session.engine.sql_simple_query(query, &[], |result| {
        let delivery = send_notices(transport, &session.engine)
            .and_then(|()| send_result(transport, &session.engine, result));
        match delivery {
            Ok(()) => Ok(()),
            Err(error) => {
                delivery_error = Some(error);
                Err(SQLError::Internal(
                    "PostgreSQL result delivery failed".into(),
                ))
            }
        }
    });
    session
        .shared
        .clients
        .lock()
        .get_mut(&session.process_id)
        .expect("registered session")
        .executing = false;
    session.engine.reset_cancellation();
    let result = match delivery_error {
        Some(ServerError::Sql(error)) => Err(error),
        Some(error) => return Err(error),
        None => result,
    };
    send_notices(transport, &session.engine)?;
    if let Err(error) = result {
        transport.send(&BackendMessage::ErrorResponse(sql_error(&error)))?;
    }
    send_parameters(transport, session)?;
    send_notifications(transport, &session.engine)?;
    ready(transport, &session.engine)
}

fn send_parameters(
    transport: &mut Transport,
    session: &mut Session<'_>,
) -> Result<(), ServerError> {
    for name in REPORTED_PARAMETERS {
        let value = session.engine.show_variable(name)?;
        if session.reported.get(*name) != Some(&value) {
            transport.send(&BackendMessage::ParameterStatus {
                name: (*name).into(),
                value: value.clone(),
            })?;
            session.reported.insert((*name).into(), value);
        }
    }
    Ok(())
}

fn send_notifications(transport: &mut Transport, engine: &Engine) -> Result<(), ServerError> {
    engine.poll_sql_notifications()?;
    for notification in engine.take_sql_notifications() {
        transport.send(&BackendMessage::NotificationResponse(
            NotificationResponse {
                process_id: notification.process_id,
                channel: notification.channel,
                payload: notification.payload,
            },
        ))?;
    }
    Ok(())
}

fn ready(transport: &mut Transport, engine: &Engine) -> Result<(), ServerError> {
    let status = if engine.transaction_failed() {
        TransactionStatus::Failed
    } else if engine.transaction_depth() != 0 {
        TransactionStatus::InTransaction
    } else {
        TransactionStatus::Idle
    };
    transport.send(&BackendMessage::ReadyForQuery(status))
}

fn is_read_timeout(error: &ServerError) -> bool {
    matches!(error, ServerError::Io(error) if matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock))
}
