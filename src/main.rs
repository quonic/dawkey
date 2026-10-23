// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `dawkey` binary.
//!
//! - `dawkey [PROJECT]` opens the window. It also serves MCP on a private
//!   Unix socket, `$XDG_RUNTIME_DIR/dawkey/<pid>.sock`, and over HTTP on a
//!   loopback port if the preferences say so.
//! - `dawkey mcp --headless [PROJECT]` serves MCP on stdio with no window
//!   (or over HTTP, with `--http ADDR`).
//! - `dawkey mcp --attach [--pid PID]` bridges stdio to a running window's
//!   socket.
//! - `dawkey scan-plugin FILE` describes the plugins in one `.clap` file or
//!   `.vst3` bundle as JSON. Dawkey runs it to scan plugins in a separate process, so one
//!   that crashes can't take the app down.

#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context;
use clap::{Args, Parser, Subcommand, ValueEnum};
use dawkey_app::{App, AudioChoice, Platform, Preferences};
use dawkey_core::{Handle, Project};
use dawkey_studio::{AudioBackend, LoadedProject, PipeWireOptions, Studio, StudioOptions, cache};
use tracing_subscriber::EnvFilter;

// Debug builds abort if the real-time audio path allocates.
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOCATOR: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

#[derive(Debug, Parser)]
#[command(version, about, args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    gui: GuiArgs,
}

#[derive(Debug, Args)]
struct GuiArgs {
    /// Audio backend for the window (default: the preferences', PipeWire
    /// unless changed).
    #[arg(long, value_enum)]
    audio: Option<Audio>,

    /// Don't link the outputs to the default sink (default: as the
    /// preferences say).
    #[arg(long)]
    no_autoconnect: bool,

    /// Don't serve MCP (on the Unix socket, or over HTTP as the preferences
    /// may say).
    #[arg(long)]
    no_mcp: bool,

    /// Project bundle to open.
    project: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run an MCP server on stdin/stdout.
    Mcp {
        /// Run without a window: the test-harness mode.
        #[arg(long, conflicts_with = "attach")]
        headless: bool,

        /// Connect to a running Dawkey window instead.
        #[arg(long)]
        attach: bool,

        /// With --attach: the window's process ID. Defaults to the most
        /// recently started one.
        #[arg(long, requires = "attach")]
        pid: Option<u32>,

        /// Audio backend (headless). `virtual` needs no device: time
        /// advances only through the advance_clock tool, so tests are
        /// deterministic.
        #[arg(long, value_enum, default_value_t = Audio::Virtual)]
        audio: Audio,

        /// With `--audio pipewire`: don't link the outputs to the default
        /// sink.
        #[arg(long)]
        no_autoconnect: bool,

        /// Run plugins in processes of their own, as the window does by
        /// default (a crashing plugin takes only itself down; each adds a
        /// block of latency while playing): `shared` (the default) runs the
        /// instances of one plugin file in one process, `separate` each
        /// instance in its own.
        #[arg(
            long,
            value_enum,
            value_name = "PROCESSES",
            num_args = 0..=1,
            require_equals = true,
            default_missing_value = "shared"
        )]
        bridge_plugins: Option<Processes>,

        /// With --bridge-plugins: run plugins without a sandbox (by default
        /// they may only write to their own folders, and not reach the
        /// network).
        #[arg(long, requires = "bridge_plugins")]
        no_sandbox: bool,

        /// Serve MCP over HTTP at http://ADDR/mcp instead of stdio, until
        /// SIGINT or SIGTERM. ADDR is a loopback address and port (port 0
        /// picks one); the URL is printed on stdout. Requests need
        /// `Authorization: Bearer <token>`, the token being $DAWKEY_MCP_TOKEN
        /// or else the one in $XDG_CONFIG_HOME/dawkey/mcp-token (made if
        /// there's none).
        #[arg(long, value_name = "ADDR", requires = "headless")]
        http: Option<SocketAddr>,

        /// Project bundle to open (headless).
        project: Option<PathBuf>,
    },
    /// Describe the plugins in one CLAP file or VST3 bundle as JSON (used by
    /// plugin scans).
    #[command(hide = true)]
    ScanPlugin { file: PathBuf },
    /// Run one plugin for Dawkey, which talks to it on stdin (each plugin
    /// runs in a process of its own).
    #[command(hide = true)]
    PluginHost,
}

/// How plugins run in processes of their own.
#[derive(Debug, Clone, Copy)]
struct Bridge {
    processes: Processes,
    /// In a sandbox (see `dawkey_plugins::Sandbox`).
    sandbox: bool,
}

/// How bridged plugins share processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Processes {
    /// Instances of the same plugin file share one.
    Shared,
    /// Each instance has its own.
    Separate,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Audio {
    Virtual,
    Pipewire,
}

fn main() -> ExitCode {
    // Logs go to stderr: stdout carries the MCP protocol in `mcp` mode.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_env("DAWKEY_LOG")
                // The Vulkan loader warns about unrelated drivers.
                .unwrap_or_else(|_| EnvFilter::new("warn,wgpu_hal=error")),
        )
        .init();

    let cli = Cli::parse();
    let result = match cli.command {
        None => run_gui(cli.gui),
        Some(Command::ScanPlugin { file }) => scan_plugin(&file),
        Some(Command::PluginHost) => dawkey_studio::run_plugin_host().map_err(Into::into),
        Some(Command::Mcp {
            attach: true, pid, ..
        }) => run_attach(pid),
        Some(Command::Mcp {
            headless: false, ..
        }) => Err(anyhow::anyhow!("pass --headless or --attach")),
        Some(Command::Mcp {
            audio,
            no_autoconnect,
            bridge_plugins,
            no_sandbox,
            http,
            project,
            ..
        }) => {
            let bridge = bridge_plugins.map(|processes| Bridge {
                processes,
                sandbox: !no_sandbox,
            });
            run_headless(audio, no_autoconnect, bridge, http, project)
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("dawkey: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn scan_plugin(file: &Path) -> anyhow::Result<()> {
    let plugins = dawkey_plugins::scan_plugin_file(file)?;
    // The only thing on stdout: the scanning process reads it.
    serde_json::to_writer(std::io::stdout().lock(), &plugins)?;
    Ok(())
}

/// The studio. The window scans plugins in the background (`background`);
/// headless instances wait for scans, so tools see every plugin.
fn studio(
    audio: Audio,
    no_autoconnect: bool,
    project: Option<PathBuf>,
    background: bool,
    plugin_search: Option<dawkey_studio::SearchPath>,
    bridge: Option<Bridge>,
) -> anyhow::Result<Studio> {
    // Takes left behind by a Dawkey that didn't exit cleanly.
    cache::sweep_stale(&cache::recordings_root(), cache::STALE_AFTER);
    let loaded = match project {
        Some(path) => LoadedProject::open(&path)?,
        None => LoadedProject::new(Project::default()),
    };
    let backend = match audio {
        Audio::Virtual => AudioBackend::Virtual,
        Audio::Pipewire => AudioBackend::PipeWire(PipeWireOptions {
            autoconnect: !no_autoconnect,
            ..PipeWireOptions::default()
        }),
    };
    let exe = std::env::current_exe().ok();
    let options = StudioOptions {
        // Scan each plugin file in its own process (see `scan-plugin`).
        plugin_scanner: exe.clone().map(|exe| (exe, vec!["scan-plugin".into()])),
        // Run plugins in processes of their own (see `plugin-host`).
        plugin_bridge: exe
            .zip(bridge)
            .map(|(program, bridge)| dawkey_studio::BridgeProgram {
                program,
                args: vec!["plugin-host".into()],
                share: bridge.processes == Processes::Shared,
                sandbox: bridge.sandbox.then(dawkey_studio::Sandbox::default),
            }),
        background_scan: background,
        plugin_search: plugin_search.map(|search| (search, dawkey_studio::default_plugin_cache())),
    };
    Ok(Studio::with_options(loaded, backend, options)?)
}

fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting async runtime")
}

fn run_headless(
    audio: Audio,
    no_autoconnect: bool,
    bridge: Option<Bridge>,
    http: Option<SocketAddr>,
    project: Option<PathBuf>,
) -> anyhow::Result<()> {
    // Listen before starting anything, so a taken port fails at once.
    let http = http
        .map(|addr| -> anyhow::Result<_> {
            let token = match std::env::var("DAWKEY_MCP_TOKEN") {
                Ok(token) if !token.trim().is_empty() => token.trim().to_owned(),
                _ => {
                    let path = dawkey_app::default_token_path()
                        .context("set $DAWKEY_MCP_TOKEN or $HOME for the MCP token")?;
                    dawkey_app::load_or_create_token(&path)
                        .with_context(|| format!("reading {}", path.display()))?
                }
            };
            let listener = dawkey_mcp::http::bind_http(addr).map_err(anyhow::Error::msg)?;
            Ok((listener, token))
        })
        .transpose()?;
    // The app (and its plugins) lives on its own thread from the start.
    let (handle, app_thread) = Handle::spawn_thread_with(
        move || -> anyhow::Result<App> {
            Ok(App::new(
                // Headless instances ignore the preferences, so they (and
                // tests) behave the same everywhere.
                studio(audio, no_autoconnect, project, false, None, bridge)?,
                Platform::Headless,
            ))
        },
        Duration::from_millis(20),
        App::tick,
    )?;
    match http {
        None => runtime()?.block_on(dawkey_mcp::serve_stdio(handle))?,
        Some((listener, token)) => {
            use std::io::Write;
            // Stdout doesn't carry MCP here: it says where to connect.
            let url = dawkey_app::mcp_url(listener.local_addr()?);
            let mut stdout = std::io::stdout().lock();
            writeln!(stdout, "{url}")?;
            stdout.flush()?;
            drop(stdout);
            runtime()?.block_on(dawkey_mcp::http::serve_http_until_signal(
                handle, listener, token,
            ))?;
        }
    }

    // The server dropped its handles when the client disconnected (or the
    // runtime, with every HTTP session, was dropped), so the app thread
    // finishes once in-flight jobs are done, dropping the app (which stops
    // the audio backend).
    app_thread
        .join()
        .map_err(|_| anyhow::anyhow!("app thread panicked"))?;
    Ok(())
}

fn run_gui(args: GuiArgs) -> anyhow::Result<()> {
    let prefs_path = Preferences::default_path();
    let prefs = prefs_path
        .as_deref()
        .map(Preferences::load)
        .unwrap_or_default();
    // What's on the command line wins.
    let audio = args.audio.unwrap_or(match prefs.audio {
        AudioChoice::Pipewire => Audio::Pipewire,
        AudioChoice::Virtual => Audio::Virtual,
    });
    let no_autoconnect = args.no_autoconnect || !prefs.autoconnect;
    let mut app = App::new(
        studio(
            audio,
            no_autoconnect,
            args.project,
            true,
            Some(prefs.plugin_search()),
            prefs.bridge_plugins.then_some(Bridge {
                processes: if prefs.share_plugin_processes {
                    Processes::Shared
                } else {
                    Processes::Separate
                },
                sandbox: prefs.sandbox_plugins,
            }),
        )?,
        Platform::Wayland,
    );
    app.set_preferences(prefs, prefs_path);
    // Changes a Dawkey that didn't exit cleanly kept.
    app.offer_recovery();
    let socket = (!args.no_mcp)
        .then(|| socket_dir().map(|dir| dir.join(format!("{}.sock", std::process::id()))))
        .transpose()?;
    let token_path = (!args.no_mcp).then(dawkey_app::default_token_path).flatten();
    // MCP's clients are served on this runtime's threads.
    let mcp = runtime()?;
    let result = dawkey_wayland::run(app, |handle| {
        if let Some(path) = socket.clone() {
            let handle = handle.clone();
            mcp.spawn(async move {
                if let Err(e) = dawkey_mcp::serve_unix(handle, &path).await {
                    tracing::warn!("MCP socket stopped: {e:#}");
                }
            });
        }
        // The preferences start and stop it.
        if let Some(token_path) = token_path {
            let server = dawkey_mcp::http::HttpServer::new(mcp.handle().clone(), handle.clone());
            let _ = handle.send(move |app| app.set_mcp_http_server(Box::new(server), token_path));
        }
    });
    if let Some(path) = &socket {
        let _ = std::fs::remove_file(path);
    }
    // The app (and its HTTP server) goes before the runtime.
    drop(result?);
    mcp.shutdown_timeout(Duration::from_secs(1));
    Ok(())
}

fn run_attach(pid: Option<u32>) -> anyhow::Result<()> {
    let path = match pid {
        Some(pid) => socket_dir()?.join(format!("{pid}.sock")),
        None => newest_live_socket(&socket_dir()?)?,
    };
    runtime()?.block_on(dawkey_mcp::attach(&path))
}

/// `$XDG_RUNTIME_DIR/dawkey`, created owner-only.
fn socket_dir() -> anyhow::Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    let dir = Path::new(&runtime).join("dawkey");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

/// The socket of the most recently started Dawkey that is still running.
fn newest_live_socket(dir: &Path) -> anyhow::Result<PathBuf> {
    let mut candidates = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(pid) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if path.extension().is_some_and(|e| e == "sock")
            && Path::new(&format!("/proc/{pid}")).exists()
        {
            let modified = std::fs::metadata(&path)?.modified()?;
            candidates.push((modified, path));
        }
    }
    candidates
        .into_iter()
        .max()
        .map(|(_, path)| path)
        .context("no running Dawkey window found (start one with `dawkey`)")
}
