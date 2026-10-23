// SPDX-License-Identifier: AGPL-3.0-or-later
//! Drives a Dawkey process over MCP, exactly as Claude or any other MCP
//! client would.
//!
//! ```ignore
//! let dawkey = Harness::headless(env!("CARGO_BIN_EXE_dawkey")).await?;
//! let added = dawkey.call("add_track", json!({ "name": "Drums" })).await?;
//! ```

#![forbid(unsafe_code)]

use std::ffi::OsStr;
use std::path::Path;

use anyhow::{Context, bail};
use rmcp::model::{CallToolRequestParams, CallToolResult, Tool};
use rmcp::service::RunningService;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{ConfigureCommandExt, StreamableHttpClientTransport, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde_json::Value;

/// A running Dawkey process with an MCP client connected to it.
#[derive(Debug)]
pub struct Harness {
    client: RunningService<RoleClient, ()>,
    /// The process's XDG cache directory (where recordings of unsaved
    /// projects go), so tests never write to the real one.
    _cache: tempfile::TempDir,
    /// The process, when the client talks to it over HTTP (over stdio, the
    /// transport owns it).
    http: Option<HttpProcess>,
}

#[derive(Debug)]
struct HttpProcess {
    child: tokio::process::Child,
    url: String,
}

impl Harness {
    /// Starts `dawkey mcp --headless [extra args]` and connects to it.
    pub async fn headless<I, S>(binary: impl AsRef<Path>, extra_args: I) -> anyhow::Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        Self::headless_with_env(binary, extra_args, std::iter::empty::<(&str, &OsStr)>()).await
    }

    /// Like [`headless`](Self::headless), with extra environment variables
    /// (e.g. `CLAP_PATH` and `LV2_PATH` for test plugins).
    pub async fn headless_with_env<I, S, E, K, V>(
        binary: impl AsRef<Path>,
        extra_args: I,
        env: E,
    ) -> anyhow::Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
        E: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        let cache = tempfile::tempdir().context("creating a cache directory")?;
        let command = tokio::process::Command::new(binary.as_ref()).configure(|cmd| {
            cmd.args(["mcp", "--headless"])
                .args(extra_args)
                .envs(env)
                .env("XDG_CACHE_HOME", cache.path());
        });
        let transport = TokioChildProcess::new(command)
            .with_context(|| format!("spawning {}", binary.as_ref().display()))?;
        let client = ().serve(transport).await.context("MCP handshake")?;
        Ok(Self {
            client,
            _cache: cache,
            http: None,
        })
    }

    /// Starts `dawkey mcp --headless --http 127.0.0.1:0 [extra args]` with
    /// `token` as its MCP token, and connects to it over HTTP.
    pub async fn headless_http<I, S>(
        binary: impl AsRef<Path>,
        extra_args: I,
        token: &str,
    ) -> anyhow::Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        use tokio::io::AsyncBufReadExt;

        let cache = tempfile::tempdir().context("creating a cache directory")?;
        let mut child = tokio::process::Command::new(binary.as_ref())
            .args(["mcp", "--headless", "--http", "127.0.0.1:0"])
            .args(extra_args)
            .env("XDG_CACHE_HOME", cache.path())
            .env("DAWKEY_MCP_TOKEN", token)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawning {}", binary.as_ref().display()))?;
        // It prints its URL once it listens.
        let stdout = child.stdout.take().context("no stdout")?;
        let mut url = String::new();
        tokio::io::BufReader::new(stdout)
            .read_line(&mut url)
            .await
            .context("reading the URL")?;
        let url = url.trim().to_owned();
        if !url.starts_with("http://") {
            bail!("dawkey printed {url:?}, not a URL");
        }
        let transport = StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(url.clone()).auth_header(token),
        );
        let client = ().serve(transport).await.context("MCP handshake")?;
        Ok(Self {
            client,
            _cache: cache,
            http: Some(HttpProcess { child, url }),
        })
    }

    /// Connects another client to an instance that serves HTTP at `url`.
    pub async fn connect_http(url: &str, token: &str) -> anyhow::Result<Self> {
        let transport = StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(url.to_owned()).auth_header(token),
        );
        let client = ().serve(transport).await.context("MCP handshake")?;
        Ok(Self {
            client,
            _cache: tempfile::tempdir()?,
            http: None,
        })
    }

    /// The URL an HTTP instance serves MCP at.
    pub fn url(&self) -> Option<&str> {
        self.http.as_ref().map(|h| h.url.as_str())
    }

    /// The server's name from the handshake.
    pub fn server_name(&self) -> Option<String> {
        self.client
            .peer_info()
            .and_then(|info| info.server_info.clone())
            .map(|implementation| implementation.name)
    }

    pub async fn tools(&self) -> anyhow::Result<Vec<Tool>> {
        Ok(self.client.list_all_tools().await?)
    }

    /// Calls a tool and returns its raw result.
    pub async fn call_raw(&self, tool: &str, args: Value) -> anyhow::Result<CallToolResult> {
        let Value::Object(args) = args else {
            bail!("tool arguments must be a JSON object, got {args}");
        };
        let params = CallToolRequestParams::new(tool.to_owned()).with_arguments(args);
        Ok(self.client.call_tool(params).await?)
    }

    /// Calls a tool and returns its structured result. Fails if the tool
    /// reported an error.
    pub async fn call(&self, tool: &str, args: Value) -> anyhow::Result<Value> {
        let result = self.call_raw(tool, args).await?;
        if result.is_error == Some(true) {
            bail!("{tool} failed: {}", text_of(&result));
        }
        result
            .structured_content
            .with_context(|| format!("{tool} returned no structured content"))
    }

    /// Calls a tool that is expected to fail and returns its error message.
    pub async fn call_err(&self, tool: &str, args: Value) -> anyhow::Result<String> {
        let result = self.call_raw(tool, args).await?;
        if result.is_error != Some(true) {
            bail!("{tool} unexpectedly succeeded: {}", text_of(&result));
        }
        Ok(text_of(&result))
    }

    /// Disconnects and waits for the process to exit. An HTTP instance is
    /// sent SIGTERM, and must exit cleanly.
    pub async fn shutdown(self) -> anyhow::Result<()> {
        self.client.cancel().await?;
        if let Some(HttpProcess { mut child, .. }) = self.http {
            let pid = child.id().context("the process already exited")?;
            let killed = tokio::process::Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .status()
                .await?;
            if !killed.success() {
                bail!("kill -TERM {pid} failed");
            }
            let status = child.wait().await?;
            if !status.success() {
                bail!("dawkey exited with {status}");
            }
        }
        Ok(())
    }
}

fn text_of(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.as_str()))
        .collect::<Vec<_>>()
        .join("\n")
}
