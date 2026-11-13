//! `triple-c-agent` entry point: read the environment, drop the token from
//! it, bind, serve. Everything else is in the library.

use std::path::PathBuf;
use std::process::ExitCode;

use triple_c_agent::{load_token, serve, Config, ADDR_ENV, DEFAULT_ADDR, TOKEN_ENV, TOKEN_FILE_ENV};

fn main() -> ExitCode {
    let token = match load_token(
        std::env::var_os(TOKEN_ENV),
        std::env::var_os(TOKEN_FILE_ENV).map(PathBuf::from),
    ) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("triple-c-agent: {e}");
            return ExitCode::from(2);
        }
    };
    // Before the runtime starts any thread and before anything is spawned:
    // removing an env var is only sound while the process is single-threaded,
    // and every exec's base env is captured from this process's env below.
    std::env::remove_var(TOKEN_ENV);

    // Loopback by default. A non-loopback address is only ever bound when
    // someone wrote it into TRIPLEC_AGENT_ADDR on purpose.
    let addr = std::env::var(ADDR_ENV).unwrap_or_else(|_| DEFAULT_ADDR.to_string());
    let config = match Config::from_process(token) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("triple-c-agent: cannot read startup state: {e}");
            return ExitCode::from(2);
        }
    };

    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("triple-c-agent: cannot start runtime: {e}");
            return ExitCode::from(2);
        }
    };
    rt.block_on(async move {
        let listener = match tokio::net::TcpListener::bind(&addr).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("triple-c-agent: cannot bind {addr}: {e}");
                return ExitCode::from(2);
            }
        };
        let shown = listener.local_addr().map(|a| a.to_string()).unwrap_or(addr);
        // One line, never the token.
        eprintln!("triple-c-agent {} listening on {shown}", env!("CARGO_PKG_VERSION"));
        match serve(listener, config).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("triple-c-agent: server failed: {e}");
                ExitCode::FAILURE
            }
        }
    })
}
