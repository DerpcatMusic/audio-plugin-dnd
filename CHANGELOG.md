# Changelog

Concise public-facing release notes. Keep entries newest-first. Use `## x.y.z - YYYY-MM-DD` for released versions, with 1-5 bullets that describe user-visible behavior without exposing private implementation details.

## Unreleased

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
