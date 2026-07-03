#![doc = include_str!("../README.md")]

pub mod backend;
pub mod data_device;
pub mod file_payload;
pub mod foreign;
pub mod platform;
pub mod plugin;
pub mod queue;
pub mod request;
pub mod runtime;
pub mod state;

pub use backend::{
    drain_backend_events, emit_backend_event, BackendStart, DragWindow, ExternalDragBackend,
    ExternalDragError, RawWindowBackend,
};
pub use data_device::{ActiveWaylandDrag, PendingWaylandDrag};
pub use file_payload::{
    file_uri, file_uri_list, gnome_copied_file_list, plain_file_list, FileDragOffer,
    FileDragPayloadData, FILE_DRAG_OFFER_COUNT, MIME_GNOME_COPIED_FILES, MIME_KDE_URI_LIST,
    MIME_TEXT_PLAIN, MIME_TEXT_PLAIN_UTF8, MIME_TEXT_URI_LIST, MIME_TEXT_URI_LIST_UTF8,
    MIME_TEXT_X_URI,
};
pub use foreign::{display_from_surface_ptr, ForeignWaylandParent, ForeignWaylandParentError};
pub use plugin::{
    DragArmed, DragExportState, DragFlash, DragPayloadKind, DragPreview, DragStart, DragStatus,
    Point, RenderCache, SpectralDragPreview, DRAG_START_DISTANCE_POINTS,
};
pub use queue::{
    next_external_drag_id, ExternalDragPayload, ExternalDragPreview, ExternalDragQueue,
};
pub use request::{WaylandDragOffer, WaylandExternalDragError, WaylandExternalDragRequest};
pub use runtime::{WaylandRuntime, WaylandRuntimeError, WaylandRuntimeState};
pub use state::WaylandDragState;
