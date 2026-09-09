//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use uqa_engine::Engine;
use uqa_pg_server::{Server, ServerConfig};

pub struct Fixture {
    pub server: Server,
    pub engine: Arc<Engine>,
    _directory: tempfile::TempDir,
}

impl Fixture {
    pub fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let engine = Arc::new(Engine::open(&directory.path().join("database.db")).unwrap());
        let server = Server::start(
            ServerConfig {
                listen: ([127, 0, 0, 1], 0).into(),
                trust_authentication: true,
                ..ServerConfig::default()
            },
            Arc::clone(&engine),
        )
        .unwrap();
        Self {
            server,
            engine,
            _directory: directory,
        }
    }

    pub fn connect(&self) -> Client {
        let (client, startup) = Client::connect(self.server.local_addr(), "uqa", "uqa", 196_610);
        assert_eq!(
            startup.last().map(|message| message.0),
            Some(b'Z'),
            "{startup:?}"
        );
        client
    }
}

pub type Message = (u8, Vec<u8>);

pub struct Client {
    pub socket: TcpStream,
    pub key: Vec<u8>,
}

impl Client {
    pub fn connect(
        address: SocketAddr,
        user: &str,
        database: &str,
        version: i32,
    ) -> (Self, Vec<Message>) {
        let socket = TcpStream::connect(address).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        let mut client = Self {
            socket,
            key: Vec::new(),
        };
        let mut startup = version.to_be_bytes().to_vec();
        for (key, value) in [
            ("user", user),
            ("database", database),
            ("application_name", "wire-test"),
        ] {
            startup.extend_from_slice(key.as_bytes());
            startup.push(0);
            startup.extend_from_slice(value.as_bytes());
            startup.push(0);
        }
        startup.push(0);
        client
            .socket
            .write_all(&((startup.len() + 4) as i32).to_be_bytes())
            .unwrap();
        client.socket.write_all(&startup).unwrap();
        let mut messages = Vec::new();
        loop {
            let message = client.receive();
            if message.0 == b'K' {
                client.key.clone_from(&message.1);
            }
            let finished = matches!(message.0, b'Z' | b'E');
            messages.push(message);
            if finished {
                break;
            }
        }
        (client, messages)
    }

    pub fn send(&mut self, tag: u8, data: &[u8]) {
        self.socket.write_all(&[tag]).unwrap();
        self.socket
            .write_all(&((data.len() + 4) as i32).to_be_bytes())
            .unwrap();
        self.socket.write_all(data).unwrap();
    }

    pub fn receive(&mut self) -> Message {
        let mut header = [0; 5];
        self.socket.read_exact(&mut header).unwrap();
        let length = i32::from_be_bytes(header[1..].try_into().unwrap());
        assert!((4..16 * 1024 * 1024).contains(&length));
        let mut body = vec![0; length as usize - 4];
        self.socket.read_exact(&mut body).unwrap();
        (header[0], body)
    }

    pub fn query(&mut self, sql: &str) -> Vec<Message> {
        let mut query = sql.as_bytes().to_vec();
        query.push(0);
        self.send(b'Q', &query);
        self.finish_query()
    }

    pub fn finish_query(&mut self) -> Vec<Message> {
        let mut messages = Vec::new();
        loop {
            let message = self.receive();
            let done = message.0 == b'Z';
            messages.push(message);
            if done {
                return messages;
            }
        }
    }
}

pub fn fields(bytes: &[u8]) -> BTreeMap<u8, String> {
    let mut remaining = bytes;
    let mut fields = BTreeMap::new();
    while remaining[0] != 0 {
        let tag = remaining[0];
        remaining = &remaining[1..];
        fields.insert(tag, read_string(&mut remaining));
    }
    fields
}

pub fn read_string(bytes: &mut &[u8]) -> String {
    let length = bytes.iter().position(|byte| *byte == 0).unwrap();
    let text = String::from_utf8(bytes[..length].to_vec()).unwrap();
    *bytes = &bytes[length + 1..];
    text
}

pub fn read_i16(bytes: &mut &[u8]) -> i16 {
    let value = i16::from_be_bytes(bytes[..2].try_into().unwrap());
    *bytes = &bytes[2..];
    value
}

pub fn read_i32(bytes: &mut &[u8]) -> i32 {
    let value = i32::from_be_bytes(bytes[..4].try_into().unwrap());
    *bytes = &bytes[4..];
    value
}

pub fn evidence(messages: &[Message]) -> Value {
    result_evidence(messages, false)
}

pub fn evidence_with_fields(messages: &[Message]) -> Value {
    result_evidence(messages, true)
}

fn result_evidence(messages: &[Message], include_fields: bool) -> Value {
    let mut tags = Vec::new();
    let mut error = Value::Null;
    let mut results = Vec::new();
    let mut columns = Vec::new();
    let mut types = Vec::new();
    let mut descriptors = Vec::new();
    let mut rows: Vec<Vec<Option<String>>> = Vec::new();
    for (tag, bytes) in messages {
        let mut bytes = bytes.as_slice();
        match tag {
            b'T' => {
                for _ in 0..read_i16(&mut bytes) {
                    let name = read_string(&mut bytes);
                    let table_oid = read_i32(&mut bytes);
                    let column_attribute_number = read_i16(&mut bytes);
                    let type_oid = read_i32(&mut bytes);
                    let type_size = read_i16(&mut bytes);
                    let type_modifier = read_i32(&mut bytes);
                    let format = read_i16(&mut bytes);
                    assert_eq!(format, 0);
                    descriptors.push(json!({ "name": name, "table_oid": table_oid, "column_attribute_number": column_attribute_number, "type_oid": type_oid, "type_size": type_size, "type_modifier": type_modifier, "format": format }));
                    columns.push(name);
                    types.push(type_oid);
                }
            }
            b'D' => {
                let row = (0..read_i16(&mut bytes))
                    .map(|_| {
                        let length = read_i32(&mut bytes);
                        if length == -1 {
                            return None;
                        }
                        let value = String::from_utf8(bytes[..length as usize].to_vec()).unwrap();
                        bytes = &bytes[length as usize..];
                        Some(value)
                    })
                    .collect();
                rows.push(row);
            }
            b'C' | b'I' => {
                tags.push(if *tag == b'C' {
                    json!(read_string(&mut bytes))
                } else {
                    Value::Null
                });
                let mut result = json!({ "columns": columns, "type_oids": types, "rows": rows });
                if include_fields {
                    result["fields"] = json!(descriptors);
                }
                results.push(result);
                descriptors.clear();
                columns.clear();
                types.clear();
                rows.clear();
            }
            b'E' => {
                let diagnostic = fields(bytes);
                error = json!({ "sqlstate": diagnostic[&b'C'], "message": diagnostic[&b'M'] });
            }
            _ => {}
        }
    }
    json!({ "command_tags": tags, "error": error, "results": results })
}
