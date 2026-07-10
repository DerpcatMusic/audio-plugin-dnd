# Changelog

Concise public-facing release notes. Keep entries newest-first. Use `## x.y.z - YYYY-MM-DD` for released versions, with 1-5 bullets that describe user-visible behavior without exposing private implementation details.

## Unreleased

## 0.1.25 - 2026-07-09

- After XWayland→Wayland handoff, the next drag can start immediately instead of waiting out the bridge linger.

## 0.1.24 - 2026-07-09

- Linux XDND drop targeting uses raw X11 pointer coords (no Hyprland scale remap), so the hotspot stays under the cursor on scaled desktops.

## 0.1.23 - 2026-07-09

- Drag chip is static (no spring follow); thumbnails stay signal-accurate.
- Spectrogram chip reads column-major energy so the heatmap matches the selection.
- MIDI chip draws real note bars from the dragged selection instead of decorative placeholders.

## 0.1.22 - 2026-07-09

- Linux X11 drag chip redraws while moving so the thumbnail stays visible (no black box).
- Wayland handoff destroys the X11 preview and uses the native drag icon again so drops are not blocked.

## 0.1.21 - 2026-07-09

- Drag chip spring is softer and more tactile; lean is positional so no black box sits under a tilted thumbnail.
- After XWayland→Wayland handoff the spring preview keeps following the cursor instead of freezing to a static icon.

## 0.1.20 - 2026-07-09

- Linux drag preview follows the X11 pointer directly on scaled Hyprland desktops (no double scale remap).
- Native Wayland handoff linger now finishes cleanly so a new drag can start afterward.

## 0.1.19 - 2026-07-09

- Shared high-fidelity drag chip (waveform, spectral, MIDI) used across Linux, macOS, and Windows.
- Soft spring follow and slight tilt on hosts that move the preview themselves.
- macOS and Windows now show the custom drag thumbnail instead of the default file icon.

## 0.1.18 - 2026-07-08

- Improved Linux drag positioning and drop targeting on mixed-scale Wayland desktops, especially XWayland plugin windows on secondary monitors.
- Added clearer drag diagnostics for pointer mapping and target detection.

## 0.1.17 - 2026-07-07

- Allowed outbound drags to target another plugin editor window while still cancelling drops back onto the source editor.

## 0.1.16 - 2026-07-06

- Fixed premature native Wayland handoff when dragging from an XWayland plugin editor on Hyprland; XDND now stays active over the host and Bitwig until the pointer actually leaves the plugin window.
- Native handoff to Wayland apps such as Discord still triggers after you cross off the plugin UI onto an anonymous XWayland bridge.

## 0.1.15 - 2026-07-06

- Outbound drags now start with XDND immediately and switch to native Wayland delivery when the pointer settles over a native app, so crossing an X11 host on the way to Discord or Vesktop no longer locks the wrong route.
- Removed preflight pointer polling that could commit to XDND before you reached your real drop target.

## 0.1.14 - 2026-07-06

- Preflight no longer early-exits on unclassified targets while the pointer is still over the host window, so outbound drags do not start the native Wayland route prematurely.
- Drop-target classification now searches XdndAware descendants, so X11 hosts such as Bitwig are recognized even when the toplevel leaf is not XdndAware.
- Unclassified external targets from XWayland editors route straight to XDND instead of native-first.

## 0.1.13 - 2026-07-06

- Outbound drags now classify the drop target at pointer release (or stable external hover) instead of at drag start, so X11 hosts and native Wayland apps each get the correct delivery route.
- Reverted the always-XDND shortcut for XWayland editors; routing again follows the classified target under the cursor.
- Drops released back over the plugin UI cancel cleanly without starting a backend worker.

## 0.1.12 - 2026-07-06

- Outbound file drags from embedded XWayland plugin editors now always use XDND instead of trying native Wayland first, so drops into Bitwig and other X11 hosts no longer stall when the drag starts over the plugin UI.
- Registered plugin editor windows under the pointer are classified as plugin-owned targets for clearer routing logs.

## 0.1.11 - 2026-07-06

- Outbound drags now pick XDND or native Wayland based on the window under the pointer, so drops into X11 hosts such as Bitwig skip the native bridge attempt.
- Plugin editor windows register on open so outbound drags no longer treat floating panels as drop targets.

## 0.1.10 - 2026-07-06

- Fixed drag lifecycle events being lost when multiple plugin windows drain the shared event bus so each drag id now has its own routable terminal state.
- Added per-drag worker tracking so GUI adapters can clear in-flight state as soon as the backend worker exits instead of waiting for a 30-second watchdog.

## 0.1.9 - 2026-07-06

- Fixed drag-and-drop breaking after dropping into Bitwig on Hyprland by requiring stronger bridge evidence before treating a transfer as complete and falling back to XDND when a native session only saw a hover probe.
- Fixed the drop router reclaiming the host proxy mid-drag by pausing proxy claims while an outbound drag is active.
- Added a lingering lifecycle phase so GUI adapters can distinguish bridge cleanup from a finished drag.

## 0.1.8 - 2026-07-06

- Fixed fast native Wayland drags failing after hover-only data requests by waiting for the physical mouse release before treating bridge transfers as complete.
- Fixed overlapping native drag sessions by keeping GUI drag state in flight until the backend reports a terminal result.

## 0.1.7 - 2026-07-06

- Fixed Hyprland crashes after drops into X11 targets by keeping the native Wayland drag client connected until the compositor bridge finishes cleanup.

## 0.1.6 - 2026-07-06

- Added native Wayland drag icons for preview-capable drag payloads.
- Added a quiet-target fallback so GUI adapters recover when a backend sees target activity but misses a terminal drag callback.

## 0.1.5 - 2026-07-06

- Added typed drag lifecycle events so GUI adapters can keep one outbound drag in flight until the platform backend finishes, cancels, or fails it.
- Improved Hyprland Wayland/XWayland drag routing so outbound drags are not relayed back into the source editor as inbound drops.
- Finished bridged native drags promptly after a target requests file data and the session goes quiet.

## 0.1.4 - 2026-07-06

- Added native Wayland drag-out routing for XWayland plugin editors on Hyprland, with fallback to the standard XDND route when unavailable.
- Added routed inbound drops so embedded XWayland plugin editors can receive files dragged from native Wayland apps.
- Fixed X11/XWayland file drags that request plain text paths so non-WAV audio files can be handed to hosts consistently.

## 0.1.3 - 2026-07-06

- Added Windows OLE and macOS AppKit native launchers behind the raw-window backend API.
- Added target-specific dependencies for Windows shell drag payloads and AppKit pasteboard drags.
- Updated backend status documentation to mark Linux, Windows, and macOS launchers as implemented.

## 0.1.2 - 2026-07-06

- Added the Linux X11/XWayland XDND native launcher behind the raw-window backend API.
- Added Linux portal file-transfer payload support for XDND targets that request portal data.
- Added XDND diagnostics for target discovery, acceptance, data requests, and bridge behavior.

## 0.1.1 - 2026-07-06

- Added the first public native backend adapter contract around raw window handles.
- Added backend diagnostics and route-aware start results for toolkit integrations.
- Kept native launchers conservative while the XDND/OLE/AppKit implementations move behind the API.

## 0.1.0 - 2026-07-06

- Added the initial plugin drag lifecycle, file payload, queue, and experimental Wayland protocol core.
