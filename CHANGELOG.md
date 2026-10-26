# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.5](https://github.com/quonic/dawkey/compare/v0.1.4...v0.1.5) - 2026-10-26

### Added

- *(mcp)* list a choice parameter's options in get_plugin
- add a Claude Code plugin with a skill for working in Dawkey

### Fixed

- stop repeat_clip adding a sliver of an extra copy at the end
- *(mcp)* stop requiring a position in seconds when it's given in beats or bars

### Other

- changes ci to self-hosted isolet

## [0.1.4](https://github.com/quonic/dawkey/compare/v0.1.3...v0.1.4) - 2026-10-26

### Fixed

- *(mcp)* put import_midi at the bar or beat asked for when it takes the file's tempo
- *(plugins)* keep plugins that print to stdout while they're scanned
- *(plugins)* let sandboxed plugins save a first LV2 or VST3 preset
- keep a recording in progress when replacing the project
- *(engine)* play an instrument plugin's notes in time when its sidechain is late
- *(mcp)* tell clients that only read and subscribe to the project of its changes
- *(mcp)* take strings in a batch that only start with $ as they are
- *(ui)* drop files where the pointer is at a UI scale other than 1
- *(mcp)* measure set_clip_length's length_beats at the tempo where the clip starts
- *(media)* write WAV exports past 4 GiB as RF64
- *(plugins)* send a bridged plugin's MIDI after the latency it reports
- *(mcp)* make get_changes' complete flag right across new projects and gestures
- *(plugins)* keep a late block's events for a bridged plugin
- *(media)* stop sample-rate conversion from adding a frame per second
- *(ui)* keep keys from reaching the window behind the preferences
- *(media)* refuse FLAC exports above 96 kHz before rendering
- *(ui)* keep the other preferences when one in the file has the wrong type
- *(ui)* stop dragged notes at the ends of the keyboard without collapsing chords
- *(ui)* stop ctrl shortcuts typing their letter into names
- *(ui)* keep what's typed in a number field when it's clicked again
- *(ui)* keep the selected automation point and tempo change through an undo
- *(ui)* keep the plugin picker and editor open when their panel is clicked
- *(ui)* finish drags that a right click or a view change interrupts
- *(core)* stop saving over a bundle reusing another project's audio files
- *(core)* repair sends when loading a project, and refuse sends to the master
- *(core)* remove send automation with the bus it sends to
- *(core)* make add_midi_clip with no notes a bar long on a bar line
- *(core)* stop note edits in big MIDI clips freezing for seconds
- *(engine)* line up post-fader aux sends from latency-compensated tracks
- *(engine)* stop latency-compensated tracks replaying old audio when playback starts
- *(engine)* release an instrument plugin's notes when it's bypassed or the track plays a device
- *(engine)* stop instrument plugin notes sticking when an edit follows stop, locate or a loop wrap
- *(engine)* follow send automation while the transport is stopped
- *(engine)* keep aux send, plugin output and sidechain delays across edits during playback
- *(engine)* enter and wrap loops shorter than an audio block
- *(engine)* render and export straight through the loop instead of repeating it
- *(engine)* keep MIDI 2.0 poly pressure, program changes and RPNs from live input
- *(mcp)* refuse render segments that would be too many instead of running out of memory
- *(media)* export to file names close to the length limit
- *(media)* stop reporting NaN audio as digital silence
- *(media)* import WAV and FLAC files that were cut off
- *(plugins)* stop a load into a busy plugin process hanging Dawkey
- *(plugins)* stop a plugin's helper process stalling the plugin scan
- *(plugins)* scale LV2 ports' bounds and default by the sample rate with lv2:sampleRate
- *(plugins)* read and write LV2 atom:Literal state values with their header
- *(plugins)* keep the bounds and default an LV2 control port declares
- *(studio)* keep the other tracks' takes when an armed track is removed while recording

### Other

- Merge branch 'fix/mcp' into fix/open-bugs
- Merge branch 'fix/bridge' into fix/open-bugs
- Merge branch 'fix/engine-transport' into fix/open-bugs
- Merge branch 'fix/core' into fix/open-bugs
- Merge branch 'fix/media' into fix/open-bugs
- Merge branch 'fix/ui-keys' into fix/open-bugs
- *(mcp)* mention FLAC's 96 kHz limit, RF64 and non_finite in the render and export tools
- Merge branch 'fix/lv2-scan' into fix/open-bugs

## [0.1.3](https://github.com/quonic/dawkey/compare/v0.1.2...v0.1.3) - 2026-10-25

### Other

- docs
- Merge pull request #79 from quonic/dev
- Fix import_midi panicking with several replacing targets

## [0.1.2](https://github.com/quonic/dawkey/compare/v0.1.1...v0.1.2) - 2026-10-25

### Other

- Set plugin properties that aren't parameters
- Add aux sends
- Export to Ogg Vorbis and Opus
- Export a seamless loop: fold the tail onto the start
- Render stems and chosen tracks, and analyze loudness over time
- Import a MIDI file into existing tracks
- Add a batch tool: several commands in one call and one undo step
- Tell clients what changed the project outside their calls
- Smaller get_state, open_project and list_plugins results
- Take positions in beats and bars over MCP, and report notes in beats
- Make the output folder before rendering or exporting
- Report the transport as it is once play, stop, record and locate took effect

## [0.1.1](https://github.com/quonic/dawkey/compare/v0.1.0...v0.1.1) - 2026-10-24

### Other

- sets up release ci
- keep it ASCII
- Adds ability to rename tracks and clips
- formatting
- Defines a boarder when renaming
- Fixes workflow error
