// SPDX-License-Identifier: AGPL-3.0-or-later
//! MCP over HTTP, an opt-in setting: the window serves MCP's streamable HTTP
//! transport on a loopback port (`Preferences::mcp_http`), for clients that
//! can't start `dawkey mcp --attach`. Every request must carry the token
//! kept in `$XDG_CONFIG_HOME/dawkey/mcp-token` as `Authorization: Bearer`.
//! The server itself lives in `dawkey-mcp`; the binary hands the window one
//! ([`App::set_mcp_http_server`]), and the preferences start, move and stop
//! it.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use crate::App;

/// The port the preferences offer until another is typed.
pub const DEFAULT_MCP_PORT: u16 = 7381;

/// Where an HTTP server listens, and the token it takes.
#[derive(Clone, PartialEq, Eq)]
pub struct McpEndpoint {
    pub addr: SocketAddr,
    pub token: String,
}

impl std::fmt::Debug for McpEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never log the token.
        f.debug_struct("McpEndpoint")
            .field("addr", &self.addr)
            .finish_non_exhaustive()
    }
}

/// Serves MCP over HTTP for the window.
pub trait McpHttpServer {
    /// Serves at `endpoint`, replacing what was served (a new token applies
    /// to the next request), or stops serving (`None`). Returns the address
    /// it listens at, or why it can't (the port is taken).
    fn serve(&mut self, endpoint: Option<McpEndpoint>) -> Result<Option<SocketAddr>, String>;
}

/// What the preferences panel says about the HTTP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpHttpState {
    /// This instance has no HTTP server (headless instances serve stdio,
    /// or HTTP from the command line).
    Unavailable,
    Off,
    Serving(SocketAddr),
    Failed(String),
}

/// `$XDG_CONFIG_HOME/dawkey/mcp-token` (`~/.config` if unset).
pub fn default_token_path() -> Option<PathBuf> {
    crate::Preferences::default_path().map(|p| p.with_file_name("mcp-token"))
}

/// The token in `path`, made (owner-only) if there's none yet.
pub fn load_or_create_token(path: &Path) -> std::io::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(token) if !token.trim().is_empty() => Ok(token.trim().to_owned()),
        Ok(_) => new_token(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => new_token(path),
        Err(e) => Err(e),
    }
}

/// Replaces the token in `path` with a new random one, so clients with the
/// old one are refused.
pub fn new_token(path: &Path) -> std::io::Result<String> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(token.as_bytes())?;
    file.write_all(b"\n")?;
    drop(file);
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&tmp, path)?;
    Ok(token)
}

/// Whether two tokens are equal, taking as long whatever differs.
pub fn tokens_match(given: &[u8], token: &[u8]) -> bool {
    given.len() == token.len() && given.iter().zip(token).fold(0, |d, (a, b)| d | (a ^ b)) == 0
}

/// `path` as shown, with the home folder as `~`.
pub(crate) fn tilde(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home
        .as_deref()
        .and_then(|home| path.strip_prefix(home).ok())
    {
        Some(rest) => Path::new("~").join(rest).display().to_string(),
        None => path.display().to_string(),
    }
}

/// The server's URL at `addr`.
pub fn mcp_url(addr: SocketAddr) -> String {
    format!("http://{addr}/mcp")
}

impl App {
    /// Gives the window an HTTP server, which the preferences start and
    /// stop, taking its token from `token_path`.
    pub fn set_mcp_http_server(&mut self, server: Box<dyn McpHttpServer>, token_path: PathBuf) {
        self.mcp_http = Some(server);
        self.mcp_token_path = Some(token_path);
        self.apply_mcp_http();
    }

    /// What the HTTP server is doing.
    pub fn mcp_http_state(&self) -> &McpHttpState {
        &self.mcp_http_state
    }

    /// Starts, moves or stops the server as the preferences say.
    pub(crate) fn apply_mcp_http(&mut self) {
        let Some(server) = self.mcp_http.as_mut() else {
            self.mcp_http_state = McpHttpState::Unavailable;
            return;
        };
        let endpoint = match (&self.mcp_token_path, self.prefs.mcp_http) {
            (_, false) => None,
            (None, true) => {
                self.mcp_http_state = McpHttpState::Failed("no place for a token".into());
                return;
            }
            (Some(path), true) => match load_or_create_token(path) {
                Ok(token) => Some(McpEndpoint {
                    addr: (Ipv4Addr::LOCALHOST, self.prefs.mcp_http_port).into(),
                    token,
                }),
                Err(e) => {
                    let _ = server.serve(None);
                    self.mcp_http_state =
                        McpHttpState::Failed(format!("reading {}: {e}", path.display()));
                    return;
                }
            },
        };
        self.mcp_http_state = match server.serve(endpoint) {
            Ok(Some(addr)) => McpHttpState::Serving(addr),
            Ok(None) => McpHttpState::Off,
            Err(e) => McpHttpState::Failed(e),
        };
    }

    /// Makes a new token (the panel's "New token"); clients with the old
    /// one are refused from their next request.
    pub(crate) fn renew_mcp_token(&mut self) -> Result<(), String> {
        let path = self
            .mcp_token_path
            .clone()
            .ok_or("this instance doesn't serve MCP over HTTP")?;
        new_token(&path).map_err(|e| format!("writing {}: {e}", path.display()))?;
        self.apply_mcp_http();
        self.message = Some("Made a new MCP token".into());
        Ok(())
    }

    /// Where the token is kept, if this instance serves HTTP.
    pub(crate) fn mcp_token_path(&self) -> Option<&Path> {
        self.mcp_token_path.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_owner_only_and_kept() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dawkey/mcp-token");
        let token = load_or_create_token(&path).unwrap();
        assert_eq!(token.len(), 64);
        assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(load_or_create_token(&path).unwrap(), token);
        let renewed = new_token(&path).unwrap();
        assert_ne!(renewed, token);
        assert_eq!(load_or_create_token(&path).unwrap(), renewed);
    }

    #[test]
    fn tokens_match_only_exactly() {
        assert!(tokens_match(b"abc", b"abc"));
        assert!(!tokens_match(b"abd", b"abc"));
        assert!(!tokens_match(b"ab", b"abc"));
        assert!(!tokens_match(b"", b"abc"));
    }
}
