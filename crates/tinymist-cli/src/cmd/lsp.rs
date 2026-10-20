use std::net::SocketAddr;

use sync_ls::transport::{MirrorArgs, with_stdio_transport, with_tcp_transport};
use sync_ls::{Connection, LspBuilder, LspMessage};
use tinymist::LONG_VERSION;
use tinymist::world::CompileFontArgs;
use tinymist::{RegularInit, ServerState};
use tinymist_std::error::prelude::*;

use crate::*;

#[cfg(feature = "remote-transport")]
mod ws;

/// Arguments for serving the language server over a socket instead of stdio.
#[derive(Debug, Clone, Default, clap::Parser)]
pub struct SocketArgs {
    /// Serves a single LSP client over a TCP connection, e.g. `127.0.0.1:23627`.
    ///
    /// The server exits when the client disconnects. The client has to send a
    /// `TINYMIST-AUTH <token>` line before the LSP stream if `--auth-token` is
    /// configured.
    #[clap(long, value_name = "ADDR")]
    #[cfg_attr(feature = "remote-transport", clap(conflicts_with = "ws"))]
    pub tcp: Option<SocketAddr>,

    /// Serves a single LSP client over a WebSocket connection, e.g. `0.0.0.0:23626`.
    ///
    /// The server exits when the client disconnects. The client has to pass an
    /// `Authorization: Bearer <token>` header during the upgrade if
    /// `--auth-token` is configured.
    #[cfg(feature = "remote-transport")]
    #[clap(long, value_name = "ADDR", conflicts_with = "tcp")]
    pub ws: Option<SocketAddr>,

    /// The token required to authenticate remote clients.
    ///
    /// It is recommended to pass the token through the `TINYMIST_AUTH_TOKEN`
    /// environment variable instead of the command line.
    #[clap(long, env = "TINYMIST_AUTH_TOKEN", value_name = "TOKEN")]
    pub auth_token: Option<String>,
}

/// Arguments for starting LSP server.
#[derive(Debug, Clone, Default, clap::Parser)]
pub struct LspArgs {
    /// Arguments for mirroring the transport.
    #[clap(flatten)]
    pub mirror: MirrorArgs,
    /// Arguments for font.
    #[clap(flatten)]
    pub font: CompileFontArgs,
    /// Arguments for serving the language server over a socket.
    #[clap(flatten)]
    pub socket: SocketArgs,
}

/// The main entry point for the language server.
pub fn lsp_main(args: LspArgs) -> Result<()> {
    let pairs = LONG_VERSION.trim().split('\n');
    let pairs = pairs
        .map(|e| e.splitn(2, ":").map(|e| e.trim()).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    log::info!("tinymist version information: {pairs:?}");
    log::info!("starting language server: {args:?}");

    let is_replay = !args.mirror.replay.is_empty();
    let font = args.font.clone();

    let serve = move |conn: Connection<LspMessage>| -> anyhow::Result<()> {
        let client = client_root(conn.sender);
        ServerState::install_lsp(LspBuilder::new(
            RegularInit {
                client: client.weak().to_typed(),
                font_opts: font,
                exec_cmds: Vec::new(),
            },
            client.weak(),
        ))
        .build()
        .start(conn.receiver, is_replay)
    };

    if let Some(addr) = args.socket.tcp {
        log::info!("serving language server over tcp: {addr}");
        return Ok(with_tcp_transport::<LspMessage>(
            addr,
            args.socket.auth_token,
            serve,
        )?);
    }

    #[cfg(feature = "remote-transport")]
    if let Some(addr) = args.socket.ws {
        log::info!("serving language server over websocket: {addr}");
        return Ok(ws::with_websocket_transport::<LspMessage>(
            addr,
            args.socket.auth_token,
            serve,
        )?);
    }

    with_stdio_transport::<LspMessage>(args.mirror.clone(), serve)?;

    log::info!("language server did shut down");
    Ok(())
}
