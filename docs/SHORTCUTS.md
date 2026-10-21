# Keyboard and mouse

Only the left mouse button does anything in Dawkey, except for right-clicking the title bar. Dragging snaps to beats (or to the piano roll's grid) while the toolbar's Snap button is on; hold **Shift** while dragging to place things freely.

## Everywhere

| Key | Action |
| --- | --- |
| Space | Play or stop |
| Home | Move the playhead to the start |
| R | Start or stop recording |
| L | Turn the loop on or off |
| Ctrl+Z | Undo |
| Ctrl+Shift+Z, Ctrl+Y | Redo |
| Ctrl+S | Save |
| Ctrl+Shift+S | Save as |
| Ctrl+O | Open a project |
| Ctrl+I | Import audio files at the playhead |
| Ctrl+E | Export |
| Ctrl+, | Preferences |
| Tab | Switch between the arrange view and the mixer (from the piano roll, back to the arrange view) |
| + / − | Zoom the timeline in or out |

The Stop button jumps to the start when the transport is already stopped. The button next to "Auto" switches the automation mode between touch, latch and write.

## Arrange view

### Timeline

| Gesture | Action |
| --- | --- |
| Click the ruler or an empty lane | Move the playhead there |
| Drag on the ruler | Scrub |
| Shift+drag on the ruler | Set the loop range and turn looping on |
| Ctrl+drag on the ruler | Set the punch range and turn punch on |
| Double-click an empty MIDI lane | Add a one-bar MIDI clip |
| Scroll | Scroll through the tracks |
| Shift+scroll | Scroll through time |
| Ctrl+scroll | Zoom around the pointer |
| Drop audio files | Import them at the pointer, onto the track under it and new tracks after it |

### Clips

| Key or gesture | Action |
| --- | --- |
| Click | Select the clip |
| Double-click a MIDI clip, or Return | Open it in the piano roll |
| Drag | Move the clip in time, or onto another track of the same kind |
| Ctrl+drag | Copy the clip |
| Drag the left or right edge | Trim the clip |
| Ctrl+drag the right edge | Repeat the clip up to where you let go |
| S | Split the selected clip at the playhead |
| Ctrl+C | Copy the selected clip |
| Ctrl+V | Paste it at the pointer (or at the playhead) |
| Delete, Backspace | Delete the selected clip |
| Escape | Deselect the clip |

Clips never overlap in the window: a moved, copied or pasted clip goes to the nearest space it fits in, and trims and repeats stop at the next clip.

### Takes

Show a track's take lanes with its "Takes" button.

| Gesture | Action |
| --- | --- |
| Click a take | Make it the take that plays |
| Drag across a take lane | Make that lane play over the range you drag |
| Click a take lane's header | Make that lane play wherever it has clips |

### Automation lanes

Show a track's automation lanes with its automation button. A parameter's "automate" button in the plugin editor adds a lane for it.

| Key or gesture | Action |
| --- | --- |
| Double-click a lane | Add a point |
| Click a point | Select it |
| Drag a point | Move it in time and value |
| Delete, Backspace | Delete the selected point |

With "Auto" on while playing, moving a fader, a pan control or a plugin parameter writes its lane at the playhead.

### Tempo and time signature lanes

Click the toolbar's tempo or time signature to show these lanes.

| Key or gesture | Action |
| --- | --- |
| Double-click the tempo lane | Add a tempo change |
| Drag a tempo point up or down | Change its tempo (Ctrl+drag for finer steps) |
| Drag a tempo point sideways | Move it in time |
| Up / Down | Change the selected tempo by 1 BPM (Shift: 10 BPM) |
| Double-click the time signature lane | Add a change at the nearest bar |
| Drag a time signature change | Move it to another bar |
| Up / Down | Change the selected time signature's beats per bar (Shift: halve or double the beat length) |
| Delete, Backspace | Remove the selected change |
| Escape | Deselect it |

The lane headers' buttons change the selected point, or the one in effect at the playhead. Click the BPM or either number of the time signature to type a value.

## Piano roll

| Key or gesture | Action |
| --- | --- |
| Click a note | Select it |
| Shift+click, Ctrl+click | Add a note to the selection, or take it out |
| Drag on empty grid | Select the notes in a box (Shift or Ctrl adds to the selection) |
| Double-click empty grid | Add a note one grid step long |
| Drag a note | Move the selected notes in time and pitch |
| Drag a note's right edge | Change its length |
| Up / Down | Transpose the selected notes by a semitone (Shift: an octave) |
| Left / Right | Move the selected notes by one grid step |
| Ctrl+A | Select all notes |
| Q | Quantize the selected notes, or all of them |
| Delete, Backspace | Delete the selected notes (or the selected controller point) |
| Escape | Deselect the notes; with none selected, close the piano roll |
| Scroll | Scroll through pitches |
| Shift+scroll | Scroll through time |
| Ctrl+scroll | Zoom time around the pointer |

### Velocity and controller lane

The button at its left switches the lane between velocity and controllers.

| Gesture | Action |
| --- | --- |
| Drag a velocity bar | Change that note's velocity, or every selected note's |
| Draw across the lane | Set velocities, or controller values, along the stroke |
| Double-click (controllers) | Add a controller change |
| Drag a controller point | Move it |

## Mixer and plugins

| Gesture | Action |
| --- | --- |
| Drag a fader | Change the track's gain |
| Drag a pan control sideways | Change the pan |
| Click "+" on a strip | Open the plugin picker |
| Click a plugin slot | Open the plugin editor |
| Drag a parameter slider | Change the value |
| Double-click a parameter slider | Type a value |
| Ctrl+click a parameter slider | Reset it to its default |
| Escape | Close the plugin editor |

### Plugin picker

Type to search plugins by name, vendor, format or kind.

| Key | Action |
| --- | --- |
| Up / Down / Home / End | Move the highlight |
| Return | Add the highlighted plugin |
| Backspace | Delete the last character |
| Escape | Clear the search; when it's empty, close the picker |

## Text fields

Some values can be typed: click the BPM or a time signature number in the lane headers, or the record offset or MCP port in the preferences, or double-click a plugin parameter or the piano roll's MIDI output latency. The field starts with the value selected.

| Key | Action |
| --- | --- |
| Return | Set the value |
| Tab | Set the value (in the time signature, move on to the beat length) |
| Escape | Cancel |
| Backspace, Delete | Delete a character, or the selected value |

Clicking anywhere else also sets the value.

## Dialogs

| Key | Save changes? | Export | Recover unsaved changes? | Preferences |
| --- | --- | --- | --- | --- |
| Return | Save | Choose where to export | Recover | |
| Escape | Cancel | Close | | Close |

## Window

| Gesture | Action |
| --- | --- |
| Drag the title bar | Move the window |
| Double-click the title bar | Maximize or restore |
| Right-click the title bar | The window menu |
| Drag an edge or corner | Resize |
