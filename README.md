# derpcat-wayland-dnd

Reusable native Wayland drag-and-drop support for Rust GUI and plugin windows.

This crate owns the compositor-facing Wayland data-device pieces needed to start
an external file/data drag from a native Wayland window. It is intended for GUI
toolkits, plugin editor hosts, and window adapters that already have access to
the Wayland parent handles for their window.

## What It Implements

- Builds `wl_data_source` offers for one or more MIME payloads.
- Starts native drags through `wl_data_device.start_drag`.
- Tracks the required Wayland origin state: `wl_surface`, `wl_seat`,
  `wl_data_device`, and the pointer-button serial that initiated the gesture.
- Handles the data-source lifecycle: target MIME acceptance, compositor
  `send` requests, `drop_performed`, `finished`, and cancellation cleanup.
- Converts `raw-window-handle` Wayland display/surface handles into typed
  Wayland client objects for adapters that receive foreign parent handles.

## What It Does Not Solve

- It does not bridge XWayland/XDND drags into native Wayland targets.
- It does not bypass compositor policy. A compositor may reject or constrain
  cross-client drag behavior.
- It does not invent a valid input serial. The serial must come from the
  pointer press that begins the drag on the origin surface.
- It does not provide a toolkit-specific event loop, widget, cursor image, or
  file encoder.
- It does not replace portal file-transfer APIs. Portals can help with file
  access, but they do not start a Wayland drag by themselves.

If a host stack only exposes an X11/XWayland window, MIME tweaks and portal
tokens are not enough. Native Wayland drag-out needs ownership of the native
Wayland surface, seat/data-device, and initiating serial.

## Cross-Backend Bridges

XWayland-to-Wayland and Wayland-to-XWayland drag delivery is compositor or
Xwayland-manager work, not normal client work.

A full bridge has to observe XDND ownership on the X side, track pointer focus
on both display systems, translate MIME/data requests, and synthesize the
destination-side drag events from inside the compositor. A normal application
cannot legally mint the `wl_data_device.start_drag` origin surface and implicit
grab serial required by the Wayland core protocol.

Known working designs, such as KWin's Xwayland bridge, implement this inside
the compositor: when an X client owns `XdndSelection`, the compositor maps an X
proxy over native Wayland targets and translates XDND into Wayland data-device
events. In the opposite direction, the compositor claims `XdndSelection` and
translates a native Wayland source into XDND messages for Xwayland clients.

For application/toolkit code, the practical routes are:

- Use this crate when the source window is native Wayland.
- Use XDND when the source window is X11/XWayland and the target is also X11 or
  XWayland.
- Rely on the compositor for cross-backend delivery.
- If the compositor does not bridge a direction, run the source window on the
  target's native side or add the missing bridge in the compositor/plugin layer.

## Minimal Integration

1. Capture the native Wayland parent handles from your windowing layer.
2. Build a `ForeignWaylandParent` from the `raw-window-handle` display and
   window handles.
3. Create a `WaylandRuntime` for that parent and dispatch it regularly from the
   GUI thread.
4. Let the runtime observe pointer button events so it can remember the serial
   for the drag gesture.
5. Build a `WaylandExternalDragRequest` with one or more `WaylandDragOffer`
   values.
6. Call `WaylandRuntime::start_external_drag` while handling the pointer gesture
   that should initiate drag-out.

```rust,no_run
use derpcat_wayland_dnd::{
    ForeignWaylandParent, WaylandDragOffer, WaylandExternalDragRequest,
    WaylandRuntime,
};
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
use std::path::PathBuf;

fn start_drag(
    display: RawDisplayHandle,
    window: RawWindowHandle,
    file_uri_list: Vec<u8>,
    path: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let parent = ForeignWaylandParent::from_raw_handles(display, window)?;
    let mut runtime = WaylandRuntime::new(parent)?;

    let request = WaylandExternalDragRequest::new(
        vec![WaylandDragOffer::new("text/uri-list", file_uri_list)],
        vec![path],
    );

    runtime.start_external_drag(request)?;
    runtime.dispatch_pending()?;
    Ok(())
}
```

In a real adapter, keep the runtime alive for the window lifetime. Starting a
drag from a freshly created runtime, as shown above, only works if the runtime
has already observed the relevant seat, data-device, surface, and pointer
serial state.

## Supported Platform

This crate is Linux/Wayland specific. Non-Wayland backends should keep using
their native drag implementation and report `UnsupportedBackend` before calling
into this crate.

## Development

```sh
cargo fmt --check
cargo test
cargo check
```
