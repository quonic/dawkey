// SPDX-License-Identifier: AGPL-3.0-or-later
//! Running plugins in processes of their own (one per plugin file, shared
//! by its instances, or one per instance), so a plugin that crashes takes
//! only itself down. Dawkey starts the plugin host program (`dawkey
//! plugin-host`, [`run_plugin_host`]) with one end of a socket as its
//! stdin, and for each instance hands it shared memory for the audio
//! ([`shm`]) and tells it what to load ([`proto`]). The plugin runs there
//! as it would in-process; a [`host::BridgedProcessor`] stands in for it on
//! the audio thread.

mod child;
mod host;
mod proto;
mod realtime;
mod sandbox;
mod shm;
mod x11;

pub use child::run_plugin_host;
pub use host::BridgeProgram;
pub(crate) use host::{BridgedInstance, Processes, load};
pub use sandbox::Sandbox;
