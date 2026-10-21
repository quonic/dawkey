// SPDX-License-Identifier: AGPL-3.0-or-later
//! Peak meters written by the real-time thread and read by the controller.

use std::sync::atomic::{AtomicU32, Ordering};

use dk_core::TrackId;
use dk_media::gain_to_db;
use serde::Serialize;

/// Peak of one stereo signal. Stores `f32` bits; for non-negative floats the
/// bit patterns order the same way as the values, so `fetch_max` works.
#[derive(Debug, Default)]
pub(crate) struct StereoPeak([AtomicU32; 2]);

impl StereoPeak {
    pub(crate) fn record(&self, left: f32, right: f32) {
        self.0[0].fetch_max(left.to_bits(), Ordering::Relaxed);
        self.0[1].fetch_max(right.to_bits(), Ordering::Relaxed);
    }

    fn take_db(&self) -> [Option<f64>; 2] {
        self.0
            .each_ref()
            .map(|a| gain_to_db(f64::from(f32::from_bits(a.swap(0, Ordering::Relaxed)))))
    }
}

/// Meters for one compiled graph.
#[derive(Debug)]
pub(crate) struct MeterBank {
    pub(crate) master: StereoPeak,
    pub(crate) tracks: Box<[StereoPeak]>,
    track_ids: Box<[TrackId]>,
}

impl MeterBank {
    pub(crate) fn new(track_ids: Vec<TrackId>) -> Self {
        Self {
            master: StereoPeak::default(),
            tracks: track_ids.iter().map(|_| StereoPeak::default()).collect(),
            track_ids: track_ids.into(),
        }
    }

    /// Returns peaks since the last read and resets them.
    pub(crate) fn take(&self) -> MeterReading {
        MeterReading {
            master: self.master.take_db(),
            tracks: self
                .track_ids
                .iter()
                .zip(self.tracks.iter())
                .map(|(&track, peak)| TrackMeter {
                    track,
                    peak_dbfs: peak.take_db(),
                })
                .collect(),
        }
    }
}

/// Peak levels since the previous reading, in dBFS. `None` means silence.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MeterReading {
    pub master: [Option<f64>; 2],
    pub tracks: Vec<TrackMeter>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TrackMeter {
    pub track: TrackId,
    /// Post-fader peak, left and right.
    pub peak_dbfs: [Option<f64>; 2],
}
