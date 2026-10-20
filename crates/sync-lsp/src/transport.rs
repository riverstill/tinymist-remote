//! Transport layer for LSP messages.

use std::{
    io::{self, BufRead, Read, Write},
    net::{SocketAddr, TcpListener},
    thread,
};

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};

use crate::{Connection, ConnectionRx, ConnectionTx, GetMessageKind, Message};

/// The prefix of the authentication preamble in the TCP transport.
///
/// The client must send a single line, `TINYMIST-AUTH <token>`, before the LSP
/// stream when the server is configured with an auth token.
pub const AUTH_PREFIX: &str = "TINYMIST-AUTH ";

/// Convenience cli arguments for setting up a transport with an optional mirror
/// or replay file.
///
/// The `mirror` argument will write the stdin to the file.
/// The `replay` argument will read the file as input.
///
/// # Example
///
/// The example below shows the typical usage of the `MirrorArgs` struct.
/// It records an LSP or DAP session and replays it to compare the output.
///
/// If the language server has stable output, the replayed output should be the
/// same.
///
/// ```bash
/// $ my-lsp --mirror /tmp/mirror.log > responses.txt
/// $ ls /tmp
/// mirror.log
/// $ my-lsp --replay /tmp/mirror.log > responses-replayed.txt
/// $ diff responses.txt responses-replayed.txt
/// ```
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "clap", derive(clap::Parser))]
pub struct MirrorArgs {
    /// Mirror the stdin to the file
    #[cfg_attr(feature = "clap", clap(long, default_value = "", value_name = "FILE"))]
    pub mirror: String,
    /// Replay input from the file
    #[cfg_attr(feature = "clap", clap(long, default_value = "", value_name = "FILE"))]
    pub replay: String,
}

/// Note that we must have our logging only write out to stderr.
pub fn with_stdio_transport<M: TryFrom<Message, Error = anyhow::Error> + GetMessageKind>(
    args: MirrorArgs,
    f: impl FnOnce(Connection<M>) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    with_stdio_transport_impl(args, M::MESSAGE_KIND, |conn| f(conn.into()))
}

/// Note that we must have our logging only write out to stderr.
fn with_stdio_transport_impl(
    args: MirrorArgs,
    kind: crate::MessageKind,
    f: impl FnOnce(Connection<Message>) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    // Set up input and output
    let replay = args.replay.clone();
    let mirror = args.mirror.clone();
    let i = move || -> Box<dyn BufRead> {
        if !replay.is_empty() {
            // Get input from file
            let file = std::fs::File::open(&replay).unwrap();
            let file = std::io::BufReader::new(file);
            Box::new(file)
        } else if mirror.is_empty() {
            // Get input from stdin
            let stdin = std::io::stdin().lock();
            Box::new(stdin)
        } else {
            let file = std::fs::File::create(&mirror).unwrap();
            let stdin = std::io::stdin().lock();
            Box::new(MirrorWriter(stdin, file, std::sync::Once::new()))
        }
    };
    let o = || std::io::stdout().lock();

    let (event_sender, event_receiver) = unbounded::<crate::Event>();

    // Create the transport. Includes the stdio (stdin and stdout) versions but this
    // could also be implemented to use sockets or HTTP.
    let (lsp_sender, lsp_receiver, io_threads) = io_transport(kind, i, o);
    let connection = Connection {
        // lsp_sender,
        // lsp_receiver,
        sender: ConnectionTx {
            event: event_sender,
            lsp: lsp_sender,
            marker: std::marker::PhantomData,
        },
        receiver: ConnectionRx {
            event: event_receiver,
            lsp: lsp_receiver,
            marker: std::marker::PhantomData,
        },
    };

    f(connection)?;

    io_threads.join_write()?;

    Ok(())
}

/// Serves a language server over the given input and output streams.
///
/// The stream factories are called on the dedicated reader and writer threads
/// respectively, so that socket-backed and pipe-backed streams can be created
/// lazily.
pub fn with_stream_transport<M: TryFrom<Message, Error = anyhow::Error> + GetMessageKind>(
    kind: crate::MessageKind,
    inp: impl FnOnce() -> Box<dyn BufRead> + Send + Sync + 'static,
    out: impl FnOnce() -> Box<dyn Write> + Send + Sync + 'static,
    f: impl FnOnce(Connection<M>) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let (event_sender, event_receiver) = unbounded::<crate::Event>();
    let (lsp_sender, lsp_receiver, io_threads) = io_transport(kind, inp, out);

    let connection = Connection {
        sender: ConnectionTx {
            event: event_sender,
            lsp: lsp_sender,
            marker: std::marker::PhantomData,
        },
        receiver: ConnectionRx {
            event: event_receiver,
            lsp: lsp_receiver,
            marker: std::marker::PhantomData,
        },
    };

    f(connection.into())?;

    io_threads.join_write()?;

    Ok(())
}

/// Serves a language server over a single TCP connection.
///
/// The server accepts exactly one connection and returns once the peer closes
/// the connection or sends an exit notification. Deployment-level supervisors
/// (systemd, docker restart policies, ...) are expected to restart the process
/// for the next session, since the language server state is bound to a single
/// client.
pub fn with_tcp_transport<M: TryFrom<Message, Error = anyhow::Error> + GetMessageKind>(
    addr: SocketAddr,
    auth_token: Option<String>,
    f: impl FnOnce(Connection<M>) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind(addr)?;
    let local = listener.local_addr()?;
    log::info!("language server listening on tcp://{local}");

    let (stream, peer) = listener.accept()?;
    log::info!("language server accepted a tcp connection from {peer}");
    // The listener must not accept a second session, as the server state is
    // bound to a single client.
    drop(listener);

    stream.set_nodelay(true).ok();

    let read_half = stream.try_clone()?;
    let mut reader = io::BufReader::new(read_half);

    if let Some(expected) = auth_token {
        verify_tcp_auth(&mut reader, &expected)?;
    }

    with_stream_transport(
        M::MESSAGE_KIND,
        move || Box::new(reader),
        move || Box::new(stream),
        f,
    )
}

/// Verifies the authentication preamble of a TCP connection.
fn verify_tcp_auth(reader: &mut impl BufRead, expected: &str) -> anyhow::Result<()> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        anyhow::bail!("client closed the connection before authentication");
    }

    let line = line.trim_end_matches(['\r', '\n']);
    let Some(token) = line.strip_prefix(AUTH_PREFIX) else {
        anyhow::bail!("malformed authentication preamble");
    };

    if !constant_time_eq(token.as_bytes(), expected.as_bytes()) {
        anyhow::bail!("invalid authentication token");
    }

    Ok(())
}

/// Compares two byte strings without leaking the length of the common prefix
/// through timing.
pub fn constant_time_eq(lhs: &[u8], rhs: &[u8]) -> bool {
    if lhs.len() != rhs.len() {
        return false;
    }

    let mut diff = 0u8;
    for (l, r) in lhs.iter().zip(rhs) {
        diff |= *l ^ *r;
    }
    diff == 0
}

/// Creates an LSP connection via io.
///
/// # Example
///
/// ```
/// use std::io::{stdin, stdout};
/// use sync_ls::{Message, MessageKind, transport::{io_transport, IoThreads}};
/// use crossbeam_channel::{bounded, Receiver, Sender};
/// pub fn stdio_transport() -> (Sender<Message>, Receiver<Message>, IoThreads) {
///   io_transport(MessageKind::Lsp, || stdin().lock(), || stdout().lock())
/// }
/// ```
pub fn io_transport<I: BufRead, O: Write>(
    kind: crate::MessageKind,
    inp: impl FnOnce() -> I + Send + Sync + 'static,
    out: impl FnOnce() -> O + Send + Sync + 'static,
) -> (Sender<Message>, Receiver<Message>, IoThreads) {
    let (writer_sender, writer_receiver) = bounded::<Message>(0);
    let writer = thread::spawn(move || {
        let mut out = out();
        let res = writer_receiver
            .into_iter()
            .try_for_each(|it| it.write(&mut out));

        log::info!("writer thread finished");
        res
    });
    let (reader_sender, reader_receiver) = bounded::<Message>(0);
    let reader = thread::spawn(move || {
        let mut inp = inp();
        let read_impl = match kind {
            #[cfg(feature = "lsp")]
            crate::MessageKind::Lsp => Message::read_lsp::<I>,
            #[cfg(feature = "dap")]
            crate::MessageKind::Dap => Message::read_dap::<I>,
        };
        while let Some(msg) = read_impl(&mut inp)? {
            #[cfg(feature = "lsp")]
            use crate::LspMessage;
            #[cfg(feature = "lsp")]
            let is_exit = matches!(&msg, Message::Lsp(LspMessage::Notification(n)) if n.is_exit());

            log::trace!("sending message {msg:#?}");
            reader_sender
                .send(msg)
                .expect("receiver was dropped, failed to send a message");

            #[cfg(feature = "lsp")]
            if is_exit {
                break;
            }
        }

        log::info!("reader thread finished");
        Ok(())
    });
    let threads = IoThreads { reader, writer };
    (writer_sender, reader_receiver, threads)
}

/// A pair of threads for reading and writing LSP messages.
pub struct IoThreads {
    reader: thread::JoinHandle<io::Result<()>>,
    writer: thread::JoinHandle<io::Result<()>>,
}

impl IoThreads {
    /// Waits for the reader and writer threads to finish.
    pub fn join(self) -> io::Result<()> {
        match self.reader.join() {
            Ok(r) => r?,
            Err(err) => {
                eprintln!("reader panicked!");
                std::panic::panic_any(err)
            }
        }
        match self.writer.join() {
            Ok(r) => r,
            Err(err) => {
                eprintln!("writer panicked!");
                std::panic::panic_any(err);
            }
        }
    }

    /// Waits for the writer threads to finish.
    pub fn join_write(self) -> io::Result<()> {
        match self.writer.join() {
            Ok(r) => r,
            Err(err) => {
                eprintln!("writer panicked!");
                std::panic::panic_any(err);
            }
        }
    }
}

struct MirrorWriter<R: Read, W: Write>(R, W, std::sync::Once);

impl<R: Read, W: Write> Read for MirrorWriter<R, W> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let res = self.0.read(buf)?;

        if let Err(err) = self.1.write_all(&buf[..res]) {
            self.2.call_once(|| {
                log::warn!("failed to write to mirror: {err}");
            });
        }

        Ok(res)
    }
}

impl<R: Read + BufRead, W: Write> BufRead for MirrorWriter<R, W> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.0.fill_buf()
    }

    fn consume(&mut self, amt: usize) {
        let buf = self.0.fill_buf().unwrap();

        if let Err(err) = self.1.write_all(&buf[..amt]) {
            self.2.call_once(|| {
                log::warn!("failed to write to mirror: {err}");
            });
        }

        self.0.consume(amt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constant_time_eq() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        assert!(!constant_time_eq(b"secret", b"secret "));
        assert!(!constant_time_eq(b"", b"secret"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn test_verify_tcp_auth() {
        let mut input =
            io::BufReader::new(&b"TINYMIST-AUTH token\r\nContent-Length: 0\r\n\r\n"[..]);
        verify_tcp_auth(&mut input, "token").unwrap();
        let mut rest = String::new();
        input.read_line(&mut rest).unwrap();
        assert_eq!(rest, "Content-Length: 0\r\n");

        let mut input = io::BufReader::new(&b"TINYMIST-AUTH wrong\r\n"[..]);
        assert!(verify_tcp_auth(&mut input, "token").is_err());

        let mut input = io::BufReader::new(&b"not-a-preamble\r\n"[..]);
        assert!(verify_tcp_auth(&mut input, "token").is_err());

        let mut input = io::BufReader::new(&b""[..]);
        assert!(verify_tcp_auth(&mut input, "token").is_err());
    }

    #[test]
    fn test_tcp_transport_roundtrip() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let server = thread::spawn(move || {
            with_tcp_transport::<crate::LspMessage>(addr, None, |conn| {
                let msg = conn.receiver.recv()?;
                let crate::EventOrMessage::Msg(crate::LspMessage::Notification(notif)) = msg else {
                    anyhow::bail!("unexpected message");
                };
                assert_eq!(notif.method, "exit");
                Ok(())
            })
        });

        // Waits for the server to bind the address.
        let mut stream = None;
        for _ in 0..100 {
            match std::net::TcpStream::connect(addr) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(_) => thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
        let mut stream = stream.expect("cannot connect to the tcp transport");

        let body = r#"{"jsonrpc":"2.0","method":"exit"}"#;
        write!(stream, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
        stream.flush().unwrap();

        server.join().unwrap().unwrap();
    }
}
