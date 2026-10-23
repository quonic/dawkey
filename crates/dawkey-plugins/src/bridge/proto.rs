// SPDX-License-Identifier: AGPL-3.0-or-later
//! What Dawkey and a plugin host process say to each other over their
//! socket, besides the shared memory: frames of a JSON message and a blob
//! of bytes (a plugin's state), each with its length first. A process may
//! host several instances, so each message is in an [`Envelope`] with the
//! instance it's about.

use std::io::{self, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::{ParamChange, PluginInfo, SearchPath, StatePaths};

/// The longest frame part accepted (plugin states can be big).
const MAX_PART: u32 = 512 * 1024 * 1024;

/// A message about one of the instances a process hosts. A request has a
/// number its reply carries back (so a reply that comes late, after a GUI's
/// dialog kept the process busy, isn't taken for the next one's); notices
/// have 0.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Envelope<T> {
    pub(crate) instance: u32,
    #[serde(default)]
    pub(crate) request: u64,
    pub(crate) message: T,
}

/// From Dawkey to the host process.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum ToHost {
    /// Load this plugin as the instance (its state is the blob). The
    /// instance's shared memory follows the frame, as a descriptor.
    Load {
        info: Box<PluginInfo>,
        sample_rate: u32,
        search: SearchPath,
        has_state: bool,
        paths: StatePaths,
        /// For an offline render (or the virtual clock).
        #[serde(default)]
        offline: bool,
    },
    ParamText {
        index: usize,
        value: f64,
    },
    ParamValue {
        index: usize,
        text: String,
    },
    SaveState {
        paths: StatePaths,
    },
    /// Show the plugin's GUI (titled this), or hide it.
    ShowGui {
        title: String,
    },
    HideGui,
    /// Unload the instance.
    Unload,
    /// Unload every instance and exit.
    Quit,
    /// Enter this sandbox, before loading anything (for the process: the
    /// instance is 0).
    Sandbox(super::sandbox::Sandbox),
}

/// From the host process to Dawkey: replies, in order, and notices.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum FromHost {
    /// The instance is unloaded (the reply to `Unload`).
    Unloaded,
    /// The process is in its sandbox, as far as the kernel enforces it.
    Sandboxed(super::sandbox::Entered),
    /// The plugin is loaded and its audio thread waits for blocks.
    Loaded {
        latency: u32,
        /// It has a GUI that can be shown.
        gui: bool,
    },
    /// The GUI is showing, or hidden (replies to `ShowGui` and `HideGui`).
    Gui {
        visible: bool,
    },
    GuiFailed {
        error: String,
    },
    Failed {
        error: String,
    },
    Text(Option<String>),
    Value(Option<f64>),
    /// Its state is the blob (`saves`: it saves any at all).
    State {
        saves: bool,
    },
    StateFailed {
        error: String,
    },
    /// Notice: the plugin said its state changed.
    StateDirty,
    /// Notice: the plugin changed parameters itself.
    ParamsChanged(Vec<ParamChange>),
    /// Notice: its GUI was closed (its window, or by itself).
    GuiClosed,
}

/// Writes a frame.
pub(crate) fn write_frame(
    out: &mut impl Write,
    message: &impl Serialize,
    blob: &[u8],
) -> io::Result<()> {
    let json = serde_json::to_vec(message).map_err(io::Error::other)?;
    let mut frame = Vec::with_capacity(8 + json.len() + blob.len());
    frame.extend_from_slice(&(json.len() as u32).to_le_bytes());
    frame.extend_from_slice(&json);
    frame.extend_from_slice(&(blob.len() as u32).to_le_bytes());
    frame.extend_from_slice(blob);
    out.write_all(&frame)?;
    out.flush()
}

/// Reads a frame: the message and its blob.
pub(crate) fn read_frame<T: DeserializeOwned>(input: &mut impl Read) -> io::Result<(T, Vec<u8>)> {
    let mut part = || -> io::Result<Vec<u8>> {
        let mut len = [0u8; 4];
        input.read_exact(&mut len)?;
        let len = u32::from_le_bytes(len);
        if len > MAX_PART {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too long"));
        }
        let mut bytes = vec![0u8; len as usize];
        input.read_exact(&mut bytes)?;
        Ok(bytes)
    };
    let json = part()?;
    let blob = part()?;
    let message = serde_json::from_slice(&json).map_err(io::Error::other)?;
    Ok((message, blob))
}
