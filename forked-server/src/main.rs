//! Binary entrypoint. Configuration via flags or environment:
//!
//! ```text
//! forked-server [--db <path>|memory] [--addr <ip:port>] [--token <secret>]
//!   FORKED_DB     storage: a SQLite file path, or "memory" (default: forked.db)
//!   FORKED_ADDR   listen address               (default: 127.0.0.1:8080)
//!   FORKED_TOKEN  require `Authorization: Bearer <token>` when set
//! ```

use std::process::ExitCode;
use std::sync::Arc;

use forked_core::Forked;
use forked_memory::MemoryStorage;
use forked_server::{AppState, Backend};
use forked_sqlite::SqliteStorage;

struct Config {
    db: String,
    addr: String,
    token: Option<String>,
}

fn parse_config() -> Result<Config, String> {
    let mut config = Config {
        db: std::env::var("FORKED_DB").unwrap_or_else(|_| "forked.db".to_string()),
        addr: std::env::var("FORKED_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string()),
        token: std::env::var("FORKED_TOKEN").ok(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .ok_or_else(|| format!("{name} requires a value"))
        };
        match flag.as_str() {
            "--db" => config.db = value("--db")?,
            "--addr" => config.addr = value("--addr")?,
            "--token" => config.token = Some(value("--token")?),
            "--help" | "-h" => {
                println!(
                    "forked-server [--db <path>|memory] [--addr <ip:port>] [--token <secret>]"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    Ok(config)
}

#[tokio::main]
async fn main() -> ExitCode {
    let config = match parse_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("forked-server: {e}");
            return ExitCode::FAILURE;
        }
    };

    let backend = if config.db == "memory" {
        println!("forked-server: storage = in-memory (volatile)");
        Backend::Memory(MemoryStorage::new())
    } else {
        match SqliteStorage::open(&config.db) {
            Ok(s) => {
                println!("forked-server: storage = sqlite ({})", config.db);
                Backend::Sqlite(s)
            }
            Err(e) => {
                eprintln!("forked-server: failed to open {}: {e}", config.db);
                return ExitCode::FAILURE;
            }
        }
    };

    let state = Arc::new(AppState {
        forked: Forked::new(backend),
        token: config.token.clone(),
    });
    println!(
        "forked-server: auth = {}",
        if config.token.is_some() {
            "bearer token"
        } else {
            "none (open)"
        }
    );

    let listener = match tokio::net::TcpListener::bind(&config.addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("forked-server: failed to bind {}: {e}", config.addr);
            return ExitCode::FAILURE;
        }
    };
    println!("forked-server: listening on http://{}", config.addr);

    let result = axum::serve(listener, forked_server::router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            println!("forked-server: shutting down");
        })
        .await;
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("forked-server: {e}");
            ExitCode::FAILURE
        }
    }
}
