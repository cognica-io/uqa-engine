//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::io::{self, Read, Write};
use std::net::TcpStream;

use uqa_pg_wire::{BackendMessage, DecodeOutcome, ProtocolVersion};

use crate::ServerError;

pub(crate) struct Transport {
    socket: TcpStream,
    input: Vec<u8>,
    pub version: ProtocolVersion,
}

impl Transport {
    pub fn new(socket: TcpStream) -> Self {
        Self {
            socket,
            input: Vec::new(),
            version: ProtocolVersion::V3_0,
        }
    }

    pub fn read<T>(
        &mut self,
        decode: impl Fn(&[u8]) -> DecodeOutcome<T>,
    ) -> Result<Option<T>, ServerError> {
        loop {
            if let Some((message, consumed)) = decode(&self.input)? {
                self.input.drain(..consumed);
                return Ok(Some(message));
            }
            let mut buffer = [0; 8192];
            match self.socket.read(&mut buffer) {
                Ok(0) if self.input.is_empty() => return Ok(None),
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "incomplete PostgreSQL message",
                    )
                    .into());
                }
                Ok(read) => self.input.extend_from_slice(&buffer[..read]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error.into()),
            }
        }
    }

    pub fn send(&mut self, message: &BackendMessage) -> Result<(), ServerError> {
        self.socket
            .write_all(&message.encode_for_protocol(self.version)?)?;
        Ok(())
    }

    pub fn reject_encryption(&mut self) -> Result<(), ServerError> {
        self.socket.write_all(b"N")?;
        Ok(())
    }
}
