# Changelog

## 0.1.3

- Added Windows OLE and macOS AppKit native launchers behind the raw-window backend API.
- Added target-specific dependencies for Windows shell drag payloads and AppKit pasteboard drags.
- Updated backend status documentation to mark Linux, Windows, and macOS launchers as implemented.

## 0.1.2

- Added the Linux X11/XWayland XDND native launcher behind the raw-window backend API.
- Added Linux portal file-transfer payload support for XDND targets that request portal data.
- Added XDND diagnostics for target discovery, acceptance, data requests, and bridge behavior.

## 0.1.1

- Added the first public native backend adapter contract around raw window handles.
- Added backend diagnostics and route-aware start results for toolkit integrations.
- Kept native launchers conservative while the XDND/OLE/AppKit implementations move behind the API.

## 0.1.0

- Added the initial plugin drag lifecycle, file payload, queue, and experimental Wayland protocol core.
