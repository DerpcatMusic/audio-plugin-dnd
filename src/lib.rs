#![doc = include_str!("../README.md")]

pub mod backend;
#[cfg(all(target_family = "unix", not(target_os = "macos")))]
pub mod data_device;
pub mod file_payload;
#[cfg(all(target_family = "unix", not(target_os = "macos")))]
pub mod foreign;
pub mod platform;
pub mod plugin;
pub mod queue;
pub mod request;
#[cfg(all(target_family = "unix", not(target_os = "macos")))]
pub mod runtime;
#[cfg(all(target_family = "unix", not(target_os = "macos")))]
pub mod state;

pub use backend::{
    drain_backend_events, emit_backend_event, BackendStart, DragWindow, ExternalDragBackend,
    ExternalDragError, RawWindowBackend,
};
#[cfg(all(target_family = "unix", not(target_os = "macos")))]
pub use data_device::{ActiveWaylandDrag, PendingWaylandDrag};
pub use file_payload::{
    file_uri, file_uri_list, gnome_copied_file_list, plain_file_list, FileDragOffer,
    FileDragPayloadData, FILE_DRAG_OFFER_COUNT, MIME_GNOME_COPIED_FILES, MIME_KDE_URI_LIST,
    MIME_TEXT_PLAIN, MIME_TEXT_PLAIN_UTF8, MIME_TEXT_URI_LIST, MIME_TEXT_URI_LIST_UTF8,
    MIME_TEXT_X_URI,
};
#[cfg(all(target_family = "unix", not(target_os = "macos")))]
pub use foreign::{display_from_surface_ptr, ForeignWaylandParent, ForeignWaylandParentError};
pub use plugin::{
    DragArmed, DragExportState, DragFlash, DragPayloadKind, DragPreview, DragStart, DragStatus,
    Point, RenderCache, SpectralDragPreview, DRAG_START_DISTANCE_POINTS,
};
pub use queue::{
    next_external_drag_id, ExternalDragPayload, ExternalDragPreview, ExternalDragQueue,
};
pub use request::{WaylandDragOffer, WaylandExternalDragError, WaylandExternalDragRequest};
#[cfg(all(target_family = "unix", not(target_os = "macos")))]
pub use runtime::{WaylandRuntime, WaylandRuntimeError, WaylandRuntimeState};
#[cfg(all(target_family = "unix", not(target_os = "macos")))]
pub use state::WaylandDragState;
