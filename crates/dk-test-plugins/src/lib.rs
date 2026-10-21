// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tiny plugins for Dawkey's tests, in one shared library that is a CLAP
//! plugin file, an LV2 binary and a VST3 binary. Their output is simple and exact, so
//! tests can check what the host did sample for sample:
//!
//! | ID | Format | Does |
//! |---|---|---|
//! | `dawkey.test.gain` | CLAP | stereo gain; param 1 "Gain", -60 to +12 dB; showing -59.5 dB as text takes 3 s (`STALL_DB`: a stand-in for a GUI's dialog holding up the main thread) |
//! | `dawkey.test.delay` | CLAP | delays by [`DELAY_FRAMES`] and reports that latency |
//! | `dawkey.test.beep` | CLAP | instrument: outputs `velocity × level` (DC) while a key is held, scaled by the key's volume note expression; param 1 "Level" |
//! | `urn:dawkey:test:gain` | LV2 | stereo gain; control port `gain` in dB |
//! | `dawkey.test.octave` | CLAP | MIDI effect: sends each note an octave up; no audio ports |
//! | `urn:dawkey:test:beep` | LV2 | instrument like the CLAP beep, MIDI through an atom port |
//! | `urn:dawkey:test:octave` | LV2 | MIDI effect like the CLAP octave, atom ports in and out; says it has a `ui:Qt5UI` (which its library doesn't have) |
//! | `dawkey.test.memo` | CLAP | effect that takes notes: adds the last note's velocity (0 to 1) to its audio as DC, remembering it only in its state (and marking the state dirty) |
//! | `urn:dawkey:test:memo` | LV2 | like the CLAP memo, with `state:interface`; says it has a `ui:GtkUI` (which its library doesn't have) |
//! | `dawkey.test.tempo` | CLAP | shows the transport: left = tempo / 1000, right = (bar + 1) / 100 while playing (0 stopped) |
//! | `urn:dawkey:test:tempo` | LV2 | like the CLAP tempo, from `time:Position` |
//! | `dawkey.test.subtract` | CLAP | stereo effect with a stereo sidechain input: outputs main − sidechain |
//! | `urn:dawkey:test:subtract` | LV2 | like the CLAP subtract (`lv2:isSideChain` ports) |
//! | `dawkey.test.split` | CLAP | instrument with two stereo outputs: DC of the held velocities below key 60 on the main one, of 60 and up on "High" |
//! | `urn:dawkey:test:split` | LV2 | like the CLAP split (outputs "High L"/"High R") |
//! | `urn:dawkey:test:worker` | LV2 | effect that takes notes: adds a level to its audio as DC, set only through `work:schedule` (by a note's velocity, a `patch:Set`, or restoring its state); a `meter` output port, a notify port that needs 100 000 bytes, and an X11 UI that reports what it's sent (see `lv2/worker.rs`), and says it has a `ui:Gtk3UI` and a `ui:Qt6UI` too |
//!
//!
//! The VST3 set (`vst3:<class ID>`; [`vst3_id`] gives the IDs) is the CLAP
//! one again, "Test Gain (VST3)" and so on, with the same outputs, in
//! VST3's terms: parameters are normalized (the gain's 0 to 1 is -60 to
//! +12 dB), and what a CLAP plugin does in its processor a VST3 one does
//! through its controller. The beep's key 127 reaches its controller,
//! which turns Level to half through the host's component handler; MIDI
//! CC 7 (mapped with `IMidiMapping` to a hidden parameter) scales its
//! output; the octave's pitch bend comes in mapped to a parameter and goes
//! out as MIDI; the memo's controller is a class of its own, which says
//! hello over the connection (the processor won't start without it) and
//! marks the state dirty when the processor tells it of a new memory; and
//! the gain has an editor that draws nothing but takes a run loop timer
//! and, on its first tick, moves its knob to -42 dB and asks for 300 × 150.
//!
//! [`install`] lays the library out as a CLAP file, an LV2 bundle and a
//! VST3 bundle, for `CLAP_PATH`, `LV2_PATH` and `VST3_PATH`.
//!
//! With `DK_TEST_PLUGIN_CRASH` set, loading the CLAP entry (or the VST3
//! factory) aborts the process, to test that scans survive crashing
//! plugins.

mod clap;
mod lv2;
mod vst3;

pub use clap::STALL_DB;
pub use vst3::id as vst3_id;

use std::io;
use std::path::{Path, PathBuf};

/// Latency of `dawkey.test.delay`, in frames.
pub const DELAY_FRAMES: u32 = 64;

/// The built plugin library: `libdk_test_plugins.so` next to the running
/// test executable (Cargo builds it there when a crate depends on this one).
pub fn library() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dirs = exe.ancestors().skip(1).take(3);
    for dir in dirs {
        for candidate in [
            dir.join("libdk_test_plugins.so"),
            dir.join("deps/libdk_test_plugins.so"),
        ] {
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "libdk_test_plugins.so not found near {}; depend on dk-test-plugins to build it",
            exe.display()
        ),
    ))
}

/// Where [`install`] put the plugins.
#[derive(Debug, Clone)]
pub struct Installed {
    /// For `CLAP_PATH`: holds `dawkey-test.clap`.
    pub clap_path: PathBuf,
    /// For `LV2_PATH`: holds the bundle `dawkey-test.lv2/`.
    pub lv2_path: PathBuf,
    /// For `VST3_PATH`: holds the bundle `dawkey-test.vst3/`.
    pub vst3_path: PathBuf,
}

/// Installs the plugins under `dir`, in `dir/clap`, `dir/lv2` and
/// `dir/vst3`.
pub fn install(dir: &Path) -> io::Result<Installed> {
    let library = library()?;
    let installed = Installed {
        clap_path: dir.join("clap"),
        lv2_path: dir.join("lv2"),
        vst3_path: dir.join("vst3"),
    };
    std::fs::create_dir_all(&installed.clap_path)?;
    let clap = installed.clap_path.join("dawkey-test.clap");
    if !clap.exists() {
        std::os::unix::fs::symlink(&library, &clap)?;
    }
    let bundle = installed.lv2_path.join("dawkey-test.lv2");
    std::fs::create_dir_all(&bundle)?;
    let binary = bundle.join("libdk_test_plugins.so");
    if !binary.exists() {
        std::os::unix::fs::symlink(&library, &binary)?;
    }
    std::fs::write(bundle.join("manifest.ttl"), lv2::MANIFEST)?;
    let binaries = installed
        .vst3_path
        .join("dawkey-test.vst3/Contents")
        .join(format!("{}-linux", std::env::consts::ARCH));
    std::fs::create_dir_all(&binaries)?;
    let binary = binaries.join("dawkey-test.so");
    if !binary.exists() {
        std::os::unix::fs::symlink(&library, &binary)?;
    }
    Ok(installed)
}
