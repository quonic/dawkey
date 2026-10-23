// SPDX-License-Identifier: AGPL-3.0-or-later
//! MCP over HTTP: the streamable HTTP transport at `http://<addr>/mcp`, for
//! clients that can't start `dawkey mcp --attach`. Only loopback addresses
//! are served, every request must carry the token as `Authorization: Bearer
//! <token>`, and requests naming another host (DNS rebinding) or coming
//! from a web page that isn't on this computer are refused.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock, mpsc};
use std::time::Duration;

use bytes::Bytes;
use dawkey_app::{App, McpEndpoint, McpHttpServer, tokens_match};
use dawkey_core::Handle;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue, WWW_AUTHENTICATE};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use tokio_util::sync::CancellationToken;

use crate::DawkeyMcp;

/// The path MCP is served at.
pub const MCP_PATH: &str = "/mcp";

type Token = Arc<RwLock<String>>;
type Service = StreamableHttpService<DawkeyMcp, LocalSessionManager>;

/// Listens on `addr`, which must be a loopback address (port 0 picks one).
pub fn bind_http(addr: SocketAddr) -> Result<std::net::TcpListener, String> {
    if !addr.ip().is_loopback() {
        return Err(format!(
            "{addr} isn't a loopback address: Dawkey serves MCP over HTTP only to this computer"
        ));
    }
    let listener =
        std::net::TcpListener::bind(addr).map_err(|e| format!("can't listen on {addr}: {e}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("can't listen on {addr}: {e}"))?;
    Ok(listener)
}

/// Serves MCP on `listener`, requiring `token`, until the process gets
/// SIGINT or SIGTERM.
pub async fn serve_http_until_signal(
    app: Handle<App>,
    listener: std::net::TcpListener,
    token: String,
) -> anyhow::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};

    let stop = CancellationToken::new();
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    tokio::spawn({
        let stop = stop.clone();
        async move {
            tokio::select! {
                _ = terminate.recv() => {}
                _ = interrupt.recv() => {}
            }
            stop.cancel();
        }
    });
    serve(app, listener, Arc::new(RwLock::new(token)), stop).await
}

/// Serves until `stop`; the listener is closed when it returns.
async fn serve(
    app: Handle<App>,
    listener: std::net::TcpListener,
    token: Token,
    stop: CancellationToken,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::from_std(listener)?;
    tracing::info!(
        "MCP listening on http://{}{MCP_PATH}",
        listener.local_addr()?
    );
    // Hosts are checked against the loopback names (the default); web pages
    // may only call from this computer, and still need the token.
    let config = StreamableHttpServerConfig::default()
        .with_allowed_origins(["http://localhost:*", "http://127.0.0.1:*", "http://[::1]:*"])
        .with_cancellation_token(stop.child_token());
    let service: Service = StreamableHttpService::new(
        move || Ok(DawkeyMcp::new(app.clone())),
        Arc::default(),
        config,
    );
    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(e) => {
                    // Out of file descriptors, most likely: wait a little.
                    tracing::warn!("MCP over HTTP: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
            () = stop.cancelled() => return Ok(()),
        };
        let (service, token, stop) = (service.clone(), token.clone(), stop.clone());
        tokio::spawn(async move {
            let handler = hyper::service::service_fn(move |request| {
                let (service, token) = (service.clone(), token.clone());
                async move { Ok::<_, Infallible>(respond(&service, &token, request).await) }
            });
            let builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
            let connection = builder.serve_connection(TokioIo::new(stream), handler);
            tokio::select! {
                served = connection => if let Err(e) = served {
                    tracing::debug!("MCP over HTTP connection: {e}");
                },
                () = stop.cancelled() => {}
            }
        });
    }
}

/// Refuses requests to other paths and without the token; MCP answers the
/// rest.
async fn respond(
    service: &Service,
    token: &Token,
    request: Request<Incoming>,
) -> Response<BoxBody<Bytes, Infallible>> {
    if request.uri().path() != MCP_PATH {
        return plain(StatusCode::NOT_FOUND, "Dawkey serves MCP at /mcp\n");
    }
    let given = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, given)| given.trim());
    let token = token.read().unwrap_or_else(|e| e.into_inner()).clone();
    if !given.is_some_and(|given| tokens_match(given.as_bytes(), token.as_bytes())) {
        let mut response = plain(
            StatusCode::UNAUTHORIZED,
            "Send Dawkey's MCP token as `Authorization: Bearer <token>`\n",
        );
        response
            .headers_mut()
            .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return response;
    }
    service.handle(request).await
}

fn plain(status: StatusCode, text: &'static str) -> Response<BoxBody<Bytes, Infallible>> {
    let mut response = Response::new(Full::new(Bytes::from_static(text.as_bytes())).boxed());
    *response.status_mut() = status;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

/// The window's HTTP server, started, moved and stopped by its preferences
/// (see [`dawkey_app::McpHttpServer`]).
#[derive(Debug)]
pub struct HttpServer {
    runtime: tokio::runtime::Handle,
    app: Handle<App>,
    running: Option<Running>,
}

#[derive(Debug)]
struct Running {
    /// The address asked for, and the one listened at.
    requested: SocketAddr,
    addr: SocketAddr,
    token: Token,
    stop: CancellationToken,
    /// Signalled once the listener is closed.
    stopped: mpsc::Receiver<()>,
}

impl HttpServer {
    /// A server that will run its connections on `runtime`.
    pub fn new(runtime: tokio::runtime::Handle, app: Handle<App>) -> Self {
        Self {
            runtime,
            app,
            running: None,
        }
    }

    fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            running.stop.cancel();
            // Wait for the port to close, so it can be listened on again.
            let _ = running.stopped.recv_timeout(Duration::from_secs(1));
        }
    }
}

impl McpHttpServer for HttpServer {
    fn serve(&mut self, endpoint: Option<McpEndpoint>) -> Result<Option<SocketAddr>, String> {
        let Some(McpEndpoint { addr, token }) = endpoint else {
            self.stop();
            return Ok(None);
        };
        if let Some(running) = &self.running
            && running.requested == addr
        {
            *running.token.write().unwrap_or_else(|e| e.into_inner()) = token;
            return Ok(Some(running.addr));
        }
        self.stop();
        let listener = bind_http(addr)?;
        let bound = listener.local_addr().map_err(|e| e.to_string())?;
        let token = Arc::new(RwLock::new(token));
        let stop = CancellationToken::new();
        let (done, stopped) = mpsc::sync_channel(1);
        self.runtime.spawn({
            let (app, token, stop) = (self.app.clone(), token.clone(), stop.clone());
            async move {
                if let Err(e) = serve(app, listener, token, stop).await {
                    tracing::warn!("MCP over HTTP stopped: {e:#}");
                }
                let _ = done.send(());
            }
        });
        self.running = Some(Running {
            requested: addr,
            addr: bound,
            token,
            stop,
            stopped,
        });
        Ok(Some(bound))
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    use dawkey_app::Platform;
    use dawkey_core::Project;
    use dawkey_studio::{AudioBackend, LoadedProject, Studio};

    use super::*;

    /// The status code of an empty POST to `/mcp` with `token`.
    fn status(addr: SocketAddr, token: &str) -> std::io::Result<u16> {
        let mut stream = TcpStream::connect(addr)?;
        write!(
            stream,
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\
             Content-Length: 0\r\nConnection: close\r\n\r\n"
        )?;
        let mut response = String::new();
        stream.read_to_string(&mut response)?;
        Ok(response[9..12].parse().unwrap())
    }

    #[test]
    fn the_window_server_moves_renews_and_stops() {
        let (app, _thread) = Handle::spawn_thread_with(
            || -> Result<App, String> {
                let studio = Studio::new(
                    LoadedProject::new(Project::new("Test")),
                    AudioBackend::Virtual,
                )
                .map_err(|e| e.to_string())?;
                Ok(App::new(studio, Platform::Headless))
            },
            Duration::from_millis(20),
            |_| {},
        )
        .unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut server = HttpServer::new(runtime.handle().clone(), app);
        let endpoint = |addr: &str, token: &str| {
            Some(McpEndpoint {
                addr: addr.parse().unwrap(),
                token: token.into(),
            })
        };

        let addr = server
            .serve(endpoint("127.0.0.1:0", "first"))
            .unwrap()
            .unwrap();
        assert_eq!(status(addr, "nope").unwrap(), 401);
        assert_ne!(status(addr, "first").unwrap(), 401);

        // The same address with a new token: the old one stops working.
        let same = server.serve(endpoint("127.0.0.1:0", "second")).unwrap();
        assert_eq!(same, Some(addr));
        assert_eq!(status(addr, "first").unwrap(), 401);
        assert_ne!(status(addr, "second").unwrap(), 401);

        // Stopped, the port closes, and it can be listened on again at once.
        assert_eq!(server.serve(None).unwrap(), None);
        assert!(status(addr, "second").is_err());
        let again = server.serve(endpoint(&addr.to_string(), "third")).unwrap();
        assert_eq!(again, Some(addr));
        assert_ne!(status(addr, "third").unwrap(), 401);

        let refused = server.serve(endpoint("0.0.0.0:0", "third")).unwrap_err();
        assert!(refused.contains("loopback"), "{refused}");
    }
}
