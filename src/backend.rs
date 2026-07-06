//! Native backend adapter contract.
//!
//! This module is the public surface GUI/toolkit adapters should implement.
//! It intentionally depends on `raw-window-handle` instead of a specific UI
//! toolkit, so baseview, winit, Vizia, Slint, or custom plugin wrappers can all
//! feed the same drag protocol.

use std::collections::HashMap;
use std::sync::{
    mpsc::{channel, Receiver, Sender},
    Mutex, OnceLock,
};

use raw_window_handle::{
    HasRawDisplayHandle, HasRawWindowHandle, RawDisplayHandle, RawWindowHandle,
};

use crate::platform::{DragBackendKind, DragEndpointKind, DragRoute};
use crate::{ExternalDragPayload, FileDragPayloadData};

#[cfg(all(target_family = "unix", not(target_os = "macos")))]
mod dnd;
#[cfg(all(target_family = "unix", not(target_os = "macos")))]
mod linux;

#[cfg(all(target_family = "unix", not(target_os = "macos")))]
pub use linux::plugin_windows::PluginWindowGuard;
#[cfg(all(target_family = "unix", not(target_os = "macos")))]
pub use linux::router::XdndDropRouter;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// Raw native window context required by platform drag launchers.
#[derive(Clone, Copy, Debug)]
pub struct DragWindow {
    display: RawDisplayHandle,
    window: RawWindowHandle,
}

impl DragWindow {
    /// Build a drag window context from raw handles.
    #[must_use]
    pub const fn new(display: RawDisplayHandle, window: RawWindowHandle) -> Self {
        Self { display, window }
    }

    /// Build a drag window context from a toolkit window.
    #[must_use]
    pub fn from_window<W>(window: &W) -> Self
    where
        W: HasRawDisplayHandle + HasRawWindowHandle,
    {
        Self::new(window.raw_display_handle(), window.raw_window_handle())
    }

    /// Raw display handle.
    #[must_use]
    pub const fn display(&self) -> RawDisplayHandle {
        self.display
    }

    /// Raw window handle.
    #[must_use]
    pub const fn window(&self) -> RawWindowHandle {
        self.window
    }

    /// Endpoint kind inferred from the raw window handle.
    #[must_use]
    pub const fn endpoint(&self) -> DragEndpointKind {
        match self.window {
            RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_) => DragEndpointKind::XwaylandWindow,
            RawWindowHandle::Wayland(_) => DragEndpointKind::WaylandSurface,
            RawWindowHandle::AppKit(_) => DragEndpointKind::Unknown,
            RawWindowHandle::Win32(_) | RawWindowHandle::WinRt(_) => DragEndpointKind::Unknown,
            _ => DragEndpointKind::Unknown,
        }
    }

    /// Backend kind inferred from the raw window handle.
    #[must_use]
    pub const fn backend_kind(&self) -> DragBackendKind {
        match self.window {
            RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_) => DragBackendKind::X11Xdnd,
            RawWindowHandle::Wayland(_) => DragBackendKind::WaylandDataDevice,
            RawWindowHandle::AppKit(_) => DragBackendKind::MacosAppKit,
            RawWindowHandle::Win32(_) | RawWindowHandle::WinRt(_) => DragBackendKind::WindowsOle,
            _ => DragBackendKind::Unsupported,
        }
    }

    /// Route inferred for this source window.
    #[must_use]
    pub const fn source_route(&self) -> DragRoute {
        match self.window {
            RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_) => DragRoute::XwaylandToXwayland,
            RawWindowHandle::Wayland(_) => DragRoute::WaylandToWayland,
            _ => DragRoute::Unsupported,
        }
    }
}

/// Successful backend start information.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackendStart {
    pub drag_id: u64,
    pub backend: DragBackendKind,
    pub route: DragRoute,
    pub file_count: usize,
}

/// Backend drag lifecycle phase used by toolkit adapters to gate re-entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExternalDragLifecyclePhase {
    Started,
    DataRequested,
    Dropped,
    Lingering,
    Finished,
    Cancelled,
    Failed,
}

impl ExternalDragLifecyclePhase {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Finished | Self::Cancelled | Self::Failed)
    }
}

/// Typed backend drag lifecycle event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExternalDragLifecycleEvent {
    pub drag_id: u64,
    pub phase: ExternalDragLifecyclePhase,
}

impl ExternalDragLifecycleEvent {
    #[must_use]
    pub const fn new(drag_id: u64, phase: ExternalDragLifecyclePhase) -> Self {
        Self { drag_id, phase }
    }
}

/// Error returned by a native drag backend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExternalDragError {
    EmptyPayload,
    UnsupportedBackend {
        backend: DragBackendKind,
        window: String,
    },
    MissingWindowHandle(&'static str),
    BackendUnavailable(String),
    StartFailed(String),
}

impl std::fmt::Display for ExternalDragError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyPayload => formatter.write_str("no files to drag"),
            Self::UnsupportedBackend { backend, window } => {
                write!(
                    formatter,
                    "external file drag is not implemented for {} from {window}",
                    backend.summary()
                )
            }
            Self::MissingWindowHandle(message) => formatter.write_str(message),
            Self::BackendUnavailable(message) | Self::StartFailed(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl std::error::Error for ExternalDragError {}

impl From<String> for ExternalDragError {
    fn from(message: String) -> Self {
        Self::StartFailed(message)
    }
}

/// Native backend interface implemented by toolkit adapters.
pub trait ExternalDragBackend {
    /// Start an external file drag.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload is invalid, the backend is unsupported,
    /// or the native platform refuses to start a drag.
    fn start_file_drag(
        &mut self,
        window: DragWindow,
        payload: ExternalDragPayload,
    ) -> Result<BackendStart, ExternalDragError>;
}

/// Raw-window-handle backend dispatcher.
///
/// This is the public adapter entry point. It validates payloads and dispatches
/// to target-specific native launchers as they are added.
#[derive(Clone, Debug, Default)]
pub struct RawWindowBackend;

impl ExternalDragBackend for RawWindowBackend {
    fn start_file_drag(
        &mut self,
        window: DragWindow,
        payload: ExternalDragPayload,
    ) -> Result<BackendStart, ExternalDragError> {
        start_file_drag(window, payload)
    }
}

/// Start an external file drag through the raw-window backend.
///
/// # Errors
///
/// Returns an error when the source window backend is unsupported or the native
/// platform launcher cannot be started.
pub fn start_file_drag(
    window: DragWindow,
    payload: ExternalDragPayload,
) -> Result<BackendStart, ExternalDragError> {
    let drag_id = payload.id;
    let file_payload = FileDragPayloadData::new(payload.paths.clone())
        .map_err(|_| ExternalDragError::EmptyPayload)?;
    let file_count = file_payload.paths().len();
    let backend = window.backend_kind();
    let route = window.source_route();
    emit_backend_event(format!(
        "[dnd#{}] backend request: backend={}, route={}, files={}, offers={}",
        drag_id,
        backend.as_str(),
        route.summary(),
        file_count,
        file_payload.offer_count()
    ));

    if let Err(err) = platform_start_file_drag(window, payload) {
        emit_backend_lifecycle_event(ExternalDragLifecycleEvent::new(
            drag_id,
            ExternalDragLifecyclePhase::Failed,
        ));
        return Err(err);
    }
    emit_backend_lifecycle_event(ExternalDragLifecycleEvent::new(
        drag_id,
        ExternalDragLifecyclePhase::Started,
    ));

    Ok(BackendStart {
        drag_id,
        backend,
        route,
        file_count,
    })
}

#[cfg(all(target_family = "unix", not(target_os = "macos")))]
fn platform_start_file_drag(
    window: DragWindow,
    payload: ExternalDragPayload,
) -> Result<(), ExternalDragError> {
    linux::start_external_file_drag(window, payload)
}

#[cfg(target_os = "windows")]
fn platform_start_file_drag(
    window: DragWindow,
    payload: ExternalDragPayload,
) -> Result<(), ExternalDragError> {
    windows::start_external_file_drag(window, payload)
}

#[cfg(target_os = "macos")]
fn platform_start_file_drag(
    window: DragWindow,
    payload: ExternalDragPayload,
) -> Result<(), ExternalDragError> {
    macos::start_external_file_drag(window, payload)
}

#[cfg(not(any(
    all(target_family = "unix", not(target_os = "macos")),
    target_os = "windows",
    target_os = "macos"
)))]
fn platform_start_file_drag(
    window: DragWindow,
    _payload: ExternalDragPayload,
) -> Result<(), ExternalDragError> {
    Err(ExternalDragError::UnsupportedBackend {
        backend: window.backend_kind(),
        window: format!("{:?}", window.window()),
    })
}

struct BackendEventBus {
    sender: Sender<String>,
    receiver: Mutex<Receiver<String>>,
}

struct BackendLifecycleBus {
    sender: Sender<ExternalDragLifecycleEvent>,
    receiver: Mutex<Receiver<ExternalDragLifecycleEvent>>,
}

struct RoutedDragLifecycle {
    phases: Mutex<HashMap<u64, ExternalDragLifecyclePhase>>,
}

fn routed_drag_lifecycle() -> &'static RoutedDragLifecycle {
    static ROUTED: OnceLock<RoutedDragLifecycle> = OnceLock::new();
    ROUTED.get_or_init(|| RoutedDragLifecycle {
        phases: Mutex::new(HashMap::new()),
    })
}

fn backend_event_bus() -> &'static BackendEventBus {
    static BUS: OnceLock<BackendEventBus> = OnceLock::new();
    BUS.get_or_init(|| {
        let (sender, receiver) = channel();
        BackendEventBus {
            sender,
            receiver: Mutex::new(receiver),
        }
    })
}

fn backend_lifecycle_bus() -> &'static BackendLifecycleBus {
    static BUS: OnceLock<BackendLifecycleBus> = OnceLock::new();
    BUS.get_or_init(|| {
        let (sender, receiver) = channel();
        BackendLifecycleBus {
            sender,
            receiver: Mutex::new(receiver),
        }
    })
}

/// Emit a backend diagnostic event.
pub fn emit_backend_event(message: impl Into<String>) {
    let _ = backend_event_bus().sender.send(message.into());
}

/// Emit a typed backend lifecycle event.
pub fn emit_backend_lifecycle_event(event: ExternalDragLifecycleEvent) {
    if event.phase.is_terminal() || event.phase == ExternalDragLifecyclePhase::Lingering {
        if let Ok(mut phases) = routed_drag_lifecycle().phases.lock() {
            phases.insert(event.drag_id, event.phase);
        }
    }
    let _ = backend_lifecycle_bus().sender.send(event);
}

/// Remove and return a routed terminal or lingering lifecycle phase for one drag id.
///
/// Unlike [`drain_backend_lifecycle_events`], this cannot be stolen by another window
/// that does not own the drag id.
#[must_use]
pub fn take_drag_terminal(drag_id: u64) -> Option<ExternalDragLifecyclePhase> {
    let Ok(mut phases) = routed_drag_lifecycle().phases.lock() else {
        return None;
    };
    match phases.get(&drag_id) {
        Some(phase) if phase.is_terminal() || *phase == ExternalDragLifecyclePhase::Lingering => {
            phases.remove(&drag_id)
        }
        _ => None,
    }
}

/// True when a routed terminal or lingering lifecycle phase is waiting for `drag_id`.
#[must_use]
pub fn has_routed_drag_lifecycle(drag_id: u64) -> bool {
    routed_drag_lifecycle()
        .phases
        .lock()
        .ok()
        .is_some_and(|phases| phases.contains_key(&drag_id))
}

/// True while the platform backend still has an outbound drag worker for `drag_id`.
#[must_use]
pub fn is_drag_active(drag_id: u64) -> bool {
    platform_is_drag_active(drag_id)
}

#[cfg(all(target_family = "unix", not(target_os = "macos")))]
fn platform_is_drag_active(drag_id: u64) -> bool {
    linux::is_drag_active(drag_id)
}

#[cfg(not(all(target_family = "unix", not(target_os = "macos"))))]
fn platform_is_drag_active(_drag_id: u64) -> bool {
    false
}

/// Drain backend diagnostic events.
#[must_use]
pub fn drain_backend_events() -> Vec<String> {
    let Ok(receiver) = backend_event_bus().receiver.lock() else {
        return Vec::new();
    };
    let mut events = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        events.push(event);
    }
    events
}

/// Drain typed backend lifecycle events.
#[must_use]
pub fn drain_backend_lifecycle_events() -> Vec<ExternalDragLifecycleEvent> {
    let Ok(receiver) = backend_lifecycle_bus().receiver.lock() else {
        return Vec::new();
    };
    let mut events = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        events.push(event);
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use raw_window_handle::{RawDisplayHandle, XcbWindowHandle, XlibDisplayHandle};

    #[test]
    fn infers_xwayland_backend_from_xcb_window() {
        let mut handle = XcbWindowHandle::empty();
        handle.window = 42;
        let window = DragWindow::new(
            RawDisplayHandle::Xlib(XlibDisplayHandle::empty()),
            RawWindowHandle::Xcb(handle),
        );

        assert_eq!(window.backend_kind(), DragBackendKind::X11Xdnd);
        assert_eq!(window.source_route(), DragRoute::XwaylandToXwayland);
    }

    #[test]
    fn rejects_empty_payload_before_backend_dispatch() {
        let mut handle = XcbWindowHandle::empty();
        handle.window = 42;
        let window = DragWindow::new(
            RawDisplayHandle::Xlib(XlibDisplayHandle::empty()),
            RawWindowHandle::Xcb(handle),
        );
        let payload = ExternalDragPayload {
            id: 1,
            paths: Vec::new(),
            preview: None,
        };

        let err = start_file_drag(window, payload).expect_err("empty payload should fail");

        assert_eq!(err, ExternalDragError::EmptyPayload);
    }

    #[test]
    fn routed_terminal_survives_competing_bus_drain() {
        emit_backend_lifecycle_event(ExternalDragLifecycleEvent::new(
            11,
            ExternalDragLifecyclePhase::Finished,
        ));
        emit_backend_lifecycle_event(ExternalDragLifecycleEvent::new(
            22,
            ExternalDragLifecyclePhase::Cancelled,
        ));

        assert_eq!(
            drain_backend_lifecycle_events(),
            vec![
                ExternalDragLifecycleEvent::new(11, ExternalDragLifecyclePhase::Finished),
                ExternalDragLifecycleEvent::new(22, ExternalDragLifecyclePhase::Cancelled),
            ]
        );

        assert_eq!(
            take_drag_terminal(11),
            Some(ExternalDragLifecyclePhase::Finished)
        );
        assert_eq!(
            take_drag_terminal(22),
            Some(ExternalDragLifecyclePhase::Cancelled)
        );
        assert_eq!(take_drag_terminal(11), None);
    }

    #[test]
    fn take_drag_terminal_is_consume_once() {
        emit_backend_lifecycle_event(ExternalDragLifecycleEvent::new(
            5,
            ExternalDragLifecyclePhase::Failed,
        ));

        assert_eq!(
            take_drag_terminal(5),
            Some(ExternalDragLifecyclePhase::Failed)
        );
        assert_eq!(take_drag_terminal(5), None);
        assert!(!has_routed_drag_lifecycle(5));
    }

    #[test]
    fn lifecycle_bus_drains_typed_events_in_order() {
        emit_backend_lifecycle_event(ExternalDragLifecycleEvent::new(
            7,
            ExternalDragLifecyclePhase::Started,
        ));
        emit_backend_lifecycle_event(ExternalDragLifecycleEvent::new(
            7,
            ExternalDragLifecyclePhase::Finished,
        ));

        assert_eq!(
            drain_backend_lifecycle_events(),
            vec![
                ExternalDragLifecycleEvent::new(7, ExternalDragLifecyclePhase::Started),
                ExternalDragLifecycleEvent::new(7, ExternalDragLifecyclePhase::Finished),
            ]
        );
        assert!(drain_backend_lifecycle_events().is_empty());
    }
}
