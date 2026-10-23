// SPDX-License-Identifier: AGPL-3.0-or-later
//! Dawkey's built-in DSP.
//!
//! Everything here runs on the real-time thread: after construction nothing
//! allocates, locks or blocks.

#![forbid(unsafe_code)]

mod synth;

pub use synth::{PolySynth, VOICES};

#[cfg(test)]
#[global_allocator]
static ALLOCATOR: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;
