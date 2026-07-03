# audio-plugin-dnd

Drag-and-drop protocol helpers for Rust audio plugin GUIs.

`audio-plugin-dnd` is the shared drag-out layer extracted from BUFFR. It is
meant for plugins that render audio, MIDI, presets, clips, or other temporary
files and then let the user drag those files from the plugin editor into a DAW,
file manager, or desktop target.

The crate owns the reusable DND protocol pieces:

- BUFFR-style drag arming, movement threshold, render-cache slot, short UI
  flash state, and self-drop blocking.
- File drag payload construction for `text/uri-list`, UTF-8 URI lists,
  `text/x-uri`, KDE URI lists, GNOME copied-files payloads, and plain path
  text.
- Toolkit-neutral queued drag payloads with optional waveform or spectral
  previews.
- Backend route/diagnostic types for XDND, native Wayland data-device, Windows
  OLE, and macOS AppKit.
- Experimental native Wayland `wl_data_device.start_drag` support.

The crate does not render audio, write MIDI, encode files, or own your plugin
GUI. Your plugin renders a file on the GUI/background side, then gives this
crate the resulting path and optional preview metadata.

## Status

- Plugin drag lifecycle: implemented.
- File MIME payload generation: implemented.
- Toolkit-neutral drag queue: implemented.
- Native Wayland drag source: implemented, experimental.
- X11/XWayland XDND backend: documented route, intended backend.
- Windows OLE backend: documented route, intended backend.
- macOS AppKit backend: documented route, intended backend.

Wayland is the weird part. Native Wayland drag-out requires an origin
`wl_surface`, active `wl_seat`, `wl_data_device`, and the pointer-button serial
from the input event that initiated the drag. A plugin cannot invent that serial
later. XWayland-to-native-Wayland delivery is compositor bridge behavior, not a
normal app-level feature.

## Typical Plugin Flow

1. User presses on a draggable waveform, spectral selection, MIDI region, or
   export handle.
2. Plugin calls `DragExportState::arm_audio`, `arm_midi`, or `arm`.
3. While the pointer moves, plugin checks `armed_drag_ready`.
4. Plugin renders or reuses the temp export file outside the audio thread.
5. Plugin calls `queue_exported_file`.
6. The window adapter takes `ExternalDragQueue::take()` and starts the platform
   drag backend.
7. Plugin ignores file drops that match `is_own_export_path`.

```rust,no_run
use audio_plugin_dnd::{
    DragExportState, DragPreview, ExternalDragQueue, Point, RenderCache,
};
use std::path::PathBuf;

fn begin_waveform_drag(state: &mut DragExportState<String>, pointer: Point) {
    state.arm_audio(
        pointer,
        "Audio selection",
        DragPreview::Audio(vec![(-0.2, 0.4), (-0.6, 0.7)]),
    );
}

fn continue_drag(
    state: &mut DragExportState<String>,
    queue: &mut ExternalDragQueue,
    pointer: Point,
) -> Result<(), Box<dyn std::error::Error>> {
    if !state.armed_drag_ready(pointer) {
        return Ok(());
    }

    let cache_key = "flac-24-selection".to_string();
    let (path, reused) = if let Some(path) = state.reusable_cache_path(&cache_key) {
        (path.to_path_buf(), true)
    } else {
        let path = render_audio_temp_file()?;
        state.set_render_cache(RenderCache {
            key: cache_key,
            path: path.clone(),
        });
        (path, false)
    };

    state.queue_exported_file(queue, path, reused);
    Ok(())
}

fn render_audio_temp_file() -> Result<PathBuf, Box<dyn std::error::Error>> {
    // Render from your plugin's non-audio-thread export pipeline.
    Ok(PathBuf::from("/tmp/plugin-export.flac"))
}
```

## File Payloads

Use `FileDragPayloadData` when implementing a backend or testing DAW/file
manager compatibility:

```rust
use audio_plugin_dnd::{FileDragPayloadData, MIME_TEXT_URI_LIST};
use std::path::PathBuf;

let payload = FileDragPayloadData::new(vec![PathBuf::from("/tmp/take 01.flac")])?;
assert!(payload
    .offers()
    .iter()
    .any(|offer| offer.mime_type() == MIME_TEXT_URI_LIST));
# Ok::<(), String>(())
```

## Native Wayland

The Wayland backend owns the compositor-facing data-device path:

- Build `wl_data_source` offers.
- Start `wl_data_device.start_drag`.
- Track surface, seat, data-device, and initiating pointer serial.
- Serve bytes when the compositor sends a `wl_data_source.send` request.
- Clean up on `drop_performed`, `finished`, or cancellation.

```rust,no_run
use audio_plugin_dnd::{
    FileDragPayloadData, ForeignWaylandParent, WaylandExternalDragRequest, WaylandRuntime,
};
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
use std::path::PathBuf;

fn start_wayland_drag(
    display: RawDisplayHandle,
    window: RawWindowHandle,
    path: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let parent = ForeignWaylandParent::from_raw_handles(display, window)?;
    let mut runtime = WaylandRuntime::new(parent)?;
    let payload = FileDragPayloadData::new(vec![path.clone()])?;
    let request = WaylandExternalDragRequest::new(payload.wayland_offers(), vec![path]);

    runtime.start_external_drag(request)?;
    runtime.dispatch_pending()?;
    Ok(())
}
```

In a real adapter, keep `WaylandRuntime` alive for the editor window lifetime
and dispatch it from the GUI thread so it can observe the seat, data-device,
surface, and pointer serial before a drag begins.

## Platform Reality

The same plugin-facing lifecycle can feed multiple native backends:

- X11/XWayland source windows use XDND.
- Native Wayland source windows use Wayland data-device.
- Windows uses OLE `DoDragDrop`/`CF_HDROP`.
- macOS uses AppKit pasteboard dragging from an `NSView`.

Crossing XWayland and native Wayland is not guaranteed by changing MIME payloads
inside the plugin. That direction needs compositor/Xwayland-manager bridge
support because the destination-side protocol events are privileged by the
display server.

## Development

```sh
cargo fmt --check
cargo test
cargo check
cargo publish --dry-run
```

## License

ISC
