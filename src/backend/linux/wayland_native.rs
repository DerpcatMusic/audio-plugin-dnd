//! Serial-less native Wayland drag route for XWayland plugin editors.
//!
//! `wl_data_device.start_drag` normally requires the implicit-grab serial of
//! the button press on the origin surface. A plugin embedded in an XWayland
//! host can never obtain one: the press happened on the host's XWayland
//! surface, whose Wayland client is the Xwayland server itself.
//!
//! Hyprland, however, ignores both the serial and the origin surface in its
//! `startDrag` handler. Drag focus is picked from the physical pointer, and
//! the drop fires on the physical button release — which is exactly the state
//! the user is in mid-gesture. That lets an XWayland editor open its own
//! side connection to the compositor and start a fully native drag:
//!
//! - Native Wayland targets receive a normal `wl_data_device` drag.
//! - X11/XWayland targets (including the host DAW) receive the compositor's
//!   own Wayland-to-X11 XDND bridge, which stock Hyprland implements.
//!
//! Compositors with strict serial validation (KWin, Mutter, wlroots) refuse
//! this route, but they also bridge X11-source drags natively, so the XDND
//! backend remains the right path there. The route is therefore only
//! attempted when the compositor is known to accept serial-less drags, and
//! any rejection falls back to the XDND source.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use smithay_client_toolkit::{
    data_device_manager::{
        data_device::DataDeviceHandler,
        data_offer::{DataOfferHandler, DragOffer},
        data_source::DataSourceHandler,
        DataDeviceManagerState, WritePipe,
    },
    delegate_data_device, delegate_seat, delegate_shm,
    reexports::{calloop::EventLoop, calloop_wayland_source::WaylandSource},
    seat::{Capability, SeatHandler, SeatState},
    shm::{
        slot::{Buffer, SlotPool},
        Shm, ShmHandler,
    },
};
use wayland_client::{
    globals::{registry_queue_init, GlobalListContents},
    protocol::{
        wl_compositor::WlCompositor, wl_data_device::WlDataDevice,
        wl_data_device_manager::DndAction, wl_data_source::WlDataSource, wl_registry,
        wl_seat::WlSeat, wl_shm, wl_surface::WlSurface,
    },
    Connection, Dispatch, Proxy, QueueHandle,
};
use x11rb::connection::Connection as X11Connection;
use x11rb::protocol::xproto::ConnectionExt;
use x11rb::protocol::xproto::KeyButMask;
use x11rb::rust_connection::RustConnection;

use super::portal;
use crate::backend::dnd::{DragFailureKind, DragSessionReport, DragSessionStats};
use crate::backend::{
    emit_backend_event, emit_backend_lifecycle_event, ExternalDragLifecycleEvent,
    ExternalDragLifecyclePhase,
};
use crate::data_device::ActiveWaylandDrag;
use crate::preview_render::{
    render_drag_chip_sized, rgba_to_argb8888_premul, CHIP_HEIGHT, CHIP_WIDTH,
};
use crate::request::{WaylandDragOffer, WaylandExternalDragRequest};
use crate::{ExternalDragPreview, FileDragPayloadData};

/// How long a native drag session may stay open overall. Drags end on the
/// physical button release, so this is only a safety net against a compositor
/// that never delivers a terminal event.
const SESSION_DEADLINE: Duration = Duration::from_secs(120);
/// Idle grace after a drop before inferring the outcome from the evidence.
const POST_DROP_QUIET: Duration = Duration::from_millis(1500);
/// Keep the native Wayland client connected after a bridged X11 drop so
/// Hyprland's Wayland-to-X11 cleanup can finish before `wl_client_destroy`.
const BRIDGE_LINGER_TIMEOUT: Duration = Duration::from_secs(4);
/// After pointer release with weak bridge evidence, keep dispatching for
/// more compositor events before treating the session as failed.
const POST_RELEASE_BRIDGE_WAIT: Duration = Duration::from_secs(4);
/// A cancellation this early, with zero target interaction, means the
/// compositor refused the serial-less drag rather than the user missing.
const EARLY_REJECT_WINDOW: Duration = Duration::from_millis(1000);

const MIME_PORTAL_FILETRANSFER: &str = "application/vnd.portal.filetransfer";
const MIME_PORTAL_FILES: &str = "application/vnd.portal.files";

/// Why the native route could not complete; callers fall back to XDND.
#[derive(Debug)]
pub(super) enum NativeDragError {
    /// Setup failed before the drag started (no display, no globals, ...).
    Unavailable(String),
    /// The compositor cancelled the drag immediately: serial validation.
    Rejected(String),
}

impl std::fmt::Display for NativeDragError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(detail) => write!(f, "native Wayland route unavailable: {detail}"),
            Self::Rejected(detail) => {
                write!(f, "compositor rejected serial-less native drag: {detail}")
            }
        }
    }
}

/// True when the serial-less native drag route should be attempted.
///
/// `AUDIO_PLUGIN_DND_NATIVE_WAYLAND=1|0` force-enables or disables the route.
/// By default it is only used on Hyprland, whose `startDrag` handler ignores
/// the pointer-grab serial and the origin surface.
pub(super) fn route_enabled() -> bool {
    if let Ok(value) = std::env::var("AUDIO_PLUGIN_DND_NATIVE_WAYLAND") {
        return matches!(
            value.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        );
    }

    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return false;
    }

    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
        || std::env::var("XDG_CURRENT_DESKTOP")
            .map(|desktop| desktop.to_ascii_lowercase().contains("hyprland"))
            .unwrap_or(false)
}

pub(super) fn run_native_drag(
    drag_id: u64,
    paths: Vec<PathBuf>,
    preview: Option<ExternalDragPreview>,
) -> Result<DragSessionReport, NativeDragError> {
    let payload = FileDragPayloadData::new(paths.clone()).map_err(NativeDragError::Unavailable)?;

    let mut offers = Vec::new();
    match portal::start_file_transfer(&paths) {
        Ok(transfer) => {
            emit_backend_event(format!(
                "[dnd#{drag_id}] Portal FileTransfer ready via {}: {} file(s), key {} bytes",
                transfer.backend,
                paths.len(),
                transfer.key.len()
            ));
            offers.push(WaylandDragOffer::new(
                MIME_PORTAL_FILETRANSFER,
                transfer.key.clone(),
            ));
            offers.push(WaylandDragOffer::new(MIME_PORTAL_FILES, transfer.key));
        }
        Err(err) => {
            emit_backend_event(format!(
                "[dnd#{drag_id}] Portal FileTransfer unavailable: {err}"
            ));
        }
    }
    offers.extend(payload.wayland_offers());

    let connection = Connection::connect_to_env()
        .map_err(|err| NativeDragError::Unavailable(format!("connect: {err}")))?;
    let (globals, event_queue) = registry_queue_init::<NativeDragState>(&connection)
        .map_err(|err| NativeDragError::Unavailable(format!("registry: {err}")))?;
    let queue_handle = event_queue.handle();

    let compositor: WlCompositor = globals
        .bind(&queue_handle, 1..=6, ())
        .map_err(|err| NativeDragError::Unavailable(format!("wl_compositor: {err}")))?;
    // Hyprland never reads the origin surface, so a bare, role-less,
    // never-committed surface satisfies the protocol argument.
    let origin_surface = compositor.create_surface(&queue_handle, ());

    let manager = DataDeviceManagerState::bind(&globals, &queue_handle)
        .map_err(|err| NativeDragError::Unavailable(format!("wl_data_device_manager: {err}")))?;
    let shm = Shm::bind(&globals, &queue_handle)
        .map_err(|err| NativeDragError::Unavailable(format!("wl_shm: {err}")))?;
    let seat_state = SeatState::new(&globals, &queue_handle);
    let Some(seat) = seat_state.seats().next() else {
        return Err(NativeDragError::Unavailable("no wl_seat".to_string()));
    };
    let data_device = manager.get_data_device(&queue_handle, &seat);

    let request = WaylandExternalDragRequest::new(offers, paths);
    let active = ActiveWaylandDrag::create(&manager, &queue_handle, request)
        .map_err(|err| NativeDragError::Unavailable(err.to_string()))?;
    let icon = preview
        .as_ref()
        .and_then(|preview| NativeDragIcon::create(&compositor, &shm, &queue_handle, preview).ok());
    // Serial 0: Hyprland's startDrag handler never reads it.
    active.start(
        &data_device,
        &origin_surface,
        icon.as_ref().map(|icon| &icon.surface),
        0,
    );
    connection
        .flush()
        .map_err(|err| NativeDragError::Unavailable(format!("flush: {err}")))?;

    emit_backend_event(format!(
        "[dnd#{drag_id}] Native Wayland drag started via serial-less start_drag; \
         native targets get wl_data_device, X11 targets get the compositor bridge"
    ));

    let mut state = NativeDragState {
        drag_id,
        seat_state,
        shm,
        active: Some(active),
        _icon: icon,
        _origin_surface: origin_surface,
        evidence: DragEvidence::new(),
    };

    let mut event_loop: EventLoop<'_, NativeDragState> = EventLoop::try_new()
        .map_err(|err| NativeDragError::Unavailable(format!("event loop: {err}")))?;
    WaylandSource::new(connection.clone(), event_queue)
        .insert(event_loop.handle())
        .map_err(|err| NativeDragError::Unavailable(format!("event source: {err}")))?;

    let started = Instant::now();
    let mut pointer_probe = PointerReleaseProbe::new();
    let mut pointer_release_at: Option<Instant> = None;
    loop {
        event_loop
            .dispatch(Duration::from_millis(50), &mut state)
            .map_err(|err| NativeDragError::Unavailable(format!("dispatch: {err}")))?;

        if pointer_probe.poll_released() {
            state.evidence.note_pointer_released();
            pointer_release_at.get_or_insert_with(Instant::now);
        }

        let evidence = &state.evidence;
        if evidence.finished || evidence.cancelled {
            break;
        }
        if evidence.drop_performed
            && evidence
                .last_activity
                .is_some_and(|at| at.elapsed() > POST_DROP_QUIET)
        {
            break;
        }
        if bridge_transfer_ready(evidence)
            && evidence.pointer_released
            && evidence
                .last_activity
                .is_some_and(|at| at.elapsed() > POST_DROP_QUIET)
        {
            break;
        }
        if evidence.pointer_released
            && !bridge_transfer_ready(evidence)
            && pointer_release_at.is_some_and(|at| at.elapsed() > POST_RELEASE_BRIDGE_WAIT)
        {
            break;
        }
        if started.elapsed() > SESSION_DEADLINE {
            break;
        }
    }

    if state.evidence.cancelled
        && !state.evidence.drop_performed
        && !state.evidence.target_interacted
        && state.evidence.send_requests == 0
        && started.elapsed() < EARLY_REJECT_WINDOW
    {
        teardown_native_session(drag_id, &connection, &mut state);
        return Err(NativeDragError::Rejected(format!(
            "cancelled {}ms after start with no target interaction",
            started.elapsed().as_millis()
        )));
    }

    let linger = needs_bridge_linger(&state.evidence);
    let report = state.evidence.clone().into_report();

    if linger {
        linger_and_teardown(drag_id, connection, event_loop, &mut state);
    } else {
        teardown_native_session(drag_id, &connection, &mut state);
    }

    Ok(report)
}

/// True when the compositor bridge has enough evidence to treat the transfer
/// as complete (or terminal events already arrived).
fn bridge_transfer_ready(evidence: &DragEvidence) -> bool {
    if evidence.finished || evidence.drop_performed {
        return true;
    }
    evidence.pointer_released && (evidence.send_requests >= 2 || evidence.accept_mime_events >= 1)
}

/// True when the compositor bridge may still hold drag state after we inferred
/// success but never delivered a terminal `dnd_finished` event.
fn needs_bridge_linger(evidence: &DragEvidence) -> bool {
    if evidence.finished || evidence.cancelled {
        return false;
    }
    if evidence.drop_performed {
        return true;
    }
    evidence.pointer_released && (evidence.send_requests >= 2 || evidence.accept_mime_events >= 1)
}

fn linger_and_teardown(
    drag_id: u64,
    connection: Connection,
    mut event_loop: EventLoop<'_, NativeDragState>,
    state: &mut NativeDragState,
) {
    emit_backend_lifecycle_event(ExternalDragLifecycleEvent::new(
        drag_id,
        ExternalDragLifecyclePhase::Lingering,
    ));
    // Tear down the drag surface and clear in-flight BEFORE the grace wait so the
    // next drag is not blocked for the full linger window.
    teardown_native_session(drag_id, &connection, state);
    crate::backend::linux::clear_drag_active(drag_id);
    emit_backend_lifecycle_event(ExternalDragLifecycleEvent::new(
        drag_id,
        ExternalDragLifecyclePhase::Finished,
    ));
    // #region agent log
    {
        use std::io::Write;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let line = format!(
            "{{\"sessionId\":\"dbdcd7\",\"hypothesisId\":\"H2\",\"runId\":\"post-fix\",\"location\":\"wayland_native.rs:linger_and_teardown\",\"message\":\"finished before linger wait\",\"data\":{{\"drag_id\":{drag_id}}},\"timestamp\":{ts}}}\n"
        );
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/home/derpcat/projects/drop-recorder/.cursor/debug-dbdcd7.log")
        {
            let _ = file.write_all(line.as_bytes());
        }
    }
    // #endregion
    emit_backend_event(format!(
        "[dnd#{drag_id}] Native Wayland bridge linger started (up to {}ms)",
        BRIDGE_LINGER_TIMEOUT.as_millis()
    ));
    let deadline = Instant::now() + BRIDGE_LINGER_TIMEOUT;
    while Instant::now() < deadline {
        if state.evidence.finished || state.evidence.cancelled {
            break;
        }
        let timeout = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(50));
        if event_loop.dispatch(timeout, state).is_err() {
            break;
        }
    }
    let reason = if state.evidence.finished {
        "dnd_finished"
    } else if state.evidence.cancelled {
        "cancelled"
    } else {
        "grace_timeout"
    };
    emit_backend_event(format!(
        "[dnd#{drag_id}] Native Wayland bridge linger ending: {reason}"
    ));
    // #region agent log
    {
        use std::io::Write;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let line = format!(
            "{{\"sessionId\":\"dbdcd7\",\"hypothesisId\":\"H1\",\"runId\":\"post-fix\",\"location\":\"wayland_native.rs:linger_and_teardown\",\"message\":\"linger wait ended\",\"data\":{{\"drag_id\":{drag_id},\"reason\":\"{reason}\"}},\"timestamp\":{ts}}}\n"
        );
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/home/derpcat/projects/drop-recorder/.cursor/debug-dbdcd7.log")
        {
            let _ = file.write_all(line.as_bytes());
        }
    }
    // #endregion
}

fn teardown_native_session(drag_id: u64, connection: &Connection, state: &mut NativeDragState) {
    state._icon = None;
    if let Some(active) = state.active.take() {
        active.destroy();
    }
    if let Err(err) = connection.flush() {
        emit_backend_event(format!(
            "[dnd#{drag_id}] Native Wayland session flush failed: {err}"
        ));
    }
    emit_backend_event(format!(
        "[dnd#{drag_id}] Native Wayland session disconnected"
    ));
}

#[derive(Debug, Clone)]
struct DragEvidence {
    target_interacted: bool,
    accepted_mime: Option<String>,
    send_requests: usize,
    accept_mime_events: usize,
    post_drop_send_requests: usize,
    drop_performed: bool,
    pointer_released: bool,
    finished: bool,
    cancelled: bool,
    last_activity: Option<Instant>,
}

impl DragEvidence {
    fn new() -> Self {
        Self {
            target_interacted: false,
            accepted_mime: None,
            send_requests: 0,
            accept_mime_events: 0,
            post_drop_send_requests: 0,
            drop_performed: false,
            pointer_released: false,
            finished: false,
            cancelled: false,
            last_activity: None,
        }
    }

    fn touch(&mut self) {
        self.last_activity = Some(Instant::now());
    }

    fn note_pointer_released(&mut self) {
        if self.pointer_released {
            return;
        }
        self.pointer_released = true;
        self.touch();
    }

    fn stats(&self) -> DragSessionStats {
        DragSessionStats {
            selection_requests: self.send_requests as u32,
            pre_drop_data_requests: self
                .send_requests
                .saturating_sub(self.post_drop_send_requests)
                as u32,
            post_drop_data_requests: self.post_drop_send_requests as u32,
            drop_target_data_requests: self.post_drop_send_requests as u32,
        }
    }

    fn into_report(self) -> DragSessionReport {
        let stats = self.stats();
        if self.finished {
            return DragSessionReport::completed_confirmed(
                stats,
                "target finished the native Wayland drag",
            );
        }
        if self.drop_performed && self.send_requests > 0 {
            return DragSessionReport::completed_confirmed(
                stats,
                "drop performed and target requested file data",
            );
        }
        if self.pointer_released
            && self.send_requests > 0
            && (self.send_requests >= 2 || self.accept_mime_events >= 1)
        {
            // X11 targets fetch data through the compositor bridge after the
            // physical button release, often without a terminal source event.
            return DragSessionReport::completed_inferred(
                stats,
                "target requested file data through the compositor bridge after release",
            );
        }
        if self.drop_performed {
            return DragSessionReport::completed_inferred(
                stats,
                "compositor reported the drop but no data was requested yet",
            );
        }
        if self.cancelled {
            return DragSessionReport::failed(
                DragFailureKind::Cancelled,
                stats,
                "compositor cancelled the native drag (released outside a target or target refused)",
            );
        }
        DragSessionReport::failed(
            DragFailureKind::TargetNoData,
            stats,
            "native drag session ended without a drop or data request",
        )
    }
}

struct NativeDragState {
    drag_id: u64,
    seat_state: SeatState,
    shm: Shm,
    active: Option<ActiveWaylandDrag>,
    _icon: Option<NativeDragIcon>,
    _origin_surface: WlSurface,
    evidence: DragEvidence,
}

impl NativeDragState {
    fn log(&self, message: impl AsRef<str>) {
        emit_backend_event(format!("[dnd#{}] {}", self.drag_id, message.as_ref()));
    }
}

struct PointerReleaseProbe {
    conn: Option<RustConnection>,
    root: x11rb::protocol::xproto::Window,
    saw_primary_down: bool,
    released: bool,
}

impl PointerReleaseProbe {
    fn new() -> Self {
        let Ok((conn, screen_num)) = RustConnection::connect(None) else {
            return Self::unavailable();
        };
        let Some(screen) = conn.setup().roots.get(screen_num) else {
            return Self::unavailable();
        };
        Self {
            root: screen.root,
            conn: Some(conn),
            saw_primary_down: false,
            released: false,
        }
    }

    fn unavailable() -> Self {
        Self {
            conn: None,
            root: x11rb::NONE,
            saw_primary_down: false,
            released: false,
        }
    }

    fn poll_released(&mut self) -> bool {
        if self.released {
            return true;
        }
        let Some(conn) = self.conn.as_ref() else {
            return false;
        };
        let Ok(cookie) = conn.query_pointer(self.root) else {
            return false;
        };
        let Ok(pointer) = cookie.reply() else {
            return false;
        };
        let primary_down = pointer.mask.contains(KeyButMask::BUTTON1);
        self.saw_primary_down |= primary_down;
        if self.saw_primary_down && !primary_down {
            self.released = true;
        }
        self.released
    }
}

struct NativeDragIcon {
    surface: WlSurface,
    _pool: SlotPool,
    _buffer: Buffer,
}

impl NativeDragIcon {
    const WIDTH: i32 = CHIP_WIDTH as i32;
    const HEIGHT: i32 = CHIP_HEIGHT as i32;
    const STRIDE: i32 = Self::WIDTH * 4;

    fn create<State>(
        compositor: &WlCompositor,
        shm: &Shm,
        queue: &QueueHandle<State>,
        preview: &ExternalDragPreview,
    ) -> Result<Self, String>
    where
        State: Dispatch<wl_shm::WlShm, smithay_client_toolkit::globals::GlobalData> + 'static,
        State: Dispatch<WlSurface, ()> + 'static,
    {
        let surface = compositor.create_surface(queue, ());
        let mut pool = SlotPool::new((Self::STRIDE * Self::HEIGHT) as usize, shm)
            .map_err(|err| err.to_string())?;
        let (buffer, canvas) = pool
            .create_buffer(
                Self::WIDTH,
                Self::HEIGHT,
                Self::STRIDE,
                wl_shm::Format::Argb8888,
            )
            .map_err(|err| err.to_string())?;
        let image = render_drag_chip_sized(preview, CHIP_WIDTH, CHIP_HEIGHT);
        let packed = rgba_to_argb8888_premul(&image.rgba);
        let copy_len = canvas.len().min(packed.len());
        canvas[..copy_len].copy_from_slice(&packed[..copy_len]);
        surface.attach(Some(buffer.wl_buffer()), 0, 0);
        surface.damage_buffer(0, 0, Self::WIDTH, Self::HEIGHT);
        surface.commit();
        Ok(Self {
            surface,
            _pool: pool,
            _buffer: buffer,
        })
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for NativeDragState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: <wl_registry::WlRegistry as Proxy>::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlCompositor, ()> for NativeDragState {
    fn event(
        _state: &mut Self,
        _proxy: &WlCompositor,
        _event: <WlCompositor as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSurface, ()> for NativeDragState {
    fn event(
        _state: &mut Self,
        _proxy: &WlSurface,
        _event: <WlSurface as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
    }
}

impl SeatHandler for NativeDragState {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: WlSeat,
        _capability: Capability,
    ) {
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: WlSeat,
        _capability: Capability,
    ) {
    }

    fn remove_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: WlSeat) {}
}

impl ShmHandler for NativeDragState {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl DataDeviceHandler for NativeDragState {
    fn enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _data_device: &WlDataDevice,
        _x: f64,
        _y: f64,
        _surface: &WlSurface,
    ) {
    }

    fn leave(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _data_device: &WlDataDevice) {}

    fn motion(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _data_device: &WlDataDevice,
        _x: f64,
        _y: f64,
    ) {
    }

    fn selection(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _device: &WlDataDevice) {}

    fn drop_performed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _data_device: &WlDataDevice,
    ) {
    }
}

impl DataOfferHandler for NativeDragState {
    fn source_actions(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        offer: &mut DragOffer,
        actions: DndAction,
    ) {
        offer.set_actions(actions, DndAction::Copy);
    }

    fn selected_action(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _offer: &mut DragOffer,
        _actions: DndAction,
    ) {
    }
}

impl DataSourceHandler for NativeDragState {
    fn accept_mime(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        source: &WlDataSource,
        mime: Option<String>,
    ) {
        self.evidence.target_interacted = true;
        self.evidence.accept_mime_events += 1;
        self.evidence.touch();
        if let Some(mime) = &mime {
            if self.evidence.accepted_mime.as_deref() != Some(mime.as_str()) {
                self.log(format!("Native drag target accepted mime {mime}"));
            }
            self.evidence.accepted_mime = Some(mime.clone());
        }
        if let Some(active) = self.active.as_mut() {
            if active.matches_source(source) {
                active.note_accepted_mime(mime);
            }
        }
    }

    fn send_request(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        source: &WlDataSource,
        mime: String,
        pipe: WritePipe,
    ) {
        self.evidence.target_interacted = true;
        self.evidence.send_requests += 1;
        if self.evidence.drop_performed {
            self.evidence.post_drop_send_requests += 1;
        }
        self.evidence.touch();
        self.log(format!("Native drag data requested as {mime}"));
        emit_backend_lifecycle_event(ExternalDragLifecycleEvent::new(
            self.drag_id,
            ExternalDragLifecyclePhase::DataRequested,
        ));
        if let Some(active) = self.active.as_mut() {
            if active.matches_source(source) {
                let _ = active.write_payload(&mime, pipe);
            }
        }
    }

    fn cancelled(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, source: &WlDataSource) {
        self.evidence.cancelled = true;
        self.evidence.touch();
        if let Some(active) = self.active.take() {
            if active.matches_source(source) {
                active.destroy();
            } else {
                self.active = Some(active);
            }
        }
    }

    fn dnd_dropped(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, source: &WlDataSource) {
        self.evidence.drop_performed = true;
        self.evidence.touch();
        self.log("Native drag drop performed");
        emit_backend_lifecycle_event(ExternalDragLifecycleEvent::new(
            self.drag_id,
            ExternalDragLifecyclePhase::Dropped,
        ));
        if let Some(active) = self.active.as_mut() {
            if active.matches_source(source) {
                active.note_drop_performed();
            }
        }
    }

    fn dnd_finished(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, source: &WlDataSource) {
        self.evidence.finished = true;
        self.evidence.touch();
        if let Some(active) = self.active.take() {
            if active.matches_source(source) {
                let mut active = active;
                active.note_finished();
                active.destroy();
            } else {
                self.active = Some(active);
            }
        }
    }

    fn action(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _source: &WlDataSource,
        _action: DndAction,
    ) {
    }
}

delegate_seat!(NativeDragState);
delegate_data_device!(NativeDragState);
delegate_shm!(NativeDragState);

#[cfg(test)]
mod tests {
    use super::*;

    fn bridge_drop_evidence() -> DragEvidence {
        DragEvidence {
            target_interacted: true,
            send_requests: 1,
            post_drop_send_requests: 1,
            drop_performed: true,
            pointer_released: true,
            ..DragEvidence::new()
        }
    }

    #[test]
    fn hover_data_before_release_is_not_bridge_success() {
        let evidence = DragEvidence {
            target_interacted: true,
            send_requests: 1,
            post_drop_send_requests: 0,
            ..DragEvidence::new()
        };
        let report = evidence.into_report();
        assert!(matches!(
            report.completion,
            crate::backend::dnd::DragCompletion::Failed(_)
        ));
        assert!(!bridge_transfer_ready(&DragEvidence {
            target_interacted: true,
            send_requests: 1,
            ..DragEvidence::new()
        }));
    }

    #[test]
    fn single_hover_probe_after_release_is_not_success() {
        let evidence = DragEvidence {
            target_interacted: true,
            send_requests: 1,
            pointer_released: true,
            ..DragEvidence::new()
        };
        assert!(!bridge_transfer_ready(&evidence));
        let report = evidence.into_report();
        assert!(matches!(
            report.completion,
            crate::backend::dnd::DragCompletion::Failed(
                crate::backend::dnd::DragFailureKind::TargetNoData
            )
        ));
    }

    #[test]
    fn bridge_transfer_ready_for_strong_post_release_evidence() {
        let evidence = DragEvidence {
            send_requests: 2,
            pointer_released: true,
            ..DragEvidence::new()
        };
        assert!(bridge_transfer_ready(&evidence));
        let report = evidence.into_report();
        assert!(report.is_success());
    }

    #[test]
    fn bridge_transfer_ready_when_mime_accepted() {
        let evidence = DragEvidence {
            send_requests: 1,
            accept_mime_events: 1,
            pointer_released: true,
            ..DragEvidence::new()
        };
        assert!(bridge_transfer_ready(&evidence));
    }

    #[test]
    fn bridge_linger_needed_for_inferred_bridge_success() {
        assert!(needs_bridge_linger(&bridge_drop_evidence()));
    }

    #[test]
    fn bridge_linger_not_needed_for_weak_probe() {
        let evidence = DragEvidence {
            send_requests: 1,
            pointer_released: true,
            ..DragEvidence::new()
        };
        assert!(!needs_bridge_linger(&evidence));
    }

    #[test]
    fn bridge_linger_not_needed_after_dnd_finished() {
        let mut evidence = bridge_drop_evidence();
        evidence.finished = true;
        assert!(!needs_bridge_linger(&evidence));
    }

    #[test]
    fn bridge_linger_not_needed_after_cancel() {
        let mut evidence = bridge_drop_evidence();
        evidence.cancelled = true;
        assert!(!needs_bridge_linger(&evidence));
    }

    #[test]
    fn bridge_linger_not_needed_without_bridge_activity() {
        assert!(!needs_bridge_linger(&DragEvidence::new()));
    }
}
