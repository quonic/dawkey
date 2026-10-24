// SPDX-License-Identifier: AGPL-3.0-or-later
//! A running Dawkey instance: the open session, its decoded audio, and the
//! engine playing it through an audio backend.
//!
//! [`Studio`] lives on one thread (see [`dawkey_core::Handle`]). Every change to
//! the project goes through it, so it can recompile the engine graph whenever
//! the session's revision moves.

#![forbid(unsafe_code)]

pub mod cache;
mod midi_takes;
mod plugins;
mod recorder;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dawkey_backend_pipewire::{
    BackendStatus, MidiCaptureRoute, MidiRoutes, PipeWireError, PipeWireOutput,
};
use dawkey_core::{
    AddClip, AddMidiClip, AutomationTarget, Command, ControlSpec, Controller, CoreError,
    MeterChange, MeterMap, NoteSpec, Outcome, PluginId, Project, RemoveClip, Session, SetTempoMap,
    SetTimeSignatureMap, SourceId, TempoChange, TimeRange, TimeSignature, TrackId, TrackKind,
};
use dawkey_engine::{
    EngineBusy, EngineController, EngineProcessor, Graph, InputFilter, MediaPool, MeterReading,
    MidiEvent, MidiRecord, PluginProcessor, RecordShared, TrackMeter, device_tag, new_engine,
    render_offline_keeping,
};
use dawkey_media::{Analysis, AudioData, DecodedFile, MidiFile, Peaks, analyze};
use dawkey_plugins::{Catalog, PluginFormat};

use crate::midi_takes::{MidiTake, Span};
use crate::plugins::Plugins;
pub use crate::plugins::{PluginProblem, default_cache as default_plugin_cache};
use crate::recorder::{Recorder, Take, Target};
pub use dawkey_plugins::{
    BridgeProgram, Catalog as PluginCatalog, ParamChange, PluginInfo, Sandbox, SearchPath,
    run_plugin_host,
};
use serde::Serialize;

pub use dawkey_backend_pipewire::Options as PipeWireOptions;

/// Block size used when advancing the virtual clock.
const VIRTUAL_BLOCK: usize = 512;
/// Longest stretch `advance` or `render` will produce at once, in seconds.
pub const MAX_RENDER_S: f64 = 600.0;
/// Longest export, in seconds (exports stream to their file).
pub const MAX_EXPORT_S: f64 = 6.0 * 3600.0;

#[derive(Debug, thiserror::Error)]
pub enum StudioError {
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error(transparent)]
    PipeWire(#[from] PipeWireError),
    #[error(transparent)]
    Engine(#[from] EngineBusy),
    #[error("{0}")]
    Invalid(String),
}

/// Which audio backend drives the engine.
#[derive(Debug, Clone)]
pub enum AudioBackend {
    /// No device. Time advances only when [`Studio::advance`] is called,
    /// which makes tests deterministic.
    Virtual,
    /// A PipeWire node.
    PipeWire(PipeWireOptions),
}

enum Backend {
    Virtual {
        processor: Box<EngineProcessor>,
        buffers: [Vec<f32>; 2],
    },
    PipeWire(PipeWireOutput),
}

/// A session with all of its audio decoded, ready to hand to a [`Studio`].
/// Building one can take a while, so do it off the studio's thread.
#[derive(Debug)]
pub struct LoadedProject {
    session: Session,
    media: MediaPool,
    peaks: HashMap<SourceId, Arc<Peaks>>,
    missing: BTreeMap<SourceId, String>,
}

impl LoadedProject {
    pub fn new(project: Project) -> Self {
        Self {
            session: Session::new(project),
            media: MediaPool::new(),
            peaks: HashMap::new(),
            missing: BTreeMap::new(),
        }
    }

    /// Opens a project bundle and decodes every source. Sources that fail to
    /// load are reported in [`EngineStatus::missing_media`], not as errors.
    pub fn open(path: &Path) -> Result<Self, CoreError> {
        Ok(Self::decode(Session::open(path)?))
    }

    /// Like [`open`](Self::open), from what a [`Recovery`] kept.
    pub fn recover(recovery: &Recovery) -> Result<Self, CoreError> {
        Ok(Self::decode(Session::recover(
            &recovery.file,
            recovery.bundle.clone(),
        )?))
    }

    /// Decodes every source of `session`.
    fn decode(session: Session) -> Self {
        let rate = session.project().sample_rate;
        let mut media = MediaPool::new();
        let mut peaks = HashMap::new();
        let mut missing = BTreeMap::new();
        for source in &session.project().sources {
            let path = session.source_path(source.id).expect("source exists");
            match dawkey_media::decode_file(&path, rate) {
                Ok(decoded) => {
                    peaks.insert(source.id, Arc::new(Peaks::compute(&decoded.audio)));
                    media.insert(source.id, Arc::new(decoded.audio));
                }
                Err(e) => {
                    tracing::warn!("source {}: {e}", source.id);
                    missing.insert(source.id, e.to_string());
                }
            }
        }
        Self {
            session,
            media,
            peaks,
            missing,
        }
    }
}

/// Unsaved changes a Dawkey that stopped without exiting cleanly kept in an
/// autosave.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Recovery {
    /// The autosave.
    pub file: PathBuf,
    /// The project's bundle; `None` if it was never saved.
    pub bundle: Option<PathBuf>,
    /// A never-saved project's cache folder (its recordings), which the
    /// Dawkey that recovers it takes over.
    pub dir: Option<PathBuf>,
    /// When the autosave was written.
    #[serde(skip)]
    pub written: std::time::SystemTime,
}

impl Recovery {
    /// How long ago the autosave was written.
    pub fn age(&self) -> Duration {
        self.written.elapsed().unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct TransportState {
    pub playing: bool,
    pub recording: bool,
    pub position_frames: u64,
    pub position_s: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MissingMedia {
    pub source: SourceId,
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EngineStatus {
    /// "virtual" or "pipewire".
    pub backend: &'static str,
    pub sample_rate: u32,
    pub transport: TransportState,
    /// Audio cycles run so far.
    pub cycles: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pipewire: Option<BackendStatus>,
    /// Sources whose audio could not be loaded; their clips are silent.
    pub missing_media: Vec<MissingMedia>,
    /// How far recordings are shifted earlier to line up with playback:
    /// the backend's estimated round trip plus `record_offset_ms`.
    pub record_latency_ms: f64,
    /// The user's correction to the latency estimate.
    pub record_offset_ms: f64,
    /// The default audio source is connected to Dawkey's inputs (an audio
    /// track is armed).
    pub capturing: bool,
    /// MIDI devices are connected to Dawkey's MIDI input, and play through
    /// the armed MIDI tracks' synths (a MIDI track is armed).
    pub midi_capturing: bool,
    /// MIDI devices (and other apps' MIDI ports, as `node:port`) that can
    /// play into a track (set_track_midi_input).
    pub midi_inputs: Vec<String>,
    /// MIDI devices (and other apps' MIDI ports) a track can send to
    /// (set_track_midi_output).
    pub midi_outputs: Vec<String>,
    /// How long those take to get MIDI, as the audio system reports it, in
    /// ms (one cycle for ALSA's MIDI bridge). Added to a track's
    /// latency_ms for delay compensation.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub midi_output_latency_ms: BTreeMap<String, f64>,
    /// How late MIDI from each input device arrives, as the audio system
    /// reports it, in ms (one cycle for ALSA's MIDI bridge).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub midi_input_latency_ms: BTreeMap<String, f64>,
    /// How far MIDI recorded from each of those is shifted earlier: what
    /// playback takes to be heard, the device's input latency (a cycle if
    /// it reports none), plugin latency and `record_offset_ms`.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub midi_record_latency_ms: BTreeMap<String, f64>,
    /// What the last recording made, if anything was lost.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_recording: Option<RecordingReport>,
    /// Frames every track (and MIDI sent to devices) is delayed by to line
    /// up plugins with latency, and MIDI devices with their latency
    /// (set_track_midi_output's latency_ms).
    pub plugin_latency_frames: u32,
    /// Plugin slots that couldn't be loaded; they pass audio through.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub plugin_problems: Vec<PluginProblem>,
}

/// A plugin slot and its parameters, as reported to clients.
#[derive(Debug, Clone, Serialize)]
pub struct PluginDetails {
    pub plugin: PluginId,
    pub track: TrackId,
    /// `clap:...`, `lv2:...` or `vst3:...`.
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub format: Option<PluginFormat>,
    pub instrument: bool,
    pub bypass: bool,
    /// Why it isn't running, if it isn't.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Has a sidechain input (set_plugin_sidechain), and the track that
    /// feeds it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub has_sidechain: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidechain: Option<TrackId>,
    /// Its other outputs (set_plugin_output), and the bus each goes to.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<OutputState>,
    /// The size of what the plugin saved of itself beyond its parameters
    /// (in bytes), if it saved anything.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_bytes: Option<usize>,
    /// The process it runs in, if it runs in one of its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_id: Option<u32>,
    /// That process is sandboxed: the plugin writes only to its own
    /// folders and can't reach the network.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub sandboxed: bool,
    /// It has a GUI of its own (show_plugin_gui), and it's showing.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub has_gui: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub gui_visible: bool,
    /// How many parameters the plugin has.
    pub param_count: usize,
    /// Index of `params[0]` (when only some are listed).
    #[serde(skip_serializing_if = "is_zero")]
    pub first_param: usize,
    pub params: Vec<ParamState>,
}

/// One of a plugin's other outputs, as reported to clients.
#[derive(Debug, Clone, Serialize)]
pub struct OutputState {
    /// Its number, for set_plugin_output.
    pub output: u32,
    pub name: String,
    pub channels: u32,
    /// The bus it goes to; `None`: nowhere (silent).
    pub bus: Option<TrackId>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Serialize)]
pub struct ParamState {
    pub id: String,
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    /// The value now: its automation's at the playhead if it has any,
    /// otherwise the one set on the slot, or the default.
    pub value: f64,
    /// The value as the plugin shows it, e.g. "-6.0 dB".
    pub text: String,
    pub stepped: bool,
    /// An automation lane drives it.
    pub automated: bool,
}

/// What an imported MIDI file became.
#[derive(Debug, Clone, Default, Serialize)]
pub struct MidiImport {
    /// The track each clip went on: one per file track (and channel) with
    /// notes, new unless it was mapped to an existing one.
    pub tracks: Vec<TrackId>,
    pub clips: Vec<dawkey_core::ClipId>,
    pub notes: usize,
    /// The file's tempo map and time signature were adopted.
    pub used_file_tempo: bool,
    /// Clips of mapped tracks that the imported clips replaced.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub removed_clips: Vec<dawkey_core::ClipId>,
}

/// Where a MIDI file's track goes when importing: onto an existing MIDI
/// track instead of a new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MidiImportTarget {
    /// Index of the track in the file (as [`MidiFile::tracks`] has them).
    pub file_track: usize,
    /// The MIDI track to put its clip on.
    pub track: TrackId,
}

/// MIDI the engine sent to a device (virtual backend).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SentMidi {
    /// Seconds from the start of the advance that sent it.
    pub at_s: f64,
    pub device: String,
    /// The MIDI 1.0 message (two-byte messages end in 0). For a MIDI 2.0
    /// per-note message, its status (below 0x80: the packet's opcode and
    /// channel), note and controller number.
    pub data: [u8; 3],
    /// A MIDI 2.0 per-note message as sent: a Universal MIDI Packet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ump: Option<[u32; 2]>,
}

/// The result of the last recording.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RecordingReport {
    /// Clips placed (one per take).
    pub clips: Vec<dawkey_core::ClipId>,
    /// Frames lost because the disk fell behind.
    pub dropped_frames: u64,
    /// MIDI messages lost (the MIDI ring overflowed).
    pub dropped_midi: u64,
}

/// A recording in progress.
struct Recording {
    /// The disk writer, if audio tracks are armed.
    audio: Option<Recorder>,
    /// Recorded MIDI, if MIDI tracks are armed.
    midi: Option<MidiCapture>,
    shared: Arc<RecordShared>,
    /// Timeline position where it started.
    start: u64,
    loop_range: Option<TimeRange>,
    punch: Option<TimeRange>,
    /// The project had never been saved, so the takes go to a cache folder.
    cached: bool,
}

struct MidiCapture {
    ring: rtrb::Consumer<MidiRecord>,
    records: Vec<MidiRecord>,
    /// Armed MIDI tracks, and what each records.
    tracks: Vec<(TrackId, InputFilter)>,
}

impl MidiCapture {
    fn drain(&mut self) {
        while let Ok(record) = self.ring.pop() {
            self.records.push(record);
        }
    }
}

/// Audio rendered offline from a snapshot of the project. `Send`, so it can
/// run on a worker thread while the studio keeps working. Its plugins are
/// fresh instances; give the [`RenderLeftovers`] back to
/// [`Studio::finish_render`] to unload them.
#[derive(Debug)]
pub struct RenderJob {
    job: u64,
    graph: Graph,
    sample_rate: u32,
    start: u64,
    frames: usize,
}

/// An export: an offline render streamed to a file a block at a time.
#[derive(Debug)]
pub struct ExportJob {
    job: u64,
    render: dawkey_engine::OfflineRender,
    sample_rate: u32,
    frames: usize,
}

/// An offline render as an export reads it.
struct Blocks<'a>(&'a mut dawkey_engine::OfflineRender);

impl dawkey_media::BlockSource for Blocks<'_> {
    fn read(&mut self, left: &mut [f32], right: &mut [f32]) -> usize {
        self.0.render(left, right)
    }
}

impl ExportJob {
    /// The project's sample rate, which the render is at.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Frames the render makes (at the project's rate).
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Renders and writes the file. Returns what was written, and the
    /// plugins to hand back to [`Studio::finish_render`] (whatever happened).
    pub fn run(
        self,
        spec: &dawkey_media::ExportSpec,
        path: &Path,
        progress: &dawkey_media::ExportProgress,
    ) -> (Result<Analysis, dawkey_media::MediaError>, RenderLeftovers) {
        let (result, leftovers) = self.run_segmented(spec, path, progress, &[]);
        (result.map(|r| r.analysis), leftovers)
    }

    /// [`run`](Self::run), also analyzing `segments` (`[start, end)` in
    /// seconds from the export's start) separately.
    pub fn run_segmented(
        mut self,
        spec: &dawkey_media::ExportSpec,
        path: &Path,
        progress: &dawkey_media::ExportProgress,
        segments: &[(f64, f64)],
    ) -> (
        Result<dawkey_media::ExportAnalysis, dawkey_media::MediaError>,
        RenderLeftovers,
    ) {
        let mut blocks = Blocks(&mut self.render);
        let result = dawkey_media::export_segmented(
            &mut blocks,
            self.sample_rate,
            spec,
            path,
            progress,
            segments,
        );
        let mut graph = self.render.into_graph();
        let leftovers = RenderLeftovers {
            job: self.job,
            processors: graph.take_plugins(),
        };
        (result, leftovers)
    }
}

/// The plugins of a finished render job, for [`Studio::finish_render`].
#[derive(Debug)]
pub struct RenderLeftovers {
    job: u64,
    processors: Vec<(u64, Box<dyn PluginProcessor>)>,
}

impl RenderJob {
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Returns stereo planar audio, and the plugins to hand back.
    pub fn run(self) -> (Vec<Vec<f32>>, RenderLeftovers) {
        let (audio, mut graph) =
            render_offline_keeping(self.graph, self.sample_rate, self.start, self.frames);
        let leftovers = RenderLeftovers {
            job: self.job,
            processors: graph.take_plugins(),
        };
        (audio, leftovers)
    }
}

pub struct Studio {
    session: Session,
    media: MediaPool,
    peaks: HashMap<SourceId, Arc<Peaks>>,
    missing: BTreeMap<SourceId, String>,
    backend_kind: AudioBackend,
    engine: EngineController,
    backend: Backend,
    /// Session revision the engine's graph was compiled from.
    synced_revision: Option<u64>,
    meters: MeterAccumulator,
    recording: Option<Recording>,
    last_recording: Option<RecordingReport>,
    /// Virtual backend only: audio fed to the inputs, and how much of it
    /// has been consumed.
    virtual_input: Option<(AudioData, usize)>,
    /// Virtual backend only: MIDI to feed to the input, at frames of the
    /// virtual clock, sorted, with the device tag it comes from and the
    /// 32-bit value of a per-note message.
    virtual_midi: Vec<(u64, [u8; 3], u32, u32)>,
    /// Virtual backend only: MIDI sent to devices during the last advance.
    sent_midi: Vec<SentMidi>,
    /// Virtual backend only: the MIDI devices to offer (inputs, outputs).
    virtual_devices: (Vec<String>, Vec<String>),
    /// Virtual backend only: the latency output devices report, in ms.
    virtual_midi_latency: BTreeMap<String, f64>,
    /// Virtual backend only: the latency input devices report, in ms.
    virtual_midi_input_latency: BTreeMap<String, f64>,
    /// The reported MIDI device latencies the graph was compiled with.
    device_latency: BTreeMap<String, f64>,
    /// What the backend was last asked to connect for MIDI.
    midi_routes: MidiRoutes,
    /// Frames the virtual clock has run.
    virtual_clock: u64,
    record_offset_ms: f64,
    capturing: bool,
    midi_capturing: bool,
    /// Overrides where recordings of unsaved projects go.
    recording_dir: Option<PathBuf>,
    /// The cache folder of a Dawkey that stopped, taken over to recover its
    /// project (its `owner` file names this process).
    adopted: Option<PathBuf>,
    /// The session revision the autosave was written at.
    autosaved_revision: Option<u64>,
    /// Takes recorded into the cache folder (see [`cache`]), deleted once
    /// the project is saved, replaced or closed.
    cached_takes: Vec<PathBuf>,
    plugins: Plugins,
    /// Latency of the playing graph's plugins, in frames.
    plugin_latency: u32,
    next_render_job: u64,
}

impl std::fmt::Debug for Studio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Studio")
            .field("session", &self.session)
            .field("backend", &self.backend_kind)
            .finish_non_exhaustive()
    }
}

impl Drop for Studio {
    /// Deletes the takes of a project that was never saved, including one
    /// being recorded.
    fn drop(&mut self) {
        if let Some(recording) = self.recording.take() {
            let _ = self.engine.stop_recording();
            self.pump();
            if let (true, Some(recorder)) = (recording.cached, recording.audio) {
                match recorder.finish(Duration::from_millis(200)) {
                    Ok(takes) => self.cached_takes.extend(takes.into_iter().map(|t| t.path)),
                    Err(e) => tracing::warn!("finishing the recording: {e}"),
                }
            }
        }
        cache::remove(std::mem::take(&mut self.cached_takes));
        // Exiting cleanly: the changes were saved or discarded.
        self.discard_autosave();
        if let Some(dir) = self.adopted.take() {
            let _ = std::fs::remove_file(dir.join(cache::OWNER));
            let _ = std::fs::remove_dir(dir);
        }
    }
}

/// How a [`Studio`] finds plugins.
#[derive(Debug, Clone, Default)]
pub struct StudioOptions {
    /// The program that scans one CLAP file in its own process (see
    /// [`Studio::set_plugin_scanner`]).
    pub plugin_scanner: Option<(PathBuf, Vec<String>)>,
    /// Scan on a worker thread (see [`Studio::set_background_scan`]).
    pub background_scan: bool,
    /// Where plugins are looked for and the list is cached (see
    /// [`Studio::set_plugin_search`]); `None` for the defaults.
    pub plugin_search: Option<(SearchPath, Option<PathBuf>)>,
    /// Runs each plugin in a process of its own, started with this program
    /// (`dawkey plugin-host`); `None` runs them in this process.
    pub plugin_bridge: Option<BridgeProgram>,
}

impl Studio {
    pub fn new(loaded: LoadedProject, backend: AudioBackend) -> Result<Self, StudioError> {
        Self::with_options(loaded, backend, StudioOptions::default())
    }

    /// Like [`new`](Self::new), with plugin options that apply before the
    /// project's plugins first load.
    pub fn with_options(
        loaded: LoadedProject,
        backend: AudioBackend,
        options: StudioOptions,
    ) -> Result<Self, StudioError> {
        let rate = loaded.session.project().sample_rate;
        let mut plugins = Plugins::new(rate);
        if let Some((search, cache)) = options.plugin_search {
            plugins.set_search(search, cache);
        }
        plugins.set_bridge(options.plugin_bridge);
        plugins.set_scanner(options.plugin_scanner);
        plugins.set_background(options.background_scan);
        let (engine, running) = start_backend(rate, &backend)?;
        let mut studio = Self {
            session: loaded.session,
            media: loaded.media,
            peaks: loaded.peaks,
            missing: loaded.missing,
            backend_kind: backend,
            engine,
            backend: running,
            synced_revision: None,
            meters: MeterAccumulator::default(),
            recording: None,
            last_recording: None,
            virtual_input: None,
            virtual_midi: Vec::new(),
            sent_midi: Vec::new(),
            virtual_devices: Default::default(),
            virtual_midi_latency: BTreeMap::new(),
            virtual_midi_input_latency: BTreeMap::new(),
            device_latency: BTreeMap::new(),
            midi_routes: MidiRoutes::default(),
            virtual_clock: 0,
            record_offset_ms: 0.0,
            capturing: false,
            midi_capturing: false,
            recording_dir: None,
            adopted: None,
            autosaved_revision: None,
            cached_takes: Vec::new(),
            plugins,
            plugin_latency: 0,
            next_render_job: 1,
        };
        studio.sync();
        Ok(studio)
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn sample_rate(&self) -> u32 {
        self.engine.sample_rate()
    }

    /// Replaces the open project (new/open). Stops the transport, and
    /// restarts the backend if the sample rate changes.
    pub fn replace(&mut self, loaded: LoadedProject) -> Result<(), StudioError> {
        // Its changes were saved or discarded.
        self.discard_autosave();
        // Keep anything being recorded in the old project.
        if self.recording.is_some()
            && let Err(e) = self.stop()
        {
            tracing::warn!("finishing the recording: {e}");
        }
        let rate = loaded.session.project().sample_rate;
        self.plugins.reset(rate);
        if rate != self.sample_rate() {
            let (engine, backend) = start_backend(rate, &self.backend_kind)?;
            self.engine = engine;
            self.backend = backend;
        } else {
            // Best effort: if the audio thread isn't running, the transport
            // isn't moving anyway.
            if let Err(e) = self.engine.stop().and_then(|()| self.engine.locate(0)) {
                tracing::warn!("resetting the transport: {e}");
            }
        }
        self.session = loaded.session;
        self.media = loaded.media;
        self.peaks = loaded.peaks;
        self.missing = loaded.missing;
        self.synced_revision = None;
        self.sync();
        // The old project's takes that were never saved go with it.
        cache::remove(std::mem::take(&mut self.cached_takes));
        Ok(())
    }

    pub fn execute(&mut self, command: Command) -> Result<Outcome, CoreError> {
        let command = self.plugins.resolve(self.session.project(), command)?;
        self.plugins.check(self.session.project(), &command)?;
        let result = self.session.execute(command);
        self.sync();
        result
    }

    /// Runs commands as one undo step until `next` makes none, each made
    /// from the project as the earlier ones left it and their outcomes; see
    /// [`Session::execute_batch_with`]. Plugin commands are checked against
    /// the catalog like single ones. The engine graph is compiled once.
    pub fn execute_batch_with<F>(
        &mut self,
        label: impl Into<String>,
        mut next: F,
    ) -> Result<Vec<Outcome>, CoreError>
    where
        F: FnMut(&Project, &[Outcome]) -> Result<Option<Command>, CoreError>,
    {
        let plugins = &mut self.plugins;
        let result = self.session.execute_batch_with(label, |project, outcomes| {
            let Some(command) = next(project, outcomes)? else {
                return Ok(None);
            };
            let name = command.name();
            let named = |e: CoreError| e.in_context(name);
            let command = plugins.resolve(project, command).map_err(named)?;
            plugins.check(project, &command).map_err(named)?;
            Ok(Some(command))
        });
        self.sync();
        result
    }

    /// Runs a command that may join the previous undo step; see
    /// [`Session::execute_merging`].
    pub fn execute_merging(
        &mut self,
        command: Command,
        merge_key: &str,
    ) -> Result<Outcome, CoreError> {
        let command = self.plugins.resolve(self.session.project(), command)?;
        self.plugins.check(self.session.project(), &command)?;
        let result = self.session.execute_merging(command, merge_key);
        self.sync();
        result
    }

    /// Runs several commands that join the same undo step (see
    /// [`execute_merging`](Self::execute_merging)), compiling the engine
    /// graph once for them all. Stops at the first that fails.
    pub fn execute_merging_all(
        &mut self,
        commands: Vec<Command>,
        merge_key: &str,
    ) -> Result<(), CoreError> {
        let mut result = Ok(());
        for command in commands {
            result = self
                .plugins
                .resolve(self.session.project(), command)
                .and_then(|command| {
                    self.plugins.check(self.session.project(), &command)?;
                    self.session.execute_merging(command, merge_key)
                })
                .map(|_| ());
            if result.is_err() {
                break;
            }
        }
        self.sync();
        result
    }

    pub fn end_merge(&mut self) {
        self.session.end_merge();
    }

    /// Waveform overview of a source, if its audio is loaded.
    pub fn peaks(&self, source: SourceId) -> Option<Arc<Peaks>> {
        self.peaks.get(&source).cloned()
    }

    pub fn undo(&mut self) -> Result<Option<String>, CoreError> {
        let result = self.session.undo();
        self.sync();
        result
    }

    pub fn redo(&mut self) -> Result<Option<String>, CoreError> {
        let result = self.session.redo();
        self.sync();
        result
    }

    /// Saves the project, with what its plugins save of themselves. Takes
    /// recorded before it was first saved are copied into it, and their
    /// cached files deleted.
    /// Where the project's autosave goes: in its bundle once it has one,
    /// with its recordings (in the cache) before.
    pub fn autosave_file(&self) -> PathBuf {
        let name = dawkey_core::project_file::AUTOSAVE_NAME;
        match self.session.path() {
            Some(bundle) => bundle
                .join(dawkey_core::project_file::BACKUP_DIR)
                .join(name),
            None => self.recording_dir().join(name),
        }
    }

    /// Writes the autosave if there are unsaved changes it doesn't have.
    /// Returns whether it wrote one.
    pub fn autosave(&mut self) -> Result<bool, CoreError> {
        let revision = self.session.revision();
        if !self.session.is_dirty() || self.autosaved_revision == Some(revision) {
            return Ok(false);
        }
        self.capture_plugin_states(false);
        // An autosave of a saved project finds them in it.
        self.keep_plugin_files();
        self.session.write_autosave(&self.autosave_file())?;
        self.autosaved_revision = Some(revision);
        Ok(true)
    }

    /// Deletes the autosave: the project was saved, or its changes
    /// discarded.
    pub fn discard_autosave(&mut self) {
        let file = self.autosave_file();
        match std::fs::remove_file(&file) {
            Ok(()) => {
                // Its folder goes if that emptied it.
                if let Some(dir) = file.parent() {
                    let _ = std::fs::remove_dir(dir);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("removing {}: {e}", file.display()),
        }
        self.autosaved_revision = None;
    }

    /// The open project's unsaved changes from before, if a Dawkey that had
    /// it open stopped without exiting cleanly: an autosave in its bundle
    /// newer than the project.
    pub fn bundle_recovery(&self) -> Option<Recovery> {
        let bundle = self.session.path()?;
        let file = self.autosave_file();
        let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        let written = modified(&file)?;
        let saved = modified(&bundle.join(dawkey_core::project_file::DOCUMENT_NAME));
        // Saving deletes the autosave, so one that's there and no older
        // than the project (file times are coarse) has changes it lacks.
        (saved.is_none_or(|saved| written >= saved)).then(|| Recovery {
            file,
            bundle: Some(bundle.to_owned()),
            dir: None,
            written,
        })
    }

    /// The newest never-saved project a Dawkey that stopped without exiting
    /// cleanly left an autosave of.
    pub fn orphan_recovery(&self) -> Option<Recovery> {
        let root = self
            .recording_dir
            .as_deref()
            .and_then(Path::parent)
            .map_or_else(cache::recordings_root, Path::to_owned);
        let (dir, written) = cache::orphaned_autosaves(&root).into_iter().next()?;
        Some(Recovery {
            file: dir.join(dawkey_core::project_file::AUTOSAVE_NAME),
            bundle: None,
            dir: Some(dir),
            written,
        })
    }

    /// Opens the project as the autosave kept it, unsaved changes and all.
    /// A never-saved project's folder (with its recordings) becomes this
    /// Dawkey's.
    pub fn recover(&mut self, recovery: &Recovery) -> Result<(), StudioError> {
        let loaded = LoadedProject::recover(recovery)?;
        self.replace(loaded)?;
        if let Some(dir) = &recovery.dir {
            if let Err(e) = cache::adopt(dir) {
                tracing::warn!("taking over {}: {e}", dir.display());
            }
            // Its takes are this project's cached ones now.
            let sources = &self.session.project().sources;
            self.cached_takes.extend(
                sources
                    .iter()
                    .filter(|s| s.path.starts_with(dir))
                    .map(|s| s.path.clone()),
            );
            self.recording_dir = Some(dir.clone());
            self.adopted = Some(dir.clone());
            self.sync();
        }
        // Kept again, from this Dawkey.
        self.autosave()?;
        Ok(())
    }

    /// Deletes what a [`Recovery`] would have recovered.
    pub fn discard_recovery(recovery: &Recovery) {
        let result = match &recovery.dir {
            Some(dir) => std::fs::remove_dir_all(dir),
            None => std::fs::remove_file(&recovery.file),
        };
        if let Err(e) = result {
            tracing::warn!("discarding {}: {e}", recovery.file.display());
        }
    }

    pub fn save(&mut self, path: Option<&Path>) -> Result<PathBuf, CoreError> {
        // Plugins' files go in (and their paths are kept relative to) the
        // project it's saved as.
        if let Some(path) = path {
            let bundle = dawkey_core::project_file::bundle_path(path);
            self.set_plugin_state_dirs(Some(&bundle));
        }
        self.capture_plugin_states(false);
        let autosave = self.autosave_file();
        let saved = self.session.save(path)?;
        self.keep_plugin_files();
        cache::remove(std::mem::take(&mut self.cached_takes));
        // Nothing unsaved to keep, here or where it was before.
        let _ = std::fs::remove_file(autosave);
        self.discard_autosave();
        Ok(saved)
    }

    /// Adds decoded audio to the pool. Not an undo step; see
    /// [`Session::add_source`].
    pub fn add_source(&mut self, path: &Path, decoded: DecodedFile) -> SourceId {
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Audio".into());
        let audio = decoded.audio;
        let id = self.session.add_source(
            name,
            path,
            audio.channel_count() as u16,
            audio.frames() as u64,
        );
        self.peaks.insert(id, Arc::new(Peaks::compute(&audio)));
        self.media.insert(id, Arc::new(audio));
        self.sync();
        id
    }

    pub fn play(&mut self) -> Result<(), StudioError> {
        self.engine.play()?;
        self.pump();
        Ok(())
    }

    /// Stops the transport. If recording, finishes the takes and places
    /// them as clips (one undo step).
    pub fn stop(&mut self) -> Result<(), StudioError> {
        let recording = self.recording.take();
        if recording.is_some() {
            self.engine.stop_recording()?;
        }
        self.engine.stop()?;
        self.pump();
        if let Some(recording) = recording {
            self.finish_recording(recording)?;
        }
        Ok(())
    }

    /// Starts recording on every armed track, and playback. Stop with
    /// [`stop`](Self::stop).
    pub fn record(&mut self) -> Result<(), StudioError> {
        if self.recording.is_some() {
            return Ok(());
        }
        let project = self.session.project();
        let targets: Vec<Target> = project
            .tracks
            .iter()
            .filter(|t| t.armed && t.kind == TrackKind::Audio)
            .map(|t| Target {
                track: t.id,
                name: t.name.clone(),
                channels: t.input,
            })
            .collect();
        let midi_tracks: Vec<(TrackId, InputFilter)> = project
            .tracks
            .iter()
            .filter(|t| t.armed && t.kind == TrackKind::Midi)
            .map(|t| {
                let filter = InputFilter::new(t.midi_input.as_deref(), t.midi_input_channel);
                (t.id, filter)
            })
            .collect();
        if targets.is_empty() && midi_tracks.is_empty() {
            return Err(StudioError::Invalid(
                "no track is armed for recording (set_track_arm)".into(),
            ));
        }
        let channels: Vec<_> = targets.iter().map(|t| t.channels).collect();
        // Twenty seconds of slack before the disk falls behind.
        let buffer = project.sample_rate as usize * 20;
        let punch = project.punch.active();
        let loop_range = project.loop_region.active();
        let (session, taps) =
            dawkey_engine::record_session(&channels, !midi_tracks.is_empty(), punch, buffer);
        let audio = if targets.is_empty() {
            None
        } else {
            Some(
                Recorder::start(
                    targets,
                    taps.audio,
                    Arc::clone(&taps.shared),
                    self.recording_dir(),
                    project.sample_rate,
                )
                .map_err(StudioError::Invalid)?,
            )
        };
        let midi = taps.midi.map(|ring| MidiCapture {
            ring,
            records: Vec::new(),
            tracks: midi_tracks,
        });
        self.engine.start_recording(session)?;
        self.engine.play()?;
        self.recording = Some(Recording {
            audio,
            midi,
            shared: taps.shared,
            start: self.engine.transport().position,
            loop_range,
            punch,
            cached: self.session.path().is_none(),
        });
        self.pump();
        Ok(())
    }

    /// Where new recordings go: the project's audio folder once saved, a
    /// cache folder before (they're copied into the bundle on save).
    fn recording_dir(&self) -> PathBuf {
        match self.session.path() {
            Some(bundle) => bundle.join(dawkey_core::project_file::AUDIO_DIR),
            None if self.recording_dir.is_some() => self.recording_dir.clone().expect("checked"),
            None => cache::own_dir(),
        }
    }

    fn finish_recording(&mut self, recording: Recording) -> Result<(), StudioError> {
        let Recording {
            audio,
            midi,
            shared,
            start,
            loop_range,
            punch,
            cached,
        } = recording;
        let takes = match audio {
            Some(recorder) => recorder
                .finish(Duration::from_millis(500))
                .map_err(StudioError::Invalid)?,
            None => {
                wait_finished(&shared, Duration::from_millis(500));
                Vec::new()
            }
        };
        let dropped = shared.dropped_frames();
        if dropped > 0 {
            tracing::warn!("{dropped} recorded frames were lost (the disk fell behind)");
        }
        let dropped_midi = shared.dropped_midi();
        if dropped_midi > 0 {
            tracing::warn!("{dropped_midi} MIDI messages were lost while recording");
        }
        let midi_takes = match midi {
            Some(mut capture) => {
                capture.drain();
                let span = Span {
                    start,
                    stop: self.engine.transport().position,
                    loop_range,
                    punch,
                };
                // Tracks listening to the same input get the same takes.
                let mut inputs: Vec<InputFilter> = Vec::new();
                for (_, input) in &capture.tracks {
                    if !inputs.contains(input) {
                        inputs.push(*input);
                    }
                }
                inputs
                    .into_iter()
                    .map(|input| {
                        let tracks = capture
                            .tracks
                            .iter()
                            .filter(|(_, i)| *i == input)
                            .map(|(t, _)| *t)
                            .collect();
                        (tracks, midi_takes::takes(&capture.records, span, input))
                    })
                    .collect()
            }
            None => Vec::new(),
        };
        if cached {
            self.cached_takes
                .extend(takes.iter().map(|t| t.path.clone()));
        }
        let clips = self.place_takes(takes, midi_takes)?;
        self.last_recording = Some(RecordingReport {
            clips,
            dropped_frames: dropped,
            dropped_midi,
        });
        Ok(())
    }

    /// Adds recorded files to the pool and puts them and the MIDI takes on
    /// their tracks, shifted earlier by the record latency (one undo step).
    /// Each is a take (`AddClip::take`): in a take lane above what it
    /// overlaps, which goes silent where it plays. On each track, takes
    /// that a later take overlaps (earlier loop passes) are placed muted.
    fn place_takes(
        &mut self,
        takes: Vec<Take>,
        midi: Vec<(Vec<TrackId>, Vec<MidiTake>)>,
    ) -> Result<Vec<dawkey_core::ClipId>, StudioError> {
        let rate = self.sample_rate();
        let latency = self.record_latency_frames();
        let midi_latency = self.midi_record_latency_frames();
        let mut commands = Vec::new();
        for (i, take) in takes.iter().enumerate() {
            if take.frames == 0 {
                continue;
            }
            let decoded = dawkey_media::decode_file(&take.path, rate)
                .map_err(|e| StudioError::Invalid(e.to_string()))?;
            let source = self.add_source(&take.path, decoded);
            let start = take.start as i64 - latency;
            let (position, offset) = if start >= 0 {
                (start as u64, 0)
            } else {
                (0, (-start) as u64)
            };
            if offset >= take.frames {
                continue;
            }
            let end = take.start + take.frames;
            let overlapped = takes[i + 1..].iter().any(|later| {
                later.track == take.track
                    && later.start < end
                    && take.start < later.start + later.frames
            });
            let project = self.session.project();
            commands.push(Command::from(AddClip {
                muted: overlapped,
                take: true,
                offset_s: project.frames_to_seconds(offset),
                ..AddClip::new(
                    source,
                    Some(take.track),
                    project.frames_to_seconds(position),
                )
            }));
        }
        let project = self.session.project();
        for (tracks, takes) in &midi {
            for (i, take) in takes.iter().enumerate() {
                let overlapped = takes[i + 1..]
                    .iter()
                    .any(|later| later.start < take.end && take.start < later.end);
                let Some(clip) = midi_clip(project, take, &midi_latency, overlapped) else {
                    continue;
                };
                commands.extend(tracks.iter().map(|&track| {
                    Command::from(AddMidiClip {
                        track: Some(track),
                        ..clip.clone()
                    })
                }));
            }
        }
        if commands.is_empty() {
            return Ok(Vec::new());
        }
        let outcomes = self.session.execute_batch("record", commands)?;
        self.sync();
        Ok(outcomes.into_iter().filter_map(|o| o.clip).collect())
    }

    /// Places a MIDI file's tracks on new MIDI tracks starting at
    /// `position_s` (one undo step). With `use_file_tempo` (by default when
    /// the project has no clips yet), the project takes the file's tempo
    /// map and time signature, so the notes play as written; otherwise they
    /// keep their beat positions at the project's tempo.
    pub fn import_midi(
        &mut self,
        file: &MidiFile,
        position_s: f64,
        use_file_tempo: Option<bool>,
        targets: &[MidiImportTarget],
        replace: bool,
    ) -> Result<MidiImport, StudioError> {
        if file.tracks.is_empty() {
            return Err(StudioError::Invalid("the MIDI file has no notes".into()));
        }
        let project = self.session.project();
        for target in targets {
            if target.file_track >= file.tracks.len() {
                return Err(StudioError::Invalid(format!(
                    "the file has {} tracks with notes, so there's no file track {}",
                    file.tracks.len(),
                    target.file_track
                )));
            }
            match project.track(target.track) {
                None => return Err(CoreError::TrackNotFound(target.track).into()),
                Some(t) if t.kind != TrackKind::Midi => {
                    return Err(StudioError::Invalid(format!(
                        "track {} ({}) is not a MIDI track",
                        target.track, t.name
                    )));
                }
                Some(_) => {}
            }
            if targets
                .iter()
                .filter(|t| t.file_track == target.file_track)
                .count()
                > 1
            {
                return Err(StudioError::Invalid(format!(
                    "file track {} is mapped more than once",
                    target.file_track
                )));
            }
        }
        let empty = project.tracks.iter().all(|t| t.clips.is_empty());
        let adopt = use_file_tempo.unwrap_or(empty);

        /// A step of the import: a command, or working out which clips of a
        /// mapped track the clip just added replaces.
        enum Step {
            Command(Command),
            Replace { track: TrackId, added: usize },
        }
        let mut steps = VecDeque::new();
        let mut command_count = 0;
        let mut push = |steps: &mut VecDeque<Step>, command: Command| {
            steps.push_back(Step::Command(command));
            command_count += 1;
            command_count - 1
        };
        if adopt {
            // The file's beat 0 lands at `position_s`, at its first tempo.
            let first = file
                .tempo
                .first()
                .filter(|(beat, _)| *beat <= 0.0)
                .map_or(120.0, |&(_, bpm)| bpm);
            let offset = position_s * first / 60.0;
            push(
                &mut steps,
                Command::from(SetTempoMap {
                    bpm: first,
                    changes: file
                        .tempo
                        .iter()
                        .filter(|(beat, _)| *beat > 0.0)
                        .map(|&(beat, bpm)| TempoChange::new(beat + offset, bpm))
                        .collect(),
                }),
            );
            push(&mut steps, meter_map_of(file, offset).into());
        }
        for (index, track) in file.tracks.iter().enumerate() {
            let notes = track
                .notes
                .iter()
                .map(|n| NoteSpec {
                    pitch: n.pitch.min(127),
                    start_beats: n.start_beats,
                    length_beats: n.length_beats,
                    velocity: n.velocity.clamp(1, 127),
                })
                .collect();
            let controls = track
                .controls
                .iter()
                .filter_map(|c| {
                    let (controller, value) = Controller::from_message(c.data)?;
                    Some(ControlSpec {
                        controller,
                        time_beats: c.beat,
                        value,
                    })
                })
                .collect();
            let target = targets.iter().find(|t| t.file_track == index);
            let added = push(
                &mut steps,
                AddMidiClip {
                    track: target.map(|t| t.track),
                    track_name: target.is_none().then(|| track.name.clone()),
                    position_s,
                    notes,
                    controls,
                    ..Default::default()
                }
                .into(),
            );
            if let Some(target) = target
                && replace
            {
                steps.push_back(Step::Replace {
                    track: target.track,
                    added,
                });
            }
        }
        let mut removed = Vec::new();
        let outcomes = self.execute_batch_with("import_midi", |project, outcomes| {
            loop {
                match steps.pop_front() {
                    None => return Ok(None),
                    Some(Step::Command(command)) => return Ok(Some(command)),
                    Some(Step::Replace { track, added }) => {
                        // The clips the new one overlaps go, in front of the
                        // remaining steps.
                        let new = outcomes[added].clip.expect("add_midi_clip makes a clip");
                        let Some((_, clip)) = project.clip(new) else {
                            continue;
                        };
                        let (start, end) = (clip.start, clip.end());
                        let Some(track) = project.track(track) else {
                            continue;
                        };
                        for old in track
                            .clips
                            .iter()
                            .filter(|c| c.id != new && c.start < end && c.end() > start)
                            .rev()
                        {
                            removed.push(old.id);
                            steps.push_front(Step::Command(RemoveClip { clip: old.id }.into()));
                        }
                    }
                }
            }
        })?;
        let mut import = MidiImport {
            used_file_tempo: adopt,
            removed_clips: removed,
            ..MidiImport::default()
        };
        for outcome in outcomes {
            if let Some(clip) = outcome.clip {
                import.clips.push(clip);
                import.tracks.extend(outcome.track);
                import.notes += outcome.notes.len();
            }
        }
        Ok(import)
    }

    /// Records unsaved projects into `dir` instead of the cache folder.
    pub fn set_recording_dir(&mut self, dir: PathBuf) {
        self.recording_dir = Some(dir);
    }

    /// Where the current recording started, in seconds.
    pub fn record_start_s(&self) -> Option<f64> {
        self.recording
            .as_ref()
            .map(|r| self.session.project().frames_to_seconds(r.start))
    }

    /// Round-trip latency to compensate, in frames.
    fn record_latency_frames(&self) -> i64 {
        let estimate = match &self.backend {
            Backend::Virtual { .. } => 0,
            Backend::PipeWire(output) => i64::from(output.round_trip_estimate()),
        };
        estimate + self.record_correction_frames()
    }

    /// [`midi_record_latency_frames`](Self::midi_record_latency_frames)
    /// for each input device, in ms.
    fn midi_record_latency_ms(&self) -> BTreeMap<String, f64> {
        let latency = self.midi_record_latency_frames();
        let rate = f64::from(self.sample_rate());
        self.midi_devices()
            .0
            .into_iter()
            .map(|device| {
                let ms = latency(device_tag(&device)) as f64 * 1000.0 / rate;
                (device, ms)
            })
            .collect()
    }

    /// Plugin latency and the user's offset, which recordings of both
    /// kinds are shifted by, in frames.
    fn record_correction_frames(&self) -> i64 {
        i64::from(self.plugin_latency) + self.ms_to_frames(self.record_offset_ms)
    }

    fn ms_to_frames(&self, ms: f64) -> i64 {
        (ms * f64::from(self.sample_rate()) / 1000.0).round() as i64
    }

    /// The latency to compensate for MIDI recorded from each device (by
    /// [`device_tag`]), in frames: what playback takes to be heard plus
    /// what the device's input reports, or, if it reports nothing, the
    /// audio round-trip estimate (a cycle in).
    fn midi_record_latency_frames(&self) -> impl Fn(u32) -> i64 + use<> {
        let (playback, fallback) = match &self.backend {
            Backend::Virtual { .. } => (0, 0),
            Backend::PipeWire(output) => (
                i64::from(output.playback_latency()),
                i64::from(output.quantum()),
            ),
        };
        let correction = self.record_correction_frames();
        let reported: HashMap<u32, i64> = self
            .reported_midi_input_latency()
            .iter()
            .map(|(device, &ms)| (device_tag(device), self.ms_to_frames(ms)))
            .collect();
        move |source| playback + correction + reported.get(&source).copied().unwrap_or(fallback)
    }

    /// Adjusts the latency compensation (positive shifts recordings
    /// earlier), in milliseconds.
    pub fn set_record_offset_ms(&mut self, ms: f64) -> Result<(), StudioError> {
        if !(-1000.0..=1000.0).contains(&ms) {
            return Err(StudioError::Invalid(format!(
                "offset must be between -1000 and 1000 ms, got {ms}"
            )));
        }
        self.record_offset_ms = ms;
        Ok(())
    }

    /// Virtual backend only: feeds `audio` to the inputs from now on (then
    /// silence once it runs out). `None` feeds silence.
    pub fn set_virtual_input(&mut self, audio: Option<AudioData>) -> Result<(), StudioError> {
        if !matches!(self.backend, Backend::Virtual { .. }) {
            return Err(StudioError::Invalid(
                "input can only be injected with the virtual backend".into(),
            ));
        }
        self.virtual_input = audio.map(|a| (a, 0));
        Ok(())
    }

    /// Virtual backend only: plays MIDI into the input, as if a keyboard
    /// (named `device`, or unnamed) were connected. Each message is
    /// `(seconds from now, bytes, 32-bit value)` (the value only for MIDI
    /// 2.0 per-note messages, see [`dawkey_engine::MidiEvent`]); they arrive as
    /// later [`advance`](Self::advance) calls reach them.
    pub fn queue_virtual_midi(
        &mut self,
        device: Option<&str>,
        events: &[(f64, [u8; 3], u32)],
    ) -> Result<(), StudioError> {
        let source = device.map_or(0, device_tag);
        if !matches!(self.backend, Backend::Virtual { .. }) {
            return Err(StudioError::Invalid(
                "MIDI can only be injected with the virtual backend".into(),
            ));
        }
        let rate = f64::from(self.sample_rate());
        for &(at_s, data, value) in events {
            if !(0.0..=MAX_RENDER_S).contains(&at_s) {
                return Err(StudioError::Invalid(format!(
                    "MIDI times must be between 0 and {MAX_RENDER_S} s, got {at_s}"
                )));
            }
            let at = self.virtual_clock + (at_s * rate).round() as u64;
            // After anything already queued for the same frame.
            let index = self.virtual_midi.partition_point(|&(t, ..)| t <= at);
            self.virtual_midi.insert(index, (at, data, source, value));
        }
        Ok(())
    }

    pub fn locate(&mut self, seconds: f64) -> Result<(), StudioError> {
        if !(0.0..=86_400.0).contains(&seconds) {
            return Err(StudioError::Invalid(format!(
                "position_s must be between 0 and 86400, got {seconds}"
            )));
        }
        let frame = self.session.project().seconds_to_frames(seconds);
        self.engine.locate(frame)?;
        self.pump();
        Ok(())
    }

    /// How many audio cycles the engine has run. A transport command has
    /// been applied once this has grown by two.
    pub fn engine_cycles(&self) -> u64 {
        self.engine.transport().cycles
    }

    pub fn transport(&self) -> TransportState {
        let t = self.engine.transport();
        TransportState {
            playing: t.playing,
            recording: t.recording,
            position_frames: t.position,
            position_s: self.session.project().frames_to_seconds(t.position),
        }
    }

    /// Peak levels since the last call.
    pub fn take_meters(&mut self) -> MeterReading {
        self.drain_meters();
        self.meters.take_since_last_call()
    }

    /// Peak-hold levels for display (linear, 0..=1 of full scale), decaying
    /// at 20 dB/s. Independent of [`take_meters`](Self::take_meters).
    pub fn display_meters(&mut self) -> DisplayMeters {
        self.drain_meters();
        self.meters.display(Instant::now())
    }

    fn drain_meters(&mut self) {
        let reading = self.engine.take_meters();
        self.meters.add(&reading);
    }

    pub fn engine_status(&self) -> EngineStatus {
        let (backend, pipewire) = match &self.backend {
            Backend::Virtual { .. } => ("virtual", None),
            Backend::PipeWire(output) => ("pipewire", Some(output.status())),
        };
        EngineStatus {
            backend,
            sample_rate: self.sample_rate(),
            transport: self.transport(),
            cycles: self.engine.transport().cycles,
            pipewire,
            missing_media: self
                .missing
                .iter()
                .map(|(&source, error)| MissingMedia {
                    source,
                    error: error.clone(),
                })
                .collect(),
            record_latency_ms: self.record_latency_frames() as f64 * 1000.0
                / f64::from(self.sample_rate()),
            record_offset_ms: self.record_offset_ms,
            capturing: self.capturing,
            midi_capturing: self.midi_capturing,
            midi_inputs: self.midi_devices().0,
            midi_outputs: self.midi_devices().1,
            midi_output_latency_ms: self.reported_midi_latency(),
            midi_input_latency_ms: self.reported_midi_input_latency(),
            midi_record_latency_ms: self.midi_record_latency_ms(),
            last_recording: self.last_recording.clone(),
            plugin_latency_frames: self.plugin_latency,
            plugin_problems: self.plugins.problems(),
        }
    }

    /// Prepares an offline render of `[start_s, end_s)` of the current
    /// project. Uses the session's mute/solo state; ignores the transport.
    pub fn render_job(&mut self, start_s: f64, end_s: f64) -> Result<RenderJob, StudioError> {
        self.render_job_solo(start_s, end_s, None)
    }

    /// [`render_job`](Self::render_job) with `solo` tracks soloed (and no
    /// others), as if their solo buttons were on: only they are heard,
    /// through their buses and the master.
    pub fn render_job_solo(
        &mut self,
        start_s: f64,
        end_s: f64,
        solo: Option<&[TrackId]>,
    ) -> Result<RenderJob, StudioError> {
        let (job, graph, start, frames) = self.offline_graph(start_s, end_s, MAX_RENDER_S, solo)?;
        Ok(RenderJob {
            job,
            graph,
            sample_rate: self.session.project().sample_rate,
            start,
            frames,
        })
    }

    /// Prepares an export of `[start_s, end_s)`, like
    /// [`render_job`](Self::render_job) but rendered a block at a time.
    pub fn export_job(&mut self, start_s: f64, end_s: f64) -> Result<ExportJob, StudioError> {
        self.export_job_solo(start_s, end_s, None)
    }

    /// [`export_job`](Self::export_job) with `solo` tracks soloed, like
    /// [`render_job_solo`](Self::render_job_solo).
    pub fn export_job_solo(
        &mut self,
        start_s: f64,
        end_s: f64,
        solo: Option<&[TrackId]>,
    ) -> Result<ExportJob, StudioError> {
        let (job, graph, start, frames) = self.offline_graph(start_s, end_s, MAX_EXPORT_S, solo)?;
        let sample_rate = self.session.project().sample_rate;
        Ok(ExportJob {
            job,
            render: dawkey_engine::OfflineRender::new(graph, sample_rate, start, frames),
            sample_rate,
            frames,
        })
    }

    /// The tracks a stem render covers: `tracks` if given (checked), else
    /// every audio and MIDI track that isn't muted. Each with its name.
    pub fn stem_tracks(
        &self,
        tracks: Option<&[TrackId]>,
    ) -> Result<Vec<(TrackId, String)>, StudioError> {
        let project = self.session.project();
        match tracks {
            Some(ids) => {
                self.check_solo(ids)?;
                Ok(ids
                    .iter()
                    .filter_map(|&id| project.track(id).map(|t| (id, t.name.clone())))
                    .collect())
            }
            None => Ok(project
                .tracks
                .iter()
                .filter(|t| t.kind != TrackKind::Bus && !t.mute)
                .map(|t| (t.id, t.name.clone()))
                .collect()),
        }
    }

    /// Checks tracks to solo for a render: they exist, and aren't the
    /// master (which is always heard).
    fn check_solo(&self, solo: &[TrackId]) -> Result<(), StudioError> {
        let project = self.session.project();
        if solo.is_empty() {
            return Err(StudioError::Invalid(
                "tracks is empty; leave it out to render everything".into(),
            ));
        }
        for &id in solo {
            if id == dawkey_core::MASTER {
                return Err(StudioError::Invalid(
                    "the master (track 0) is always rendered; give the tracks to hear".into(),
                ));
            }
            if project.track(id).is_none() {
                return Err(CoreError::TrackNotFound(id).into());
            }
        }
        Ok(())
    }

    /// A graph with fresh plugin instances for rendering `[start_s, end_s)`
    /// (at most `max_s` long), with `solo` tracks soloed if given: its job
    /// number, start frame and length.
    fn offline_graph(
        &mut self,
        start_s: f64,
        end_s: f64,
        max_s: f64,
        solo: Option<&[TrackId]>,
    ) -> Result<(u64, Graph, u64, usize), StudioError> {
        let project = self.session.project();
        if !(start_s >= 0.0 && end_s > start_s && end_s - start_s <= max_s) {
            return Err(StudioError::Invalid(format!(
                "need 0 <= start_s < end_s and at most {max_s} s, got {start_s}..{end_s}"
            )));
        }
        if let Some(solo) = solo {
            self.check_solo(solo)?;
        }
        let start = project.seconds_to_frames(start_s);
        let frames = (project.seconds_to_frames(end_s) - start) as usize;
        // The fresh instances start where the running ones are.
        self.capture_plugin_states(false);
        let project = self.session.project();
        let job = self.next_render_job;
        self.next_render_job += 1;
        let mut provider = self.plugins.offline_provider(job, &project.plugin_states);
        let graph = match solo {
            Some(solo) => {
                let mut soloed = project.clone();
                for track in &mut soloed.tracks {
                    track.solo = solo.contains(&track.id);
                }
                Graph::compile_with_devices(
                    &soloed,
                    &self.media,
                    &mut provider,
                    &self.device_latency,
                )
            }
            None => Graph::compile_with_devices(
                project,
                &self.media,
                &mut provider,
                &self.device_latency,
            ),
        };
        Ok((job, graph, start, frames))
    }

    /// Unloads the plugins of a finished render.
    pub fn finish_render(&mut self, leftovers: RenderLeftovers) {
        self.plugins
            .finish_offline(leftovers.job, leftovers.processors);
    }

    /// The installed plugins (scanned on first use; cached). Waits for a
    /// scan that is running.
    pub fn plugin_catalog(&mut self) -> &Catalog {
        self.plugins.catalog()
    }

    /// The installed plugins without waiting: `None` while the first scan
    /// runs (see [`set_background_scan`](Self::set_background_scan)).
    pub fn plugin_catalog_now(&mut self) -> Option<&Catalog> {
        self.plugins.catalog_now()
    }

    /// Whether plugins are being scanned on a worker thread.
    pub fn scanning_plugins(&self) -> bool {
        self.plugins.scanning()
    }

    /// Scans plugins on a worker thread instead of waiting for them, so the
    /// thread that runs the studio (the window's) keeps going. Slots load
    /// when the scan is done; until then they pass their signal through.
    /// Off by default, so headless instances and tests stay deterministic.
    pub fn set_background_scan(&mut self, background: bool) {
        self.plugins.set_background(background);
    }

    /// Like [`rescan_plugins`](Self::rescan_plugins) on a worker thread:
    /// the current list stays until the scan is done.
    pub fn rescan_plugins_in_background(&mut self) {
        self.plugins.rescan_in_background();
        if !self.plugins.scanning() {
            // Scanned already: slots that failed may load now.
            self.synced_revision = None;
            self.sync();
        }
    }

    /// The plugin catalog, if it has been loaded (see
    /// [`plugin_catalog`](Self::plugin_catalog)).
    pub fn cached_catalog(&self) -> Option<&Catalog> {
        self.plugins.cached()
    }

    /// Scans for plugins again.
    pub fn rescan_plugins(&mut self) -> &Catalog {
        self.plugins.rescan();
        // Slots that failed may load now.
        self.synced_revision = None;
        self.sync();
        self.plugins.catalog()
    }

    /// Where plugins are looked for, and where the list is cached (`None`:
    /// not cached). Defaults to `$CLAP_PATH`/`$LV2_PATH`/`$VST3_PATH` or the standard
    /// locations, cached in `$XDG_CACHE_HOME/dawkey/plugins.json`.
    pub fn set_plugin_search(&mut self, search: SearchPath, cache: Option<PathBuf>) {
        self.plugins.set_search(search, cache);
    }

    /// Runs plugins loaded from now on in processes of their own, started
    /// with `program` (`None`: in this process).
    pub fn set_plugin_bridge(&mut self, program: Option<BridgeProgram>) {
        self.plugins.set_bridge(program);
    }

    /// Shows slot `plugin`'s plugin's own GUI in a window, or hides it
    /// (plugins that run in processes of their own).
    pub fn show_plugin_gui(&mut self, plugin: PluginId, show: bool) -> Result<(), StudioError> {
        Ok(self.plugins.show_gui(plugin, show)?)
    }

    /// Parameters plugins changed themselves (from their GUIs) since the
    /// last call, by slot. The caller records them in the project.
    pub fn take_plugin_param_changes(&mut self) -> Vec<(PluginId, ParamChange)> {
        self.plugins.take_param_changes()
    }

    /// The ID of slot `plugin`'s parameter `index`.
    pub fn plugin_param_id(&mut self, plugin: PluginId, index: usize) -> Option<String> {
        let (_, slot) = self.session.project().plugin(plugin)?;
        let id = slot.plugin.clone();
        Some(self.plugins.info(&id)?.params.get(index)?.id.clone())
    }

    /// Loads slot `plugin`'s plugin again (one that crashed, or failed to
    /// load), from its saved state.
    pub fn reload_plugin(&mut self, plugin: PluginId) -> Result<(), StudioError> {
        if self.session.project().plugin(plugin).is_none() {
            return Err(StudioError::Invalid(format!(
                "plugin {plugin} does not exist"
            )));
        }
        self.plugins.reload(plugin);
        self.synced_revision = None;
        self.sync();
        Ok(())
    }

    /// Looks for plugins on `search` from now on (keeping the cache), and
    /// scans it in the background.
    pub fn change_plugin_search(&mut self, search: SearchPath) {
        self.plugins.change_search(search);
        self.rescan_plugins_in_background();
    }

    /// The program that scans one CLAP file in its own process (the binary
    /// with `scan-plugin`), so a crashing plugin can't take Dawkey down.
    /// Without one, plugins are scanned in-process.
    pub fn set_plugin_scanner(&mut self, program: PathBuf, args: Vec<String>) {
        self.plugins.set_scanner(Some((program, args)));
    }

    /// A plugin slot with its parameters' current values.
    pub fn plugin_details(&mut self, id: PluginId) -> Option<PluginDetails> {
        self.plugin_details_window(id, 0, usize::MAX)
    }

    /// One parameter of a plugin slot, by ID.
    pub fn plugin_param(&mut self, id: PluginId, param: &str) -> Option<ParamState> {
        let (_, slot) = self.session.project().plugin(id)?;
        let index = self
            .plugins
            .info(&slot.plugin.clone())?
            .param_index(param)?;
        self.plugin_details_window(id, index, 1)?.params.pop()
    }

    /// Like [`plugin_details`](Self::plugin_details), with only `count`
    /// parameters from `first` (plugins can have thousands).
    pub fn plugin_details_window(
        &mut self,
        id: PluginId,
        first: usize,
        count: usize,
    ) -> Option<PluginDetails> {
        let (track, slot) = self.session.project().plugin(id)?;
        let (track_id, slot) = (track.id, slot.clone());
        // Automated parameters, and their value at the playhead.
        let now = self.engine.transport().position;
        let automated: Vec<(String, Option<f64>)> = track
            .automation
            .iter()
            .filter_map(|l| match &l.target {
                AutomationTarget::Param { plugin, param } if *plugin == id => {
                    Some((param.clone(), l.value_at(now)))
                }
                _ => None,
            })
            .collect();
        let error = self
            .plugins
            .problems()
            .into_iter()
            .find(|p| p.plugin == id)
            .map(|p| p.error);
        let info = self.plugins.info(&slot.plugin).cloned();
        let params = info
            .as_ref()
            .map(|i| {
                i.params
                    .iter()
                    .enumerate()
                    .skip(first)
                    .take(count)
                    .map(|(index, p)| {
                        let lane = automated.iter().find(|(param, _)| *param == p.id);
                        let value = lane
                            .and_then(|(_, v)| *v)
                            .or_else(|| slot.params.get(&p.id).copied())
                            .unwrap_or(p.default);
                        ParamState {
                            id: p.id.clone(),
                            name: p.name.clone(),
                            min: p.min,
                            max: p.max,
                            default: p.default,
                            value,
                            text: self.plugins.param_text(id, i, index, value),
                            stepped: p.stepped,
                            automated: lane.is_some(),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(PluginDetails {
            plugin: id,
            track: track_id,
            id: slot.plugin.clone(),
            name: info
                .as_ref()
                .map_or_else(|| slot.plugin.clone(), |i| i.name.clone()),
            vendor: info.as_ref().map(|i| i.vendor.clone()).unwrap_or_default(),
            format: info.as_ref().map(|i| i.format),
            instrument: info.as_ref().is_some_and(|i| i.instrument),
            bypass: slot.bypass,
            error: error.or_else(|| info.is_none().then(|| "not installed".to_owned())),
            has_sidechain: info.as_ref().is_some_and(|i| i.sidechain),
            sidechain: slot.sidechain,
            outputs: info
                .as_ref()
                .map(|i| {
                    i.outputs
                        .iter()
                        .enumerate()
                        .map(|(n, o)| OutputState {
                            output: n as u32,
                            name: o.name.clone(),
                            channels: o.channels,
                            bus: slot.outputs.get(&(n as u32)).copied(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            state_bytes: self
                .session
                .project()
                .plugin_states
                .get(&id)
                .map(|s| s.bytes().len()),
            process_id: self.plugins.process_id(id),
            sandboxed: self.plugins.sandboxed(id),
            has_gui: self.plugins.gui(id).0,
            gui_visible: self.plugins.gui(id).1,
            param_count: info.as_ref().map_or(0, |i| i.params.len()),
            first_param: first,
            params,
        })
    }

    /// Virtual backend only: runs the engine for `seconds` of audio, as if
    /// the device had played it, and returns an analysis of what it output.
    pub fn advance(&mut self, seconds: f64) -> Result<Analysis, StudioError> {
        if !(seconds > 0.0 && seconds <= MAX_RENDER_S) {
            return Err(StudioError::Invalid(format!(
                "seconds must be > 0 and at most {MAX_RENDER_S}, got {seconds}"
            )));
        }
        let rate = self.sample_rate();
        let frames = self.session.project().seconds_to_frames(seconds).max(1) as usize;
        let Backend::Virtual { processor, buffers } = &mut self.backend else {
            return Err(StudioError::Invalid(
                "the clock only advances by hand with the virtual backend; \
                 this instance plays through PipeWire"
                    .into(),
            ));
        };
        for buffer in buffers.iter_mut() {
            buffer.clear();
            buffer.resize(frames, 0.0);
        }
        // The injected input for this stretch, zero-padded.
        let mut input = [vec![0.0; frames], vec![0.0; frames]];
        if let Some((audio, cursor)) = &mut self.virtual_input {
            let available = audio.frames().saturating_sub(*cursor).min(frames);
            let right = if audio.channel_count() > 1 { 1 } else { 0 };
            input[0][..available].copy_from_slice(&audio.channel(0)[*cursor..*cursor + available]);
            input[1][..available]
                .copy_from_slice(&audio.channel(right)[*cursor..*cursor + available]);
            *cursor += frames;
        }
        let [left, right] = buffers;
        let chunks = left
            .chunks_mut(VIRTUAL_BLOCK)
            .zip(right.chunks_mut(VIRTUAL_BLOCK))
            .zip(
                input[0]
                    .chunks(VIRTUAL_BLOCK)
                    .zip(input[1].chunks(VIRTUAL_BLOCK)),
            );
        let mut midi = Vec::new();
        // Device names by tag, to report what was sent where.
        let devices: HashMap<u32, String> = self
            .session
            .project()
            .tracks
            .iter()
            .filter_map(|t| t.midi_output.as_ref())
            .map(|o| (device_tag(&o.device), o.device.clone()))
            .collect();
        let started = self.virtual_clock;
        self.sent_midi.clear();
        for ((l, r), (in_l, in_r)) in chunks {
            let (from, to) = (self.virtual_clock, self.virtual_clock + l.len() as u64);
            let due = self.virtual_midi.partition_point(|&(t, ..)| t < to);
            midi.clear();
            midi.extend(
                self.virtual_midi
                    .drain(..due)
                    .map(|(t, data, source, value)| MidiEvent {
                        offset: (t.max(from) - from) as u32,
                        data,
                        source,
                        value,
                    }),
            );
            processor.process_with_input([in_l, in_r], &midi, l, r);
            for sent in processor.midi_output() {
                let frame = from - started + u64::from(sent.offset);
                self.sent_midi.push(SentMidi {
                    at_s: frame as f64 / f64::from(rate),
                    device: devices.get(&sent.dest).cloned().unwrap_or_default(),
                    data: sent.data,
                    ump: sent.ump(),
                });
            }
            processor.clear_midi_output();
            self.virtual_clock = to;
        }
        self.engine.collect_garbage();
        if let Some(capture) = self.recording.as_mut().and_then(|r| r.midi.as_mut()) {
            capture.drain();
        }
        Ok(analyze(&buffers[..], rate))
    }

    /// The MIDI devices tracks can take input from, and send to.
    pub fn midi_devices(&self) -> (Vec<String>, Vec<String>) {
        match &self.backend {
            Backend::Virtual { .. } => self.virtual_devices.clone(),
            Backend::PipeWire(output) => {
                let status = output.status();
                (status.midi_inputs, status.midi_outputs)
            }
        }
    }

    /// Virtual backend only: pretends these MIDI devices are connected (for
    /// tests of the device choices).
    pub fn set_virtual_midi_devices(&mut self, inputs: Vec<String>, outputs: Vec<String>) {
        self.virtual_devices = (inputs, outputs);
    }

    /// How long each MIDI output device takes to get MIDI, as the backend
    /// reports it, in ms.
    pub fn reported_midi_latency(&self) -> BTreeMap<String, f64> {
        match &self.backend {
            Backend::Virtual { .. } => self.virtual_midi_latency.clone(),
            Backend::PipeWire(output) => output.midi_output_latency_ms(),
        }
    }

    /// How late MIDI from each input device arrives, as the backend reports
    /// it, in ms.
    pub fn reported_midi_input_latency(&self) -> BTreeMap<String, f64> {
        match &self.backend {
            Backend::Virtual { .. } => self.virtual_midi_input_latency.clone(),
            Backend::PipeWire(output) => output.midi_input_latency_ms(),
        }
    }

    /// Virtual backend only: pretends MIDI input `device` reports this
    /// latency (`None`: none), as PipeWire's ports do. Recorded MIDI from it
    /// is placed that much earlier.
    pub fn set_virtual_midi_input_latency(&mut self, device: &str, ms: Option<f64>) {
        match ms {
            Some(ms) => self
                .virtual_midi_input_latency
                .insert(device.to_owned(), ms),
            None => self.virtual_midi_input_latency.remove(device),
        };
    }

    /// Virtual backend only: pretends MIDI output `device` reports this
    /// latency (`None`: none), as PipeWire's ports do.
    pub fn set_virtual_midi_latency(&mut self, device: &str, ms: Option<f64>) {
        match ms {
            Some(ms) => self.virtual_midi_latency.insert(device.to_owned(), ms),
            None => self.virtual_midi_latency.remove(device),
        };
        self.sync();
    }

    /// Virtual backend only: the MIDI the engine sent to devices during the
    /// last [`advance`](Self::advance), with times from its start.
    pub fn sent_midi(&self) -> &[SentMidi] {
        &self.sent_midi
    }

    /// Housekeeping to call periodically (every few tens of milliseconds)
    /// from the studio's thread: frees retired graphs and delivers graph
    /// updates that were deferred.
    pub fn tick(&mut self) {
        if let Some(capture) = self.recording.as_mut().and_then(|r| r.midi.as_mut()) {
            capture.drain();
        }
        if self.plugins.idle() {
            // A plugin's latency changed: delay compensation follows.
            self.synced_revision = None;
            self.sync();
        }
        // Plugins that said their state changed: the project has unsaved
        // changes.
        self.capture_plugin_states(true);
        if self.plugins.poll() {
            // Slots waiting for the scan load now.
            self.synced_revision = None;
        }
        self.sync();
    }

    /// Keeps what the running plugins save of themselves in the session
    /// (every one, or only those that said it changed).
    fn capture_plugin_states(&mut self, dirty_only: bool) {
        for (id, state) in self.plugins.capture(dirty_only) {
            self.session.set_plugin_state(id, Some(state));
        }
    }

    /// Where plugins keep files with their state: in the recordings cache
    /// while Dawkey runs (where sandboxed plugins may write, wherever the
    /// project is), copied into the project (`bundle`, once it's saved)
    /// when saving; their paths are kept relative to the project.
    fn set_plugin_state_dirs(&mut self, bundle: Option<&Path>) {
        // Plugins are given absolute paths.
        let absolute = |p: &Path| std::path::absolute(p).unwrap_or_else(|_| p.to_owned());
        let base = bundle.map(absolute);
        let files = absolute(&self.plugin_files_dir());
        self.plugins.set_state_dirs(base, files);
    }

    /// Where plugins' own files are while Dawkey runs.
    fn plugin_files_dir(&self) -> PathBuf {
        self.recording_dir
            .clone()
            .unwrap_or_else(cache::own_dir)
            .join(dawkey_core::project_file::PLUGIN_DIR)
    }

    /// Copies the files plugins made into the project's `plugins` folder.
    fn keep_plugin_files(&self) {
        let Some(bundle) = self.session.path() else {
            return;
        };
        let to = bundle.join(dawkey_core::project_file::PLUGIN_DIR);
        if let Err(e) = copy_tree(&self.plugin_files_dir(), &to) {
            tracing::warn!("copying plugins' files into {}: {e}", to.display());
        }
    }

    /// Recompiles the engine graph if the session changed. At most one graph
    /// is in flight at a time; later changes wait for [`tick`](Self::tick)
    /// or the next change, so a stalled audio thread can't fill the queue.
    fn sync(&mut self) {
        let bundle = self.session.path().map(Path::to_owned);
        self.set_plugin_state_dirs(bundle.as_deref());
        // Bridged plugins start a block late (their delay grows with longer
        // blocks by itself).
        let block = match &self.backend {
            Backend::Virtual { .. } => VIRTUAL_BLOCK as u32,
            Backend::PipeWire(output) => match output.quantum() {
                0 => 1024,
                quantum => quantum,
            },
        };
        self.plugins.set_bridge_block(block);
        self.plugins
            .set_real_time(!matches!(self.backend, Backend::Virtual { .. }));
        // Connect the inputs while a track of their kind is armed, and only
        // then.
        let armed = |kind| {
            self.session
                .project()
                .tracks
                .iter()
                .any(|t| t.armed && t.kind == kind)
        };
        let (audio, midi) = (armed(TrackKind::Audio), armed(TrackKind::Midi));
        if audio != self.capturing {
            self.capturing = audio;
            if let Backend::PipeWire(output) = &self.backend {
                output.set_capture(audio);
            }
        }
        self.midi_capturing = midi;
        let routes = midi_routes(self.session.project());
        if routes != self.midi_routes {
            if let Backend::PipeWire(output) = &self.backend {
                output.set_midi_routes(routes.clone());
            }
            self.midi_routes = routes;
        }
        if self.plugins.take_back(self.engine.take_retired_plugins()) {
            self.synced_revision = None;
        }
        // Devices whose reported latency changed need new delays.
        let reported = self.reported_midi_latency();
        if reported != self.device_latency {
            self.device_latency = reported;
            self.synced_revision = None;
        }
        let revision = self.session.revision();
        if self.synced_revision == Some(revision) || self.engine.graphs_in_flight() > 0 {
            return;
        }
        let project = self.session.project();
        let graph = Graph::compile_with_devices(
            project,
            &self.media,
            &mut self.plugins.provider(&project.plugin_states),
            &self.device_latency,
        );
        let left = self.plugins.forget_missing(project);
        for (id, state) in left {
            self.session.set_plugin_state(id, Some(state));
        }
        let latency = graph.latency();
        match self.engine.set_graph(graph) {
            Ok(()) => {
                self.synced_revision = Some(revision);
                self.plugin_latency = latency;
            }
            // Retried on the next change.
            Err(e) => tracing::warn!("engine graph not updated: {e}"),
        }
        self.pump();
        // A graph that didn't make it hands its new plugins straight back.
        if self.plugins.take_back(self.engine.take_retired_plugins()) {
            self.synced_revision = None;
        }
    }

    /// With the virtual backend, applies queued engine messages right away
    /// so the transport state is current.
    fn pump(&mut self) {
        if let Backend::Virtual { processor, .. } = &mut self.backend {
            processor.process(&mut [], &mut []);
            self.engine.collect_garbage();
        }
    }
}

/// Linear peak levels for drawing meters.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DisplayMeters {
    pub master: [f32; 2],
    pub tracks: HashMap<TrackId, [f32; 2]>,
}

/// Collects engine meter readings for two consumers that read at different
/// rates: MCP clients (peaks since their last call) and the UI (decaying
/// peak hold).
#[derive(Debug, Default)]
struct MeterAccumulator {
    since_last_call: Option<MeterReading>,
    display: DisplayMeters,
    display_at: Option<Instant>,
}

impl MeterAccumulator {
    fn add(&mut self, reading: &MeterReading) {
        match &mut self.since_last_call {
            None => self.since_last_call = Some(reading.clone()),
            Some(acc) => {
                acc.master = max_db(acc.master, reading.master);
                for meter in &reading.tracks {
                    match acc.tracks.iter_mut().find(|m| m.track == meter.track) {
                        Some(m) => m.peak_dbfs = max_db(m.peak_dbfs, meter.peak_dbfs),
                        None => acc.tracks.push(meter.clone()),
                    }
                }
            }
        }
        let linear =
            |db: [Option<f64>; 2]| db.map(|d| d.map_or(0.0, |d| 10f32.powf(d as f32 / 20.0)));
        let max = |a: [f32; 2], b: [f32; 2]| [a[0].max(b[0]), a[1].max(b[1])];
        self.display.master = max(self.display.master, linear(reading.master));
        for TrackMeter { track, peak_dbfs } in &reading.tracks {
            let level = self.display.tracks.entry(*track).or_default();
            *level = max(*level, linear(*peak_dbfs));
        }
    }

    fn take_since_last_call(&mut self) -> MeterReading {
        self.since_last_call.take().unwrap_or(MeterReading {
            master: [None, None],
            tracks: Vec::new(),
        })
    }

    fn display(&mut self, now: Instant) -> DisplayMeters {
        let shown = self.display.clone();
        let dt = self
            .display_at
            .map_or(0.0, |t| now.duration_since(t).as_secs_f32());
        self.display_at = Some(now);
        // 20 dB per second.
        let factor = 10f32.powf(-dt);
        let decay = |l: &mut [f32; 2]| l.iter_mut().for_each(|v| *v *= factor);
        decay(&mut self.display.master);
        self.display.tracks.values_mut().for_each(decay);
        shown
    }
}

/// Waits up to `wait` for the audio thread to confirm the end of a
/// recording, then assumes it has stopped writing.
fn wait_finished(shared: &RecordShared, wait: Duration) {
    let deadline = Instant::now() + wait;
    while !shared.finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    if !shared.finished() {
        tracing::warn!("the audio thread didn't confirm the end of recording");
        shared.finish();
    }
}

/// The `add_midi_clip` (without a track) that places a MIDI take, shifted
/// earlier by `latency` frames. `None` if nothing is left after the shift.
fn midi_clip(
    project: &Project,
    take: &MidiTake,
    latency: &impl Fn(u32) -> i64,
    muted: bool,
) -> Option<AddMidiClip> {
    // Each note and change moves by its device's latency; the clip spans
    // them all.
    let sources = take
        .notes
        .iter()
        .map(|n| n.source)
        .chain(take.controls.iter().map(|c| c.source));
    let (most, least) = sources.fold((i64::MIN, i64::MAX), |(most, least), source| {
        let latency = latency(source);
        (most.max(latency), least.min(latency))
    });
    let start = (take.start as i64 - most).max(0) as u64;
    let end = take.end as i64 - least;
    if end <= start as i64 {
        return None;
    }
    let shift = |frame: u64, source: u32| frame as i64 - latency(source);
    let map = project.tempo_map();
    let notes: Vec<NoteSpec> = take
        .notes
        .iter()
        .filter(|n| shift(n.start, n.source) >= start as i64)
        .map(|n| {
            let from = shift(n.start, n.source) as u64 - start;
            let to = (shift(n.end, n.source).max(shift(n.start, n.source) + 1)) as u64 - start;
            let start_beats = map.clip_beats(start, from);
            NoteSpec {
                pitch: n.pitch,
                start_beats,
                length_beats: map.clip_beats(start, to) - start_beats,
                velocity: n.velocity.max(1),
            }
        })
        .collect();
    if notes.is_empty() {
        return None;
    }
    let controls = take
        .controls
        .iter()
        .filter(|c| shift(c.frame, c.source) >= start as i64)
        .map(|c| ControlSpec {
            controller: c.controller,
            time_beats: map.clip_beats(start, shift(c.frame, c.source) as u64 - start),
            value: c.value,
        })
        .collect();
    Some(AddMidiClip {
        track: None,
        track_name: None,
        position_s: project.frames_to_seconds(start),
        length_s: Some(project.frames_to_seconds(end as u64 - start)),
        notes,
        controls,
        muted,
        take: true,
    })
}

/// A MIDI file's time signatures as a map for the project, with the
/// file's beat 0 at project beat `offset`. A change that doesn't fall on a
/// bar line of the map so far starts at the next one.
fn meter_map_of(file: &MidiFile, offset: f64) -> SetTimeSignatureMap {
    let (numerator, denominator) = file
        .meters
        .first()
        .filter(|(beat, ..)| *beat <= 0.0)
        .map_or((4, 4), |&(_, n, d)| (n, d));
    let start = TimeSignature {
        numerator,
        denominator,
    };
    let mut changes: Vec<MeterChange> = Vec::new();
    for &(beat, numerator, denominator) in file.meters.iter().filter(|(b, ..)| *b > 0.0) {
        let map = MeterMap::new(start, &changes);
        let at = beat + offset;
        let bar = map.bar_at(at + 1e-9);
        let bar = if (map.bar_start(bar) - at).abs() < 1e-6 {
            bar
        } else {
            bar + 1
        };
        if bar == 0 {
            continue;
        }
        let change = MeterChange {
            bar: bar + 1,
            numerator,
            denominator,
        };
        changes.retain(|c| c.bar != change.bar);
        changes.push(change);
    }
    SetTimeSignatureMap {
        numerator,
        denominator,
        changes,
    }
}

/// The MIDI devices to connect: what armed MIDI tracks listen to, and
/// where MIDI tracks send.
fn midi_routes(project: &Project) -> MidiRoutes {
    let midi = || project.tracks.iter().filter(|t| t.kind == TrackKind::Midi);
    let armed: Vec<_> = midi().filter(|t| t.armed).collect();
    let capture = if armed.is_empty() {
        MidiCaptureRoute::Off
    } else if armed.iter().any(|t| t.midi_input.is_none()) {
        MidiCaptureRoute::All
    } else {
        let mut devices: Vec<String> = armed.iter().filter_map(|t| t.midi_input.clone()).collect();
        devices.sort();
        devices.dedup();
        MidiCaptureRoute::Devices(devices)
    };
    let mut outputs: Vec<String> = midi()
        .filter_map(|t| t.midi_output.as_ref().map(|o| o.device.clone()))
        .collect();
    outputs.sort();
    outputs.dedup();
    MidiRoutes { capture, outputs }
}

fn max_db(a: [Option<f64>; 2], b: [Option<f64>; 2]) -> [Option<f64>; 2] {
    [0, 1].map(|i| match (a[i], b[i]) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, y) => x.or(y),
    })
}

fn start_backend(
    sample_rate: u32,
    kind: &AudioBackend,
) -> Result<(EngineController, Backend), StudioError> {
    let (engine, processor) = new_engine(sample_rate, Graph::empty());
    let backend = match kind {
        AudioBackend::Virtual => Backend::Virtual {
            processor: Box::new(processor),
            buffers: [Vec::new(), Vec::new()],
        },
        AudioBackend::PipeWire(options) => {
            Backend::PipeWire(PipeWireOutput::start(processor, options)?)
        }
    };
    Ok((engine, backend))
}

/// Copies the files in `from` (recursively) into `to`, replacing what's
/// there unless it's as new and as long. Nothing to copy (no `from`) is
/// fine.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(from) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    std::fs::create_dir_all(to)?;
    for entry in entries {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
            continue;
        }
        let (source, kept) = (entry.metadata()?, std::fs::metadata(&target).ok());
        let current = kept.is_some_and(|kept| {
            kept.len() == source.len()
                && matches!((kept.modified(), source.modified()), (Ok(k), Ok(s)) if k >= s)
        });
        if !current {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
