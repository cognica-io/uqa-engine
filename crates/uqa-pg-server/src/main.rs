//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use uqa_engine::Engine;
use uqa_pg_server::{Server, ServerConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = ServerConfig::default();
    let mut path = None;
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--database" => path = args.next(),
            "--listen" => {
                config.listen = args.next().ok_or("--listen requires an address")?.parse()?;
            }
            "--trust" => config.trust_authentication = true,
            "--help" | "-h" => {
                println!("uqa-pg-server --database PATH --trust [--listen 127.0.0.1:5433]");
                return Ok(());
            }
            _ => return Err(format!("unknown argument: {argument}").into()),
        }
    }
    let path =
        path.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "--database is required"))?;
    let engine = Arc::new(Engine::open(Path::new(&path))?);
    let server = Server::start(config, engine)?;
    println!(
        "UQA PostgreSQL server listening on {} (database uqa)",
        server.local_addr()
    );
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}
