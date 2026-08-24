//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::backend::Authentication;
use crate::codec::Reader;
use crate::frontend::PasswordMessage;
use crate::protocol::PgWireError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthenticationResponseKind {
    Password,
    KerberosV5,
    Gss,
    Sspi,
    SaslInitial,
    Sasl,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthenticationResponse {
    Password(String),
    KerberosV5(Vec<u8>),
    Gss(Vec<u8>),
    Sspi(Vec<u8>),
    SaslInitial {
        mechanism: String,
        initial_response: Option<Vec<u8>>,
    },
    Sasl(Vec<u8>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthenticationFamily {
    Password,
    KerberosV5,
    Gss,
    Sspi,
    Sasl,
    SaslFinal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExchangeState {
    Ready,
    AwaitingFrontend(AuthenticationResponseKind),
    AwaitingBackend(AuthenticationFamily),
    Complete,
    Failed,
}

impl ExchangeState {
    const fn description(self) -> &'static str {
        match self {
            Self::Ready => "ready for an authentication request",
            Self::AwaitingFrontend(_) => "awaiting a frontend authentication response",
            Self::AwaitingBackend(_) => "awaiting the next backend authentication message",
            Self::Complete => "authentication is complete",
            Self::Failed => "authentication has failed",
        }
    }
}

/// Validates one `PostgreSQL` authentication exchange while leaving credential
/// verification and secret storage to the embedding server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticationExchange {
    state: ExchangeState,
}

impl Default for AuthenticationExchange {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthenticationExchange {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: ExchangeState::Ready,
        }
    }

    /// Record an authentication message before it is sent to the frontend.
    pub fn send(&mut self, authentication: &Authentication) -> Result<(), PgWireError> {
        let next = match (self.state, authentication) {
            (ExchangeState::Ready | ExchangeState::AwaitingBackend(_), Authentication::Ok) => {
                ExchangeState::Complete
            }
            (ExchangeState::Ready, Authentication::KerberosV5) => {
                ExchangeState::AwaitingFrontend(AuthenticationResponseKind::KerberosV5)
            }
            (
                ExchangeState::Ready,
                Authentication::CleartextPassword | Authentication::Md5Password(_),
            ) => ExchangeState::AwaitingFrontend(AuthenticationResponseKind::Password),
            (ExchangeState::Ready, Authentication::Gss) => {
                ExchangeState::AwaitingFrontend(AuthenticationResponseKind::Gss)
            }
            (
                ExchangeState::AwaitingBackend(AuthenticationFamily::Gss),
                Authentication::GssContinue(_),
            ) => ExchangeState::AwaitingFrontend(AuthenticationResponseKind::Gss),
            (
                ExchangeState::AwaitingBackend(AuthenticationFamily::Sspi),
                Authentication::GssContinue(_),
            ) => ExchangeState::AwaitingFrontend(AuthenticationResponseKind::Sspi),
            (ExchangeState::Ready, Authentication::Sspi) => {
                ExchangeState::AwaitingFrontend(AuthenticationResponseKind::Sspi)
            }
            (ExchangeState::Ready, Authentication::Sasl { .. }) => {
                ExchangeState::AwaitingFrontend(AuthenticationResponseKind::SaslInitial)
            }
            (
                ExchangeState::AwaitingBackend(AuthenticationFamily::Sasl),
                Authentication::SaslContinue(_),
            ) => ExchangeState::AwaitingFrontend(AuthenticationResponseKind::Sasl),
            (
                ExchangeState::AwaitingBackend(AuthenticationFamily::Sasl),
                Authentication::SaslFinal(_),
            ) => ExchangeState::AwaitingBackend(AuthenticationFamily::SaslFinal),
            (state, authentication) => {
                return Err(PgWireError::InvalidAuthenticationSequence {
                    state: state.description(),
                    message: authentication.description(),
                });
            }
        };
        self.state = next;
        Ok(())
    }

    /// Decode the context-dependent frontend message tagged `p` and advance
    /// the exchange to its next backend decision point.
    pub fn receive(
        &mut self,
        message: &PasswordMessage,
    ) -> Result<AuthenticationResponse, PgWireError> {
        let ExchangeState::AwaitingFrontend(kind) = self.state else {
            return Err(PgWireError::InvalidAuthenticationSequence {
                state: self.state.description(),
                message: "a frontend authentication response",
            });
        };
        let response = decode_response(kind, message)?;
        self.state = ExchangeState::AwaitingBackend(match kind {
            AuthenticationResponseKind::Password => AuthenticationFamily::Password,
            AuthenticationResponseKind::KerberosV5 => AuthenticationFamily::KerberosV5,
            AuthenticationResponseKind::Gss => AuthenticationFamily::Gss,
            AuthenticationResponseKind::Sspi => AuthenticationFamily::Sspi,
            AuthenticationResponseKind::SaslInitial | AuthenticationResponseKind::Sasl => {
                AuthenticationFamily::Sasl
            }
        });
        Ok(response)
    }

    pub fn fail(&mut self) -> Result<(), PgWireError> {
        if matches!(self.state, ExchangeState::Complete | ExchangeState::Failed) {
            return Err(PgWireError::InvalidAuthenticationSequence {
                state: self.state.description(),
                message: "authentication failure",
            });
        }
        self.state = ExchangeState::Failed;
        Ok(())
    }

    #[must_use]
    pub const fn is_complete(&self) -> bool {
        matches!(self.state, ExchangeState::Complete)
    }

    #[must_use]
    pub const fn is_failed(&self) -> bool {
        matches!(self.state, ExchangeState::Failed)
    }

    #[must_use]
    pub const fn awaiting_response(&self) -> Option<AuthenticationResponseKind> {
        match self.state {
            ExchangeState::AwaitingFrontend(kind) => Some(kind),
            _ => None,
        }
    }
}

fn decode_response(
    kind: AuthenticationResponseKind,
    message: &PasswordMessage,
) -> Result<AuthenticationResponse, PgWireError> {
    match kind {
        AuthenticationResponseKind::Password => {
            let mut reader = Reader::new(message.as_bytes());
            let password = reader.read_cstring("PasswordMessage password")?;
            reader.ensure_empty("PasswordMessage")?;
            Ok(AuthenticationResponse::Password(password))
        }
        AuthenticationResponseKind::KerberosV5 => Ok(AuthenticationResponse::KerberosV5(
            message.as_bytes().to_vec(),
        )),
        AuthenticationResponseKind::Gss => {
            Ok(AuthenticationResponse::Gss(message.as_bytes().to_vec()))
        }
        AuthenticationResponseKind::Sspi => {
            Ok(AuthenticationResponse::Sspi(message.as_bytes().to_vec()))
        }
        AuthenticationResponseKind::SaslInitial => decode_sasl_initial(message),
        AuthenticationResponseKind::Sasl => {
            Ok(AuthenticationResponse::Sasl(message.as_bytes().to_vec()))
        }
    }
}

fn decode_sasl_initial(message: &PasswordMessage) -> Result<AuthenticationResponse, PgWireError> {
    let mut reader = Reader::new(message.as_bytes());
    let mechanism = reader.read_cstring("SASLInitialResponse mechanism")?;
    if mechanism.is_empty() {
        return Err(PgWireError::EmptySaslMechanism);
    }
    let length = reader.read_i32("SASLInitialResponse data length")?;
    let initial_response = match length {
        -1 => None,
        length if length < -1 => {
            return Err(PgWireError::NegativeValue {
                context: "SASLInitialResponse data length",
            });
        }
        length => Some(
            reader
                .read_exact(length as usize, "SASLInitialResponse data")?
                .to_vec(),
        ),
    };
    reader.ensure_empty("SASLInitialResponse")?;
    Ok(AuthenticationResponse::SaslInitial {
        mechanism,
        initial_response,
    })
}
