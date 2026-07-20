//! Windows named-pipe transport glue for tonic (tech-stack.md §5).
//!
//! tonic serves over anything that looks like a stream of AsyncRead/AsyncWrite
//! connections; there are no TCP ports (important for a tool that audits
//! listening ports, §5). This module provides:
//!
//! * [`pipe_name`] / [`default_pipe_name`] — collision-safe `\\.\pipe\...`
//!   name construction.
//! * [`serve`] — an accept loop that creates a fresh `NamedPipeServer`
//!   instance per client (the standard Win32 named-pipe idiom: one waiting
//!   instance is always kept listening while connected instances are handed
//!   off) and drives them into `Server::serve_with_incoming`, with the pipe
//!   DACL from [`crate::security`].
//! * [`connect`] — a tonic `Channel` built over a `tower::service_fn`
//!   connector that dials a `NamedPipeClient`.

#![cfg(windows)]

use std::io;
use std::os::windows::io::AsRawHandle;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};

/// Windows error `ERROR_PIPE_BUSY` (231): all pipe instances are busy; the
/// client should wait and retry.
const ERROR_PIPE_BUSY: i32 = 231;
/// `ERROR_FILE_NOT_FOUND` (2): the server has not created the pipe yet. During
/// startup the client may dial before the first instance exists; retry until
/// it appears (bounded by the dial deadline).
const ERROR_FILE_NOT_FOUND: i32 = 2;

#[link(name = "kernel32")]
extern "system" {
    fn GetNamedPipeClientProcessId(pipe: *mut std::ffi::c_void, client_pid: *mut u32) -> i32;
}

/// Builds the full pipe path for a given instance discriminator. The name is
/// scoped by `\\.\pipe\SystemAtlas.dev.<who>` so parallel dev instances (and
/// the round-trip tests, which pass a unique token) never collide.
pub fn pipe_name(who: &str) -> String {
    format!(r"\\.\pipe\SystemAtlas.dev.{who}")
}

/// The default dev pipe name, scoped to the current user (`USERNAME`) so two
/// users on the same machine get distinct pipes. Falls back to `session` if
/// the variable is missing.
pub fn default_pipe_name() -> String {
    let who = std::env::var("USERNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "session".to_string());
    pipe_name(&who)
}

/// Connection wrapper so tonic's `Connected` bound is satisfied. The inner
/// `NamedPipeServer` already implements AsyncRead/AsyncWrite; we delegate to
/// it and provide trivial connection info.
pub struct PipeConnection {
    inner: NamedPipeServer,
    info: PipeConnectInfo,
}

/// Minimal connection info exposed through request extensions. Named pipes
/// carry no peer address; per-connection client authentication (client PID /
/// signature, tech-stack §4.5) is future work — the pipe DACL is the boundary
/// today.
#[derive(Clone, Debug, Default)]
pub struct PipeConnectInfo {
    pub client_pid: u32,
    pub client_sid: String,
}

impl tonic::transport::server::Connected for PipeConnection {
    type ConnectInfo = PipeConnectInfo;

    fn connect_info(&self) -> Self::ConnectInfo {
        self.info.clone()
    }
}

impl AsyncRead for PipeConnection {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for PipeConnection {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Creates a named-pipe server instance with the Atlas DACL applied. `first`
/// marks the first instance so a second listener on the same name can't
/// silently hijack it.
fn make_server(
    name: &str,
    security: &crate::security::SecurityDescriptor,
    first: bool,
) -> io::Result<NamedPipeServer> {
    // SAFETY: `security.as_ptr()` is a valid `*mut SECURITY_ATTRIBUTES` that
    // outlives this call (the caller keeps `security` alive for the accept
    // loop's lifetime).
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .create_with_security_attributes_raw(name, security.as_ptr())
    }
}

/// Serves a pre-built tonic `Router` over the named pipe `name` until
/// `shutdown` resolves. Callers build the router the usual way, e.g.
/// `Server::builder().add_service(AtlasQueryServer::new(svc))`, keeping the
/// concrete service bounds on tonic's side and this transport agnostic to it.
///
/// Runs the classic accept loop: keep one server instance waiting for a
/// connection; when a client connects, hand that instance to tonic and
/// immediately create the next waiting instance so a listener is always
/// present (no connect race).
///
/// The DACL is applied to every instance (built once and reused). `shutdown`
/// is any future — e.g. `tokio::signal::ctrl_c()` or a oneshot.
pub async fn serve<F>(
    name: &str,
    router: tonic::transport::server::Router,
    shutdown: F,
) -> anyhow::Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let security = crate::security::SecurityDescriptor::for_current_user()?;
    let (conn_tx, conn_rx) = tokio::sync::mpsc::channel::<Result<PipeConnection, io::Error>>(16);

    // The accept task owns the loop of "wait for a connection, hand it off,
    // open the next instance". It ends when `shutdown` fires.
    let accept_name = name.to_string();
    let accept = tokio::spawn(async move {
        let mut first = true;
        let mut server = match make_server(&accept_name, &security, first) {
            Ok(s) => s,
            Err(e) => {
                let _ = conn_tx.send(Err(e)).await;
                return;
            }
        };
        first = false;
        loop {
            // Wait for a client to connect to this instance.
            if let Err(e) = server.connect().await {
                let _ = conn_tx.send(Err(e)).await;
                return;
            }
            // Open the next waiting instance before handing this one off, so
            // there is always a listener available (no connect race).
            let next = match make_server(&accept_name, &security, first) {
                Ok(s) => s,
                Err(e) => {
                    let _ = conn_tx.send(Err(e)).await;
                    return;
                }
            };
            let info = connected_client_info(&server);
            let connected = std::mem::replace(&mut server, next);
            if conn_tx
                .send(Ok(PipeConnection {
                    inner: connected,
                    info,
                }))
                .await
                .is_err()
            {
                // Receiver (tonic) gone — stop accepting.
                return;
            }
        }
    });

    let incoming = tokio_stream::wrappers::ReceiverStream::new(conn_rx);

    router
        .serve_with_incoming_shutdown(incoming, shutdown)
        .await?;

    accept.abort();
    Ok(())
}

fn connected_client_info(pipe: &NamedPipeServer) -> PipeConnectInfo {
    let mut client_pid = 0u32;
    let ok = unsafe { GetNamedPipeClientProcessId(pipe.as_raw_handle().cast(), &mut client_pid) };
    if ok == 0 || client_pid == 0 {
        return PipeConnectInfo::default();
    }
    let client_sid = crate::security::process_user_sid_string(client_pid).unwrap_or_default();
    PipeConnectInfo {
        client_pid,
        client_sid,
    }
}

/// Connects a tonic `Channel` to the named pipe `name`. Uses a
/// `tower::service_fn` connector that dials a `NamedPipeClient`; the HTTP
/// authority is a placeholder (`http://atlas.local`) since named pipes have no
/// host — tonic requires a syntactically valid URI but never resolves it.
pub async fn connect(name: &str) -> anyhow::Result<tonic::transport::Channel> {
    let name = name.to_string();
    let channel = tonic::transport::Endpoint::try_from("http://atlas.local")?
        .connect_with_connector(tower::service_fn(move |_: tonic::transport::Uri| {
            let name = name.clone();
            async move {
                let client = dial(&name).await?;
                Ok::<_, io::Error>(hyper_util::rt::TokioIo::new(client))
            }
        }))
        .await?;
    Ok(channel)
}

/// Opens a `NamedPipeClient`, retrying briefly while all instances are busy
/// (the window between a client connecting and the server opening the next
/// instance). Mirrors the Win32 `WaitNamedPipe` idiom.
async fn dial(name: &str) -> io::Result<NamedPipeClient> {
    use tokio::time::{sleep, Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match ClientOptions::new().open(name) {
            Ok(c) => return Ok(c),
            Err(e)
                if matches!(
                    e.raw_os_error(),
                    Some(ERROR_PIPE_BUSY) | Some(ERROR_FILE_NOT_FOUND)
                ) =>
            {
                if Instant::now() >= deadline {
                    return Err(e);
                }
                sleep(Duration::from_millis(20)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_name_has_expected_prefix() {
        assert_eq!(pipe_name("abc"), r"\\.\pipe\SystemAtlas.dev.abc");
    }

    #[test]
    fn default_pipe_name_is_scoped() {
        let n = default_pipe_name();
        assert!(n.starts_with(r"\\.\pipe\SystemAtlas.dev."));
        // Never leaves a dangling empty discriminator.
        assert!(!n.ends_with("dev."));
    }
}
