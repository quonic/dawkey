// SPDX-License-Identifier: AGPL-3.0-or-later
//! Audio buffers for plugins with ports (CLAP) or buses (VST3): the
//! track's two channels into the main input, the sidechain into the first
//! other one, and the outputs back.

use dk_engine::{AuxBuffer, MAX_BLOCK};

/// A plugin's audio ports in one direction: channel counts, and which is
/// the main one.
#[derive(Debug, Clone, Default)]
pub(crate) struct Ports {
    pub(crate) channels: Vec<u32>,
    pub(crate) names: Vec<String>,
    pub(crate) main: Option<usize>,
}

impl Ports {
    /// The ports besides the main one, in order.
    pub(crate) fn others(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.channels.len()).filter(|&i| Some(i) != self.main)
    }

    /// Channels of the main port (0 without one).
    pub(crate) fn main_channels(&self) -> u32 {
        self.main.map_or(0, |m| self.channels[m])
    }

    /// A buffer for each port.
    pub(crate) fn buffers(&self) -> Vec<PortBuffer> {
        self.channels
            .iter()
            .map(|&c| PortBuffer {
                channels: c as usize,
                data: vec![0.0; c as usize * MAX_BLOCK],
            })
            .collect()
    }

    /// Channels of every port together.
    pub(crate) fn total(&self) -> usize {
        self.channels.iter().sum::<u32>() as usize
    }
}

/// Audio buffers for one plugin port: `channels` runs of [`MAX_BLOCK`].
pub(crate) struct PortBuffer {
    pub(crate) channels: usize,
    pub(crate) data: Vec<f32>,
}

/// The track's signal into the main input, the sidechain into its own;
/// silence elsewhere. Mono ports get the average.
pub(crate) fn fill_inputs(
    inputs: &mut [PortBuffer],
    main_in: Option<usize>,
    side_in: Option<usize>,
    track: [&[f32]; 2],
    sidechain: Option<[&[f32]; 2]>,
    frames: usize,
) {
    for (i, port) in inputs.iter_mut().enumerate() {
        let source = if Some(i) == main_in {
            Some(track)
        } else if Some(i) == side_in {
            sidechain
        } else {
            None
        };
        for c in 0..port.channels {
            let channel = &mut port.data[c * MAX_BLOCK..c * MAX_BLOCK + frames];
            match (source, port.channels, c) {
                (Some([l, r]), 1, _) => {
                    for (s, (l, r)) in channel.iter_mut().zip(l.iter().zip(r.iter())) {
                        *s = (l + r) * 0.5;
                    }
                }
                (Some([l, _]), _, 0) => channel.copy_from_slice(&l[..frames]),
                (Some([_, r]), _, 1) => channel.copy_from_slice(&r[..frames]),
                _ => channel.fill(0.0),
            }
        }
    }
}

/// The other outputs that go somewhere into `aux`, and the main output
/// into the track's channels (mono to both; with no main output the
/// signal passes by).
pub(crate) fn copy_outputs(
    outputs: &[PortBuffer],
    main_out: Option<usize>,
    other_out: &[usize],
    [left, right]: [&mut [f32]; 2],
    aux: &mut [AuxBuffer],
    frames: usize,
) {
    for (n, buffer) in aux.iter_mut().enumerate() {
        let [l, r] = &mut buffer.channels;
        match other_out.get(n).map(|&p| &outputs[p]) {
            Some(port) if port.channels == 1 => {
                l[..frames].copy_from_slice(&port.data[..frames]);
                r[..frames].copy_from_slice(&port.data[..frames]);
            }
            Some(port) if port.channels >= 2 => {
                l[..frames].copy_from_slice(&port.data[..frames]);
                r[..frames].copy_from_slice(&port.data[MAX_BLOCK..MAX_BLOCK + frames]);
            }
            _ => {
                l[..frames].fill(0.0);
                r[..frames].fill(0.0);
            }
        }
    }
    match main_out.map(|m| &outputs[m]) {
        Some(port) if port.channels == 1 => {
            left[..frames].copy_from_slice(&port.data[..frames]);
            right[..frames].copy_from_slice(&port.data[..frames]);
        }
        Some(port) if port.channels >= 2 => {
            left[..frames].copy_from_slice(&port.data[..frames]);
            right[..frames].copy_from_slice(&port.data[MAX_BLOCK..MAX_BLOCK + frames]);
        }
        // No audio output: the signal passes by.
        None => {}
        Some(_) => {
            left.fill(0.0);
            right.fill(0.0);
        }
    }
}

/// Silences the track and the other outputs (a plugin that failed).
pub(crate) fn silence([left, right]: [&mut [f32]; 2], aux: &mut [AuxBuffer], frames: usize) {
    left.fill(0.0);
    right.fill(0.0);
    for buffer in aux.iter_mut() {
        buffer
            .channels
            .iter_mut()
            .for_each(|c| c[..frames].fill(0.0));
    }
}
