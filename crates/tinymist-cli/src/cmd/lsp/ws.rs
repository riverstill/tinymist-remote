//! WebSocket transport for the language server.
//!
//! The transport bridges the asynchronous WebSocket connection provided by
//! `hyper-tungstenite` to the blocking LSP connection expected by `sync-ls`.
//! Every WebSocket message carries a chunk of the LSP byte stream, so a client
//! only has to forward the payloads in order into a `StreamMessageReader`.

use std::io::{BufReader, Read, Write};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use futures::{SinkExt, StreamExt};
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::service::service_fn;
use hyper_tungstenite::HyperWebsocket;
use hyper_tungstenite::tungstenite::Message as WsMessage;
use hyper_util::rt::TokioIo;
use sync_ls::transport::with_stream_transport;
use sync_ls::{Connection, GetMessageKind, Message};
use tokio::sync::mpsc;

use crate::RUNTIMES;

/// Serves a language server over a single WebSocket connection.
///
/// The server accepts exactly one connection and returns once the peer closes
/// the connection or sends an exit notification.
pub fn with_websocket_transport<M: TryFrom<Message, Error = anyhow::Error> + GetMessageKind>(
    addr: SocketAddr,
    auth_token: Option<String>,
    f: impl FnOnce(Connection<M>) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    // Binds synchronously so that a failure to listen is reported to the
    // caller instead of being lost in the background task.
    let listener = std::net::TcpListener::bind(addr)?;
    listener.set_nonblocking(true)?;
    let local = listener.local_addr()?;
    log::info!("language server listening on ws://{local}");

    // The websocket side is asynchronous, while the language server is
    // blocking. Two pipes decouple both worlds.
    let (lsp_in_reader, lsp_in_writer) = std::io::pipe()?;
    let (lsp_out_reader, lsp_out_writer) = std::io::pipe()?;

    let (to_ws_tx, to_ws_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (to_lsp_tx, to_lsp_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    spawn_thread("tinymist-ws-out", move || {
        forward_pipe_to_channel(lsp_out_reader, to_ws_tx)
    })?;
    spawn_thread("tinymist-ws-in", move || {
        forward_channel_to_pipe(to_lsp_rx, lsp_in_writer)
    })?;

    RUNTIMES.tokio_runtime.spawn(async move {
        if let Err(err) = serve(listener, auth_token, to_lsp_tx, to_ws_rx).await {
            log::error!("websocket transport failed: {err:#}");
        }
    });

    with_stream_transport(
        M::MESSAGE_KIND,
        move || Box::new(BufReader::new(lsp_in_reader)),
        move || Box::new(lsp_out_writer),
        f,
    )
}

fn spawn_thread(
    name: &str,
    run: impl FnOnce() + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new().name(name.into()).spawn(run)
}

/// Copies the blocking LSP output stream into the asynchronous channel.
fn forward_pipe_to_channel(mut reader: std::io::PipeReader, tx: mpsc::UnboundedSender<Vec<u8>>) {
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => {
                log::warn!("websocket transport: failed to read lsp output: {err}");
                break;
            }
        }
    }
}

/// Copies the asynchronous websocket input into the blocking LSP input stream.
fn forward_channel_to_pipe(
    mut rx: mpsc::UnboundedReceiver<Vec<u8>>,
    mut writer: std::io::PipeWriter,
) {
    while let Some(bytes) = rx.blocking_recv() {
        if let Err(err) = writer.write_all(&bytes).and_then(|()| writer.flush()) {
            log::warn!("websocket transport: failed to write lsp input: {err}");
            break;
        }
    }
}

async fn serve(
    listener: std::net::TcpListener,
    auth_token: Option<String>,
    to_lsp: mpsc::UnboundedSender<Vec<u8>>,
    from_lsp: mpsc::UnboundedReceiver<Vec<u8>>,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::from_std(listener)?;

    // The server state is bound to a single client. Connections that fail the
    // authentication do not consume the session, so that a mistyped token can
    // simply be retried. Once a session starts, the language server exits when
    // the session ends.
    let session = Arc::new(Mutex::new(Some(from_lsp)));
    loop {
        let (stream, peer) = listener.accept().await?;
        log::info!("language server accepted a websocket connection from {peer}");

        let auth_token = auth_token.clone();
        let to_lsp = to_lsp.clone();
        let conn_session = session.clone();
        let service = service_fn(move |req| {
            let auth_token = auth_token.clone();
            let to_lsp = to_lsp.clone();
            let session = conn_session.clone();
            async move { handle_request(req, auth_token, to_lsp, session).await }
        });

        hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(stream), service)
            .with_upgrades()
            .await?;

        let session_taken = session
            .lock()
            .map(|guard| guard.is_none())
            .unwrap_or_default();
        if session_taken {
            log::info!("websocket connection closed");
            break;
        }
    }

    Ok(())
}

async fn handle_request(
    mut req: hyper::Request<Incoming>,
    auth_token: Option<String>,
    to_lsp: mpsc::UnboundedSender<Vec<u8>>,
    from_lsp: Arc<Mutex<Option<mpsc::UnboundedReceiver<Vec<u8>>>>>,
) -> anyhow::Result<hyper::Response<Full<Bytes>>> {
    if let Some(expected) = auth_token.as_deref() {
        let authorized = req
            .headers()
            .get(hyper::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .is_some_and(|token| {
                sync_ls::transport::constant_time_eq(token.as_bytes(), expected.as_bytes())
            });

        if !authorized {
            log::warn!("websocket transport: rejecting unauthorized connection");
            return Ok(hyper::Response::builder()
                .status(hyper::StatusCode::UNAUTHORIZED)
                .body(Full::default())?);
        }
    }

    if !hyper_tungstenite::is_upgrade_request(&req) {
        return Ok(hyper::Response::builder()
            .status(hyper::StatusCode::BAD_REQUEST)
            .body(Full::default())?);
    }

    let (response, websocket) = hyper_tungstenite::upgrade(&mut req, None)
        .map_err(|err| anyhow::anyhow!("cannot upgrade as websocket connection: {err}"))?;

    let from_lsp = {
        let mut guard = from_lsp
            .lock()
            .map_err(|_| anyhow::anyhow!("websocket state is poisoned"))?;
        guard.take()
    };
    let Some(from_lsp) = from_lsp else {
        log::warn!("websocket transport: rejecting a second session");
        return Ok(hyper::Response::builder()
            .status(hyper::StatusCode::CONFLICT)
            .body(Full::default())?);
    };

    tokio::spawn(async move {
        if let Err(err) = serve_websocket(websocket, to_lsp, from_lsp).await {
            log::error!("websocket transport: session failed: {err:#}");
        }
    });

    Ok(response)
}

async fn serve_websocket(
    websocket: HyperWebsocket,
    to_lsp: mpsc::UnboundedSender<Vec<u8>>,
    from_lsp: mpsc::UnboundedReceiver<Vec<u8>>,
) -> anyhow::Result<()> {
    let websocket = websocket.await?;
    let (mut sink, mut stream) = websocket.split();

    let writer = tokio::spawn(async move {
        let mut from_lsp = from_lsp;
        // The language server writes to a byte stream, while a client expects
        // one websocket message per LSP frame. Buffers the stream until a
        // complete frame is available.
        let mut pending = Vec::new();
        while let Some(bytes) = from_lsp.recv().await {
            pending.extend_from_slice(&bytes);
            while let Some(frame) = take_frame(&mut pending) {
                if let Err(err) = sink.send(WsMessage::binary(frame)).await {
                    log::info!("websocket transport: sink closed: {err}");
                    return;
                }
            }
        }
        let _ = sink.close().await;
    });

    while let Some(msg) = stream.next().await {
        match msg {
            Ok(WsMessage::Text(text)) => {
                if to_lsp.send(text.as_str().as_bytes().to_vec()).is_err() {
                    break;
                }
            }
            Ok(WsMessage::Binary(bytes)) => {
                if to_lsp.send(bytes.to_vec()).is_err() {
                    break;
                }
            }
            Ok(WsMessage::Close(_)) => break,
            Ok(WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Frame(_)) => {}
            Err(err) => {
                log::info!("websocket transport: stream closed: {err}");
                break;
            }
        }
    }

    // Closing the sender terminates the writer loop and finally the language
    // server, because the input pipe reaches its end of file.
    drop(to_lsp);
    let _ = writer.await;

    Ok(())
}

/// Takes the next complete LSP frame off the buffer.
///
/// Returns `None` if the buffer does not contain a complete frame yet.
fn take_frame(buf: &mut Vec<u8>) -> Option<Vec<u8>> {
    let header_end = buf.windows(4).position(|window| window == b"\r\n\r\n")? + 4;

    let header = std::str::from_utf8(&buf[..header_end]).ok()?;
    let mut content_length = None;
    for line in header.split("\r\n") {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.trim().parse::<usize>().ok();
        }
    }

    let Some(content_length) = content_length else {
        // Drops the unparsable header, otherwise the buffer never makes
        // progress.
        log::error!("websocket transport: dropping a message without content length");
        buf.drain(..header_end);
        return None;
    };

    let total = header_end + content_length;
    if buf.len() < total {
        return None;
    }

    Some(buf.drain(..total).collect())
}

#[cfg(test)]
mod tests {
    use super::take_frame;

    fn frame(body: &str) -> Vec<u8> {
        format!("Content-Length: {}\r\n\r\n{}", body.len(), body).into_bytes()
    }

    #[test]
    fn takes_a_complete_frame() {
        let mut buf = frame("{\"id\":1}");
        assert_eq!(take_frame(&mut buf).unwrap(), frame("{\"id\":1}"));
        assert!(buf.is_empty());
    }

    #[test]
    fn waits_for_the_rest_of_a_frame() {
        let full = frame("{\"id\":1}");
        let mut buf = full[..10].to_vec();
        assert!(take_frame(&mut buf).is_none());
        assert_eq!(buf.len(), 10);

        buf.extend_from_slice(&full[10..]);
        assert_eq!(take_frame(&mut buf).unwrap(), full);

        // The content is incomplete.
        let mut buf = full[..full.len() - 1].to_vec();
        assert!(take_frame(&mut buf).is_none());
    }

    #[test]
    fn takes_multiple_frames() {
        let mut buf = frame("a");
        buf.extend_from_slice(&frame("bb"));
        assert_eq!(take_frame(&mut buf).unwrap(), frame("a"));
        assert_eq!(take_frame(&mut buf).unwrap(), frame("bb"));
        assert!(take_frame(&mut buf).is_none());
    }

    #[test]
    fn drops_a_header_without_content_length() {
        let mut buf = b"Foo: bar\r\n\r\n".to_vec();
        assert!(take_frame(&mut buf).is_none());
        assert!(buf.is_empty());
    }
}
