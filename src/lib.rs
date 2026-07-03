//! Reusable Linux Wayland drag-and-drop support for Rust GUI and plugin windows.
//!
//! This crate owns the compositor-facing Wayland DND protocol pieces so plugin
//! wrappers can share the same implementation instead of carrying divergent
//! copies inside each product.
//!
//! The core operation is `wl_data_device.start_drag`, driven through Smithay
//! Client Toolkit's data-device abstractions. A successful native Wayland drag
//! requires an origin `wl_surface`, a `wl_seat` with a `wl_data_device`, and
//! the pointer-button serial from the input event that initiated the drag
//! gesture. The compositor then drives the `wl_data_source` lifecycle by
//! accepting a MIME type, requesting bytes with `send`, reporting
//! `drop_performed`, and finishing or cancelling the source.
//!
//! This crate does not bridge XWayland/XDND drags into native Wayland clients,
//! bypass compositor restrictions, or replace file-transfer portals. Callers
//! must still provide toolkit-specific event-loop integration, gesture
//! detection, cursor/icon handling, and payload generation.

pub mod data_device;
pub mod foreign;
pub mod request;
pub mod runtime;
pub mod state;

pub use data_device::{ActiveWaylandDrag, PendingWaylandDrag};
pub use foreign::{display_from_surface_ptr, ForeignWaylandParent, ForeignWaylandParentError};
pub use request::{WaylandDragOffer, WaylandExternalDragError, WaylandExternalDragRequest};
pub use runtime::{WaylandRuntime, WaylandRuntimeError, WaylandRuntimeState};
pub use state::WaylandDragState;
