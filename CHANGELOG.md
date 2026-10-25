# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
