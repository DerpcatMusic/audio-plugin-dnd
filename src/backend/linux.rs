use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

mod mime;
mod portal;
pub(super) mod router;
mod target;
mod wayland_native;

use super::dnd::{
    DragCompletion, DragFailureKind, DragPhase, DragSessionReport, DragSessionStats, DragTargetKind,
};
use super::{
    emit_backend_event, emit_backend_lifecycle_event, DragWindow, ExternalDragError,
    ExternalDragLifecycleEvent, ExternalDragLifecyclePhase,
};
use crate::platform::{DragBackendPlan, DragEndpointKind, DragRoute};
use crate::{ExternalDragPayload, ExternalDragPreview, FileDragPayloadData};
use mime::MimeTargets;
use raw_window_handle::RawWindowHandle;
use target::RecentRealTarget;
use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as RandrConnectionExt;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ButtonPressEvent, ButtonReleaseEvent, ClientMessageEvent, ConfigureWindowAux,
    ConnectionExt, CoordMode, CreateGCAux, CreateWindowAux, EventMask, Gcontext, KeyButMask,
    MotionNotifyEvent, Point, PropMode, Rectangle, Screen, Segment, SelectionNotifyEvent,
    SelectionRequestEvent, StackMode, Window as XWindow, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::CURRENT_TIME;

#[allow(unused_macros)]
macro_rules! info {
    ($($arg:tt)*) => {{
        let _ = format_args!($($arg)*);
    }};
}

#[allow(unused_macros)]
macro_rules! warn {
    ($($arg:tt)*) => {{
        let _ = format_args!($($arg)*);
    }};
}

const XDND_VERSION: u32 = 5;
const STATUS_ACCEPT: u32 = 1;
const DROP_FINISH_WAIT: Duration = Duration::from_millis(260);
const DROP_READY_FINISH_WAIT: Duration = Duration::from_millis(180);
const DROP_SELECTION_GRACE: Duration = Duration::from_millis(80);

fn active_outbound_drags() -> &'static Mutex<HashSet<u64>> {
    static ACTIVE: OnceLock<Mutex<HashSet<u64>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashSet::new()))
}

fn outbound_drag_mutex() -> &'static Mutex<()> {
    static MUTEX: OnceLock<Mutex<()>> = OnceLock::new();
    MUTEX.get_or_init(|| Mutex::new(()))
}

struct ActiveOutboundDrag {
    id: u64,
}

impl ActiveOutboundDrag {
    fn register(id: u64) -> Self {
        if let Ok(mut active) = active_outbound_drags().lock() {
            active.insert(id);
        }
        Self { id }
    }
}

impl Drop for ActiveOutboundDrag {
    fn drop(&mut self) {
        if let Ok(mut active) = active_outbound_drags().lock() {
            active.remove(&self.id);
        }
    }
}

pub(super) fn has_active_outbound_drag() -> bool {
    active_outbound_drags()
        .lock()
        .map(|active| !active.is_empty())
        .unwrap_or(false)
}

fn emit_terminal_lifecycle(drag_id: u64, report: &DragSessionReport) {
    let phase = match report.completion {
        DragCompletion::Confirmed | DragCompletion::Inferred => {
            ExternalDragLifecyclePhase::Finished
        }
        DragCompletion::Failed(DragFailureKind::Cancelled) => ExternalDragLifecyclePhase::Cancelled,
        DragCompletion::Failed(_) => ExternalDragLifecyclePhase::Failed,
    };
    emit_backend_lifecycle_event(ExternalDragLifecycleEvent::new(drag_id, phase));
}

pub(super) fn start_external_file_drag(
    window: DragWindow,
    payload: ExternalDragPayload,
) -> Result<(), ExternalDragError> {
    let ExternalDragPayload { id, paths, preview } = payload;
    if paths.is_empty() {
        return Err(ExternalDragError::EmptyPayload);
    }

    let origin_window = match window.window() {
        RawWindowHandle::Xlib(handle) if handle.window != 0 => Some(handle.window as XWindow),
        RawWindowHandle::Xcb(handle) if handle.window != 0 => Some(handle.window as XWindow),
        RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_) => {
            return Err(ExternalDragError::MissingWindowHandle(
                "window does not have a valid X11/XWayland handle",
            ));
        }
        RawWindowHandle::Wayland(_) => return Err(ExternalDragError::BackendUnavailable(
            "native Wayland drag requires a live WaylandRuntime with the initiating pointer serial"
                .to_string(),
        )),
        other => {
            return Err(ExternalDragError::UnsupportedBackend {
                backend: window.backend_kind(),
                window: format!("{other:?}"),
            });
        }
    };

    thread::Builder::new()
        .name("audio-plugin-xdnd-file-drag".to_string())
        .spawn(move || {
            let _active_drag = ActiveOutboundDrag::register(id);
            let _drag_lock = outbound_drag_mutex()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if wayland_native::route_enabled() {
                let route = DragBackendPlan::new(
                    DragRoute::XwaylandToWaylandBridge,
                    DragEndpointKind::XwaylandWindow,
                    DragEndpointKind::WaylandSurface,
                );
                emit_backend_event(format!(
                    "[dnd#{id}] Native Wayland worker started: serial-less start_drag route; {}",
                    route.summary()
                ));
                match wayland_native::run_native_drag(id, paths.clone(), preview.clone()) {
                    Ok(report) if report.is_success() => {
                        emit_backend_event(format!(
                            "[dnd#{id}] Native Wayland drag {}: {}; {}",
                            report.completion,
                            report.summary(),
                            report.stats_summary()
                        ));
                        emit_terminal_lifecycle(id, &report);
                        return;
                    }
                    Ok(report) => {
                        emit_backend_event(format!(
                            "[dnd#{id}] Native Wayland drag {} (weak/failed): {}; {}; falling back to XDND source",
                            report.completion,
                            report.summary(),
                            report.stats_summary()
                        ));
                    }
                    Err(err) => {
                        emit_backend_event(format!(
                            "[dnd#{id}] {err}; falling back to XDND source"
                        ));
                    }
                }
            }

            let route = DragBackendPlan::new(
                DragRoute::XwaylandToXwayland,
                DragEndpointKind::XwaylandWindow,
                DragEndpointKind::Unknown,
            );
            emit_backend_event(format!(
                "[dnd#{id}] XDND worker started: origin={}, route={}",
                origin_window
                    .map(|window| format!("0x{window:x}"))
                    .unwrap_or_else(|| "unknown".to_string()),
                route.summary()
            ));
            match XdndSource::new(id, paths, preview, origin_window).and_then(XdndSource::run) {
                Ok(report) => {
                    emit_backend_event(format!(
                        "[dnd#{id}] XDND {}: {}; {}",
                        report.completion,
                        report.summary(),
                        report.stats_summary()
                    ));
                    emit_terminal_lifecycle(id, &report);
                }
                Err(err) => {
                    emit_backend_event(format!("[dnd#{id}] XDND failed: {err}"));
                    emit_backend_lifecycle_event(ExternalDragLifecycleEvent::new(
                        id,
                        ExternalDragLifecyclePhase::Failed,
                    ));
                }
            }
        })
        .map(|_| ())
        .map_err(|err| ExternalDragError::StartFailed(err.to_string()))
}

struct XdndAtoms {
    xdnd_selection: Atom,
    xdnd_enter: Atom,
    xdnd_leave: Atom,
    xdnd_position: Atom,
    xdnd_status: Atom,
    xdnd_drop: Atom,
    xdnd_finished: Atom,
    xdnd_aware: Atom,
    xdnd_proxy: Atom,
    xdnd_type_list: Atom,
    xdnd_action_copy: Atom,
    net_active_window: Atom,
    text_uri_list: Atom,
    text_uri_list_utf8: Atom,
    text_x_uri: Atom,
    application_vnd_portal_filetransfer: Atom,
    application_vnd_portal_files: Atom,
    application_x_kde4_urilist: Atom,
    x_special_gnome_copied_files: Atom,
    text_plain: Atom,
    text_plain_utf8: Atom,
    targets: Atom,
    timestamp_property: Atom,
    utf8_string: Atom,
    string: Atom,
    wm_class: Atom,
    wm_name: Atom,
    net_wm_name: Atom,
}

impl XdndAtoms {
    fn new(conn: &RustConnection) -> Result<Self, String> {
        Ok(Self {
            xdnd_selection: atom(conn, b"XdndSelection")?,
            xdnd_enter: atom(conn, b"XdndEnter")?,
            xdnd_leave: atom(conn, b"XdndLeave")?,
            xdnd_position: atom(conn, b"XdndPosition")?,
            xdnd_status: atom(conn, b"XdndStatus")?,
            xdnd_drop: atom(conn, b"XdndDrop")?,
            xdnd_finished: atom(conn, b"XdndFinished")?,
            xdnd_aware: atom(conn, b"XdndAware")?,
            xdnd_proxy: atom(conn, b"XdndProxy")?,
            xdnd_type_list: atom(conn, b"XdndTypeList")?,
            xdnd_action_copy: atom(conn, b"XdndActionCopy")?,
            net_active_window: atom(conn, b"_NET_ACTIVE_WINDOW")?,
            text_uri_list: atom(conn, b"text/uri-list")?,
            text_uri_list_utf8: atom(conn, b"text/uri-list;charset=utf-8")?,
            text_x_uri: atom(conn, b"text/x-uri")?,
            application_vnd_portal_filetransfer: atom(
                conn,
                b"application/vnd.portal.filetransfer",
            )?,
            application_vnd_portal_files: atom(conn, b"application/vnd.portal.files")?,
            application_x_kde4_urilist: atom(conn, b"application/x-kde4-urilist")?,
            x_special_gnome_copied_files: atom(conn, b"x-special/gnome-copied-files")?,
            text_plain: atom(conn, b"text/plain")?,
            text_plain_utf8: atom(conn, b"text/plain;charset=utf-8")?,
            targets: atom(conn, b"TARGETS")?,
            timestamp_property: atom(conn, b"DROP_RECORDER_XDND_TIME")?,
            utf8_string: atom(conn, b"UTF8_STRING")?,
            string: atom(conn, b"STRING")?,
            wm_class: atom(conn, b"WM_CLASS")?,
            wm_name: atom(conn, b"WM_NAME")?,
            net_wm_name: atom(conn, b"_NET_WM_NAME")?,
        })
    }

    fn mime_targets(&self) -> MimeTargets {
        MimeTargets {
            portal_filetransfer: self.application_vnd_portal_filetransfer,
            portal_files: self.application_vnd_portal_files,
            text_uri_list: self.text_uri_list,
            text_uri_list_utf8: self.text_uri_list_utf8,
            text_x_uri: self.text_x_uri,
            kde_uri_list: self.application_x_kde4_urilist,
            gnome_copied_files: self.x_special_gnome_copied_files,
            text_plain: self.text_plain,
            text_plain_utf8: self.text_plain_utf8,
            utf8_string: self.utf8_string,
            string: self.string,
        }
    }
}

struct PreviewWindow {
    window: XWindow,
    gc_bg: Gcontext,
    gc_panel: Gcontext,
    gc_fill: Gcontext,
    gc_border: Gcontext,
    gc_wave: Gcontext,
    gc_glow: Gcontext,
    gc_mid: Gcontext,
    width: u16,
    height: u16,
    preview: ExternalDragPreview,
    desktop: PreviewDesktop,
    screen_width: i32,
    screen_height: i32,
}

#[derive(Clone)]
struct PreviewMonitor {
    name: String,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

#[derive(Clone)]
struct HyprMonitor {
    name: String,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    scale: f32,
    x11_x: i32,
    x11_y: i32,
}

struct PreviewDesktop {
    x11_monitors: Vec<PreviewMonitor>,
    hypr_monitors: Vec<HyprMonitor>,
    env_scale: f32,
}

impl PreviewWindow {
    fn new(
        drag_id: u64,
        conn: &RustConnection,
        screen: &Screen,
        preview: ExternalDragPreview,
    ) -> Result<Self, String> {
        let width = 224;
        let height = 90;
        let window = conn.generate_id().map_err(|err| err.to_string())?;
        conn.create_window(
            screen.root_depth,
            window,
            screen.root,
            -10_000,
            -10_000,
            width,
            height,
            0,
            WindowClass::INPUT_OUTPUT,
            screen.root_visual,
            &CreateWindowAux::new()
                .override_redirect(1)
                .background_pixel(rgb(20, 17, 31))
                .event_mask(EventMask::EXPOSURE),
        )
        .map_err(|err| err.to_string())?;

        let gc_bg = Self::gc(conn, window, rgb(20, 17, 31), 1)?;
        let gc_panel = Self::gc(conn, window, rgb(13, 19, 30), 1)?;
        let gc_fill = Self::gc(conn, window, rgb(75, 58, 116), 1)?;
        let gc_border = Self::gc(conn, window, rgb(204, 222, 238), 2)?;
        let gc_wave = Self::gc(conn, window, rgb(169, 222, 255), 2)?;
        let gc_glow = Self::gc(conn, window, rgb(168, 107, 234), 1)?;
        let gc_mid = Self::gc(conn, window, rgb(75, 98, 124), 1)?;
        let desktop = PreviewDesktop::detect(conn, screen);
        emit_backend_event(format!(
            "[dnd#{drag_id}] Preview desktop map: {}",
            desktop.summary()
        ));
        let preview = Self {
            window,
            gc_bg,
            gc_panel,
            gc_fill,
            gc_border,
            gc_wave,
            gc_glow,
            gc_mid,
            width,
            height,
            preview,
            desktop,
            screen_width: i32::from(screen.width_in_pixels),
            screen_height: i32::from(screen.height_in_pixels),
        };
        preview.draw(conn)?;
        conn.map_window(window).map_err(|err| err.to_string())?;
        conn.flush().map_err(|err| err.to_string())?;
        Ok(preview)
    }

    fn gc(
        conn: &RustConnection,
        window: XWindow,
        color: u32,
        line_width: u32,
    ) -> Result<Gcontext, String> {
        let gc = conn.generate_id().map_err(|err| err.to_string())?;
        conn.create_gc(
            gc,
            window,
            &CreateGCAux::new().foreground(color).line_width(line_width),
        )
        .map_err(|err| err.to_string())?;
        Ok(gc)
    }

    fn update(&self, conn: &RustConnection, root_x: i16, root_y: i16) -> Result<(), String> {
        let (x, y) = self.preview_position(root_x, root_y);
        conn.configure_window(
            self.window,
            &ConfigureWindowAux::new()
                .x(x)
                .y(y)
                .stack_mode(StackMode::ABOVE),
        )
        .map_err(|err| err.to_string())?;
        self.draw(conn)?;
        conn.flush().map_err(|err| err.to_string())
    }

    fn preview_position(&self, root_x: i16, root_y: i16) -> (i32, i32) {
        let mut x = i32::from(root_x);
        let mut y = i32::from(root_y);
        (x, y) = self.desktop.map_point(x, y);

        let max_x = self
            .screen_width
            .saturating_sub(i32::from(self.width))
            .saturating_sub(8);
        let max_y = self
            .screen_height
            .saturating_sub(i32::from(self.height))
            .saturating_sub(8);
        ((x + 18).clamp(0, max_x), (y + 18).clamp(0, max_y))
    }

    fn draw(&self, conn: &RustConnection) -> Result<(), String> {
        conn.poly_fill_rectangle(
            self.window,
            self.gc_bg,
            &[Rectangle {
                x: 0,
                y: 0,
                width: self.width,
                height: self.height,
            }],
        )
        .map_err(|err| err.to_string())?;
        conn.poly_fill_rectangle(
            self.window,
            self.gc_panel,
            &[Rectangle {
                x: 5,
                y: 5,
                width: self.width.saturating_sub(10),
                height: self.height.saturating_sub(10),
            }],
        )
        .map_err(|err| err.to_string())?;
        conn.poly_rectangle(
            self.window,
            self.gc_border,
            &[Rectangle {
                x: 1,
                y: 1,
                width: self.width.saturating_sub(3),
                height: self.height.saturating_sub(3),
            }],
        )
        .map_err(|err| err.to_string())?;

        match &self.preview {
            ExternalDragPreview::Waveform { buckets } => {
                self.draw_waveform(conn, buckets)?;
            }
            ExternalDragPreview::Spectral {
                columns,
                rows,
                energy,
                ..
            } => {
                self.draw_spectral(conn, *columns, *rows, energy)?;
            }
        }
        Ok(())
    }

    fn draw_waveform(&self, conn: &RustConnection, buckets: &[(f32, f32)]) -> Result<(), String> {
        let center = (self.height / 2) as i16;
        let inset = 12.0_f32;
        let usable_w = (self.width as f32 - inset * 2.0).max(1.0);
        let scale = self.height as f32 * 0.36;
        let fill_segments =
            self.segments_for_waveform(buckets, inset, usable_w, center as f32, scale);
        if !fill_segments.is_empty() {
            conn.poly_segment(self.window, self.gc_fill, &fill_segments)
                .map_err(|err| err.to_string())?;
        }
        conn.poly_line(
            CoordMode::ORIGIN,
            self.window,
            self.gc_mid,
            &[
                Point { x: 9, y: center },
                Point {
                    x: self.width.saturating_sub(9) as i16,
                    y: center,
                },
            ],
        )
        .map_err(|err| err.to_string())?;

        let upper = self.points_for_waveform(buckets, inset, usable_w, center as f32, -scale, true);
        let lower =
            self.points_for_waveform(buckets, inset, usable_w, center as f32, -scale, false);
        conn.poly_line(CoordMode::ORIGIN, self.window, self.gc_glow, &upper)
            .map_err(|err| err.to_string())?;
        conn.poly_line(CoordMode::ORIGIN, self.window, self.gc_glow, &lower)
            .map_err(|err| err.to_string())?;
        conn.poly_line(CoordMode::ORIGIN, self.window, self.gc_wave, &upper)
            .map_err(|err| err.to_string())?;
        conn.poly_line(CoordMode::ORIGIN, self.window, self.gc_wave, &lower)
            .map_err(|err| err.to_string())
            .map(|_| ())
    }

    fn draw_spectral(
        &self,
        conn: &RustConnection,
        columns: usize,
        rows: usize,
        energy: &[f32],
    ) -> Result<(), String> {
        if columns == 0 || rows == 0 || energy.is_empty() {
            return Ok(());
        }

        let inset = 10_i16;
        let width = self.width.saturating_sub(20).max(1) as f32;
        let height = self.height.saturating_sub(20).max(1) as f32;
        let cell_w = (width / columns as f32).max(1.0);
        let cell_h = (height / rows as f32).max(1.0);
        let mut low = Vec::new();
        let mut mid = Vec::new();
        let mut high = Vec::new();

        for column in 0..columns {
            for row in 0..rows {
                let value = energy
                    .get(column * rows + row)
                    .copied()
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0);
                if value <= 0.035 {
                    continue;
                }
                let x = inset + (column as f32 * cell_w).round() as i16;
                let y = inset + ((rows - row - 1) as f32 * cell_h).round() as i16;
                let rect = Rectangle {
                    x,
                    y,
                    width: cell_w.ceil().max(1.0) as u16,
                    height: cell_h.ceil().max(1.0) as u16,
                };
                if value > 0.66 {
                    high.push(rect);
                } else if value > 0.28 {
                    mid.push(rect);
                } else {
                    low.push(rect);
                }
            }
        }

        if !low.is_empty() {
            conn.poly_fill_rectangle(self.window, self.gc_fill, &low)
                .map_err(|err| err.to_string())?;
        }
        if !mid.is_empty() {
            conn.poly_fill_rectangle(self.window, self.gc_glow, &mid)
                .map_err(|err| err.to_string())?;
        }
        if !high.is_empty() {
            conn.poly_fill_rectangle(self.window, self.gc_wave, &high)
                .map_err(|err| err.to_string())?;
        }
        Ok(())
    }

    fn segments_for_waveform(
        &self,
        buckets: &[(f32, f32)],
        inset: f32,
        usable_w: f32,
        center: f32,
        scale: f32,
    ) -> Vec<Segment> {
        let len = buckets.len().saturating_sub(1).max(1);
        buckets
            .iter()
            .enumerate()
            .map(|(index, (min, max))| {
                let x = (inset + index as f32 / len as f32 * usable_w).round() as i16;
                Segment {
                    x1: x,
                    y1: (center - max * scale).round() as i16,
                    x2: x,
                    y2: (center - min * scale).round() as i16,
                }
            })
            .collect()
    }

    fn points_for_waveform(
        &self,
        buckets: &[(f32, f32)],
        inset: f32,
        usable_w: f32,
        center: f32,
        scale: f32,
        upper: bool,
    ) -> Vec<Point> {
        let len = buckets.len().saturating_sub(1).max(1);
        buckets
            .iter()
            .enumerate()
            .map(|(index, (min, max))| {
                let value = if upper { *max } else { *min };
                Point {
                    x: (inset + index as f32 / len as f32 * usable_w).round() as i16,
                    y: (center + value * scale).round() as i16,
                }
            })
            .collect()
    }
}

impl PreviewMonitor {
    fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }
}

impl HyprMonitor {
    fn contains_logical(&self, x: i32, y: i32) -> bool {
        let logical_width = (self.width as f32 / self.scale.max(1.0)).round() as i32;
        let logical_height = (self.height as f32 / self.scale.max(1.0)).round() as i32;
        x >= self.x && y >= self.y && x < self.x + logical_width && y < self.y + logical_height
    }

    fn contains_x11(&self, x: i32, y: i32) -> bool {
        x >= self.x11_x
            && y >= self.x11_y
            && x < self.x11_x + self.width
            && y < self.x11_y + self.height
    }

    fn map_logical_to_x11(&self, x: i32, y: i32) -> (i32, i32) {
        (
            self.x11_x + ((x - self.x) as f32 * self.scale).round() as i32,
            self.x11_y + ((y - self.y) as f32 * self.scale).round() as i32,
        )
    }

    fn map_x11_to_logical(&self, x: i32, y: i32) -> (i32, i32) {
        (
            self.x + ((x - self.x11_x) as f32 / self.scale.max(1.0)).round() as i32,
            self.y + ((y - self.x11_y) as f32 / self.scale.max(1.0)).round() as i32,
        )
    }
}

impl PreviewDesktop {
    fn detect(conn: &RustConnection, screen: &Screen) -> Self {
        let env_scale = preview_env_scale();
        let x11_monitors = preview_monitors(conn, screen, env_scale);
        let hypr_monitors = hypr_preview_monitors(&x11_monitors);
        Self {
            x11_monitors,
            hypr_monitors,
            env_scale,
        }
    }

    fn map_point(&self, x: i32, y: i32) -> (i32, i32) {
        if !preview_coordinate_remap_enabled() {
            return (x, y);
        }

        let x11_name = self
            .x11_monitors
            .iter()
            .find(|monitor| monitor.contains(x, y))
            .map(|monitor| monitor.name.as_str());

        if let Some(monitor) = self
            .hypr_monitors
            .iter()
            .find(|monitor| monitor.contains_logical(x, y))
        {
            if monitor.scale > 1.001 || x11_name != Some(monitor.name.as_str()) {
                return monitor.map_logical_to_x11(x, y);
            }
        }

        if !self.hypr_monitors.is_empty() {
            return (x, y);
        }

        if self.env_scale > 1.0 {
            if let Some(monitor) = self
                .x11_monitors
                .iter()
                .find(|monitor| monitor.contains(x, y) && monitor.width >= 3000)
            {
                return (
                    monitor.x + ((x - monitor.x) as f32 * self.env_scale).round() as i32,
                    monitor.y + ((y - monitor.y) as f32 * self.env_scale).round() as i32,
                );
            }
        }

        (x, y)
    }

    fn map_x11_to_logical(&self, x: i32, y: i32) -> Option<(i32, i32)> {
        self.hypr_monitors
            .iter()
            .find(|monitor| monitor.contains_x11(x, y))
            .map(|monitor| monitor.map_x11_to_logical(x, y))
    }

    fn summary(&self) -> String {
        let x11 = self
            .x11_monitors
            .iter()
            .map(|monitor| {
                format!(
                    "{}={}x{}+{}+{}",
                    monitor.name, monitor.width, monitor.height, monitor.x, monitor.y
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let hypr = self
            .hypr_monitors
            .iter()
            .map(|monitor| {
                format!(
                    "{}=logical {}x{}+{}+{} scale {:.2} -> x11 +{}+{}",
                    monitor.name,
                    (monitor.width as f32 / monitor.scale.max(1.0)).round() as i32,
                    (monitor.height as f32 / monitor.scale.max(1.0)).round() as i32,
                    monitor.x,
                    monitor.y,
                    monitor.scale,
                    monitor.x11_x,
                    monitor.x11_y
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        if hypr.is_empty() {
            format!(
                "x11=[{x11}] hypr=unavailable env_scale={:.2}",
                self.env_scale
            )
        } else {
            format!("x11=[{x11}] hypr=[{hypr}] env_scale={:.2}", self.env_scale)
        }
    }
}

fn rgb(red: u32, green: u32, blue: u32) -> u32 {
    (red << 16) | (green << 8) | blue
}

fn contains_point(x: i16, y: i16, width: u16, height: u16, point_x: i16, point_y: i16) -> bool {
    let x = x as i32;
    let y = y as i32;
    let width = width as i32;
    let height = height as i32;
    let point_x = point_x as i32;
    let point_y = point_y as i32;
    point_x >= x && point_y >= y && point_x < x + width && point_y < y + height
}

fn preview_monitors(
    conn: &RustConnection,
    screen: &Screen,
    _env_scale: f32,
) -> Vec<PreviewMonitor> {
    conn.randr_get_monitors(screen.root, true)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .map(|reply| {
            reply
                .monitors
                .into_iter()
                .map(|monitor| {
                    let name = randr_monitor_name(conn, monitor.name);
                    PreviewMonitor {
                        name,
                        x: i32::from(monitor.x),
                        y: i32::from(monitor.y),
                        width: i32::from(monitor.width),
                        height: i32::from(monitor.height),
                    }
                })
                .collect::<Vec<_>>()
        })
        .filter(|monitors| !monitors.is_empty())
        .unwrap_or_else(|| {
            vec![PreviewMonitor {
                name: "screen".to_string(),
                x: 0,
                y: 0,
                width: i32::from(screen.width_in_pixels),
                height: i32::from(screen.height_in_pixels),
            }]
        })
}

fn randr_monitor_name(conn: &RustConnection, atom: Atom) -> String {
    conn.get_atom_name(atom)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .map(|reply| String::from_utf8_lossy(&reply.name).into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "monitor".to_string())
}

fn hypr_preview_monitors(x11_monitors: &[PreviewMonitor]) -> Vec<HyprMonitor> {
    if !std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .contains("hyprland")
    {
        return Vec::new();
    }

    let Ok(output) = Command::new("hyprctl").args(["monitors", "-j"]).output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return Vec::new();
    };
    let Some(monitors) = value.as_array() else {
        return Vec::new();
    };

    monitors
        .iter()
        .filter_map(|monitor| {
            let name = monitor.get("name")?.as_str()?.to_string();
            let x11 = x11_monitors.iter().find(|x11| x11.name == name)?;
            let width = monitor
                .get("width")?
                .as_i64()
                .and_then(|value| i32::try_from(value).ok())?;
            let height = monitor
                .get("height")?
                .as_i64()
                .and_then(|value| i32::try_from(value).ok())?;
            let x = monitor
                .get("x")?
                .as_i64()
                .and_then(|value| i32::try_from(value).ok())?;
            let y = monitor
                .get("y")?
                .as_i64()
                .and_then(|value| i32::try_from(value).ok())?;
            let scale = monitor.get("scale")?.as_f64()? as f32;
            if !scale.is_finite() || scale <= 0.0 {
                return None;
            }

            Some(HyprMonitor {
                name,
                x,
                y,
                width,
                height,
                scale,
                x11_x: x11.x,
                x11_y: x11.y,
            })
        })
        .collect()
}

fn preview_env_scale() -> f32 {
    [
        "AUDIO_PLUGIN_DND_PREVIEW_SCALE",
        "GDK_SCALE",
        "CLUTTER_SCALE",
    ]
    .into_iter()
    .find_map(|key| std::env::var(key).ok()?.parse::<f32>().ok())
    .filter(|scale| scale.is_finite() && *scale > 0.0)
    .unwrap_or(1.0)
}

fn preview_coordinate_remap_enabled() -> bool {
    std::env::var("AUDIO_PLUGIN_DND_PREVIEW_REMAP")
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

struct XdndSource {
    drag_id: u64,
    conn: RustConnection,
    screen_num: usize,
    atoms: XdndAtoms,
    source_window: XWindow,
    origin_window: Option<XWindow>,
    file_payload: FileDragPayloadData,
    portal_filetransfer_key: Option<Vec<u8>>,
    last_event_time: u32,
    current_target: Option<XWindow>,
    accepted_target: Option<XWindow>,
    last_real_target: Option<XWindow>,
    last_real_data_target: Option<XWindow>,
    last_real_accepted_target: Option<XWindow>,
    recent_real_target: RecentRealTarget,
    last_logged_accept: Option<XWindow>,
    drop_target: Option<XWindow>,
    drop_target_data_requests: usize,
    post_drop_data_requests: usize,
    selection_requests: usize,
    data_requests: usize,
    logged_data_request: bool,
    preview: Option<PreviewWindow>,
}

impl XdndSource {
    fn new(
        drag_id: u64,
        paths: Vec<PathBuf>,
        preview: Option<ExternalDragPreview>,
        origin_window: Option<XWindow>,
    ) -> Result<Self, String> {
        let (conn, screen_num) = RustConnection::connect(None).map_err(|err| err.to_string())?;
        let atoms = XdndAtoms::new(&conn)?;
        let screen = &conn.setup().roots[screen_num];
        let source_window = conn.generate_id().map_err(|err| err.to_string())?;

        conn.create_window(
            screen.root_depth,
            source_window,
            screen.root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            screen.root_visual,
            &CreateWindowAux::new().override_redirect(1).event_mask(
                EventMask::PROPERTY_CHANGE
                    | EventMask::STRUCTURE_NOTIFY
                    | EventMask::POINTER_MOTION
                    | EventMask::BUTTON_RELEASE,
            ),
        )
        .map_err(|err| err.to_string())?;
        conn.configure_window(
            source_window,
            &ConfigureWindowAux::new().x(-10_000).y(-10_000),
        )
        .map_err(|err| err.to_string())?;
        set_window_identity(&conn, source_window, &atoms, b"Audio Plugin DND")?;

        let timestamp = server_time(&conn, source_window, atoms.timestamp_property)?;
        conn.change_property32(
            PropMode::REPLACE,
            source_window,
            atoms.xdnd_aware,
            AtomEnum::ATOM,
            &[XDND_VERSION],
        )
        .map_err(|err| err.to_string())?;
        conn.set_selection_owner(source_window, atoms.xdnd_selection, timestamp)
            .map_err(|err| err.to_string())?;
        let owner = conn
            .get_selection_owner(atoms.xdnd_selection)
            .map_err(|err| err.to_string())?
            .reply()
            .map_err(|err| err.to_string())?
            .owner;
        if owner != source_window {
            return Err("could not own XdndSelection".to_string());
        }
        conn.flush().map_err(|err| err.to_string())?;

        #[cfg(any(
            feature = "nice-log",
            all(feature = "tracing", not(feature = "nice-log"))
        ))]
        info!("External file drag start: {} file(s)", paths.len());

        let portal_filetransfer_key = match portal::start_file_transfer(&paths) {
            Ok(transfer) => {
                emit_backend_event(format!(
                    "[dnd#{drag_id}] Portal FileTransfer ready via {}: {} file(s), key {} bytes",
                    transfer.backend,
                    paths.len(),
                    transfer.key.len()
                ));
                Some(transfer.key)
            }
            Err(err) => {
                emit_backend_event(format!(
                    "[dnd#{drag_id}] Portal FileTransfer unavailable: {err}"
                ));
                None
            }
        };

        let preview =
            preview.and_then(|preview| PreviewWindow::new(drag_id, &conn, screen, preview).ok());

        Ok(Self {
            drag_id,
            conn,
            screen_num,
            atoms,
            source_window,
            origin_window,
            file_payload: FileDragPayloadData::new(paths)?,
            portal_filetransfer_key,
            last_event_time: timestamp,
            current_target: None,
            accepted_target: None,
            last_real_target: None,
            last_real_data_target: None,
            last_real_accepted_target: None,
            recent_real_target: RecentRealTarget::default(),
            last_logged_accept: None,
            drop_target: None,
            drop_target_data_requests: 0,
            post_drop_data_requests: 0,
            selection_requests: 0,
            data_requests: 0,
            logged_data_request: false,
            preview,
        })
    }

    fn log(&self, message: impl AsRef<str>) {
        emit_backend_event(format!("[dnd#{}] {}", self.drag_id, message.as_ref()));
    }

    fn verbose_logging(&self) -> bool {
        std::env::var("AUDIO_PLUGIN_DND_VERBOSE")
            .map(|value| {
                matches!(
                    value.to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false)
    }

    fn drop_finish_wait(&self) -> Duration {
        if self.data_requests > 0 {
            DROP_READY_FINISH_WAIT
        } else {
            DROP_FINISH_WAIT
        }
    }

    fn payload_request_phase(&self) -> &'static str {
        if self.drop_target.is_some() {
            "drop payload"
        } else {
            "hover payload"
        }
    }

    fn note_event_time(&mut self, time: u32) {
        if time != CURRENT_TIME {
            self.last_event_time = time;
        }
    }

    fn run(mut self) -> Result<DragSessionReport, String> {
        self.log(format!(
            "{} via {}",
            DragPhase::Started.summary(),
            DragTargetKind::RealXWindow.summary()
        ));
        self.update_target_from_pointer()?;
        let deadline = Instant::now() + Duration::from_secs(6);
        let mut finish_deadline = None;
        let mut waiting_for_finished = false;
        let mut finished_received = false;
        let mut saw_button_down = false;
        let mut sent_drop = false;

        loop {
            let now = Instant::now();
            if finish_deadline.is_some_and(|deadline| now > deadline) {
                if finished_received {
                    #[cfg(any(
                        feature = "nice-log",
                        all(feature = "tracing", not(feature = "nice-log"))
                    ))]
                    info!(
                        "XDND finished grace ended: selection_requests={}, data_requests={}",
                        self.selection_requests, self.data_requests
                    );
                } else {
                }
                break;
            }
            if now > deadline {
                self.leave_current_target()?;
                break;
            }

            if let Some(event) = self.conn.poll_for_event().map_err(|err| err.to_string())? {
                match event {
                    Event::MotionNotify(event) if !waiting_for_finished => {
                        self.handle_motion(event)?
                    }
                    Event::ButtonPress(event) if !waiting_for_finished => {
                        self.handle_button_press(event)?
                    }
                    Event::ButtonRelease(event) if !waiting_for_finished => {
                        waiting_for_finished = self.handle_button_release(event)?;
                        sent_drop |= waiting_for_finished;
                        if waiting_for_finished {
                            finish_deadline = Some(Instant::now() + self.drop_finish_wait());
                        }
                        if !waiting_for_finished {
                            break;
                        }
                    }
                    Event::ClientMessage(event) if event.type_ == self.atoms.xdnd_status => {
                        self.handle_status(event);
                    }
                    Event::ClientMessage(event) if event.type_ == self.atoms.xdnd_finished => {
                        #[cfg(any(
                            feature = "nice-log",
                            all(feature = "tracing", not(feature = "nice-log"))
                        ))]
                        info!(
                            "XDND finished: selection_requests={}, data_requests={}",
                            self.selection_requests, self.data_requests
                        );
                        finished_received = true;
                        self.log(format!(
                            "XDND target finished drop: total_data_requests={}",
                            self.session_stats().total_data_requests()
                        ));
                        waiting_for_finished = true;
                        finish_deadline = Some(Instant::now() + DROP_SELECTION_GRACE);
                    }
                    Event::SelectionRequest(event) => {
                        self.handle_selection_request(event)?;
                        if waiting_for_finished && self.data_requests > 0 {
                            finish_deadline = Some(Instant::now() + DROP_READY_FINISH_WAIT);
                        }
                    }
                    _ => {}
                }
            } else {
                if !waiting_for_finished {
                    let pointer = self.update_target_from_pointer()?;
                    saw_button_down |= primary_down(pointer.mask);
                    if saw_button_down && !primary_down(pointer.mask) {
                        waiting_for_finished = self.handle_button_release(ButtonReleaseEvent {
                            response_type: x11rb::protocol::xproto::BUTTON_RELEASE_EVENT,
                            detail: 1,
                            sequence: 0,
                            time: CURRENT_TIME,
                            root: pointer.root,
                            event: pointer.root,
                            child: pointer.child,
                            root_x: pointer.root_x,
                            root_y: pointer.root_y,
                            event_x: pointer.win_x,
                            event_y: pointer.win_y,
                            state: pointer.mask,
                            same_screen: pointer.same_screen,
                        })?;
                        sent_drop |= waiting_for_finished;
                        if waiting_for_finished {
                            finish_deadline = Some(Instant::now() + self.drop_finish_wait());
                        }
                        if !waiting_for_finished {
                            break;
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(8));
            }
        }

        self.conn
            .destroy_window(self.source_window)
            .map_err(|err| err.to_string())?;
        if let Some(preview) = &self.preview {
            let _ = self.conn.destroy_window(preview.window);
        }
        self.conn.flush().map_err(|err| err.to_string())?;

        let stats = self.session_stats();
        if !sent_drop {
            return Ok(DragSessionReport::failed(
                DragFailureKind::Cancelled,
                stats,
                "XDND drag ended without sending a drop",
            ));
        }
        if self
            .drop_target
            .is_some_and(|target| self.is_anonymous_xdnd_bridge(target))
            && self.drop_target_data_requests == 0
        {
            return Ok(DragSessionReport::failed(
                DragFailureKind::BridgeRejected,
                stats,
                format!(
                    "released over native Wayland target through XWayland bridge; XDND cannot deliver to that target from this XWayland editor; {}",
                    self.export_fallback_hint()
                ),
            ));
        }
        if self.data_requests == 0 {
            return Ok(DragSessionReport::failed(
                DragFailureKind::TargetNoData,
                stats,
                format!(
                    "drop target never requested file data; {}",
                    self.export_fallback_hint()
                ),
            ));
        }

        if finished_received {
            return Ok(DragSessionReport::completed_confirmed(
                stats,
                "target sent XdndFinished",
            ));
        }
        if self.post_drop_data_requests > 0 || self.drop_target_data_requests > 0 {
            return Ok(DragSessionReport::completed_confirmed(
                stats,
                "target requested file data after drop",
            ));
        }
        Ok(DragSessionReport::completed_inferred(
            stats,
            "target inspected file data before drop and the drop was sent",
        ))
    }

    fn handle_motion(&mut self, event: MotionNotifyEvent) -> Result<(), String> {
        self.note_event_time(event.time);
        self.update_target_from_pointer().map(|_| ())
    }

    fn handle_button_press(&mut self, event: ButtonPressEvent) -> Result<(), String> {
        self.note_event_time(event.time);
        self.update_target_from_pointer().map(|_| ())
    }

    fn handle_button_release(&mut self, event: ButtonReleaseEvent) -> Result<bool, String> {
        self.note_event_time(event.time);
        let Some(target) = self.current_target else {
            self.leave_current_target()?;
            return Err(self.no_target_diagnostics());
        };
        if self.is_origin_window(target) {
            self.leave_current_target()?;
            return Err("drop released back on plugin window; cancelled".to_string());
        }
        let drop_target = if self.is_anonymous_xdnd_bridge(target) {
            self.bridge_release_target(target, event.root_x, event.root_y)?
        } else {
            target
        };

        #[cfg(any(
            feature = "nice-log",
            all(feature = "tracing", not(feature = "nice-log"))
        ))]
        info!("XDND drop target=0x{drop_target:x}");
        self.log(format!(
            "XDND drop sent: {}",
            self.window_diagnostics(drop_target)
        ));
        self.drop_target = Some(drop_target);
        self.drop_target_data_requests = 0;

        self.send_client_message(
            drop_target,
            self.atoms.xdnd_drop,
            [self.source_window, 0, self.last_event_time, 0, 0],
        )?;
        Ok(true)
    }

    fn handle_status(&mut self, event: ClientMessageEvent) {
        let data = event.data.as_data32();
        let target = data[0];
        if data[1] & STATUS_ACCEPT == STATUS_ACCEPT {
            self.accepted_target = Some(target);
            if self.is_real_xdnd_target(target) {
                self.last_real_accepted_target = Some(target);
                self.recent_real_target.note_status_accept(target);
            }
            if self.last_logged_accept != Some(target) {
                self.last_logged_accept = Some(target);
                self.log(format!(
                    "XDND target status: advertised accept: {} ({})",
                    self.window_diagnostics(target),
                    self.status_diagnostics(data)
                ));
            }
            #[cfg(any(
                feature = "nice-log",
                all(feature = "tracing", not(feature = "nice-log"))
            ))]
            info!("XDND status accepted target=0x{target:x}");
        } else if self.accepted_target == Some(target) {
            self.accepted_target = None;
            if self.last_real_accepted_target == Some(target) {
                self.last_real_accepted_target = None;
                self.recent_real_target.note_status_reject(target);
            }
            self.last_logged_accept = None;
            if self.verbose_logging() {
                self.log(format!(
                    "XDND target status: no advertised accept yet: {} ({})",
                    self.window_diagnostics(target),
                    self.status_diagnostics(data)
                ));
            }
            #[cfg(any(
                feature = "nice-log",
                all(feature = "tracing", not(feature = "nice-log"))
            ))]
            info!("XDND status no-accept target=0x{target:x}");
        } else if self.last_logged_accept != Some(target) {
            self.last_logged_accept = Some(target);
            if self.verbose_logging() {
                self.log(format!(
                    "XDND target status: no advertised accept yet: {} ({})",
                    self.window_diagnostics(target),
                    self.status_diagnostics(data)
                ));
            }
        }
    }

    fn status_diagnostics(&self, data: [u32; 5]) -> String {
        format!("flags=0x{:x}, action={}", data[1], self.atom_name(data[4]))
    }

    fn bridge_release_target(
        &self,
        bridge: XWindow,
        root_x: i16,
        root_y: i16,
    ) -> Result<XWindow, String> {
        let root = self.conn.setup().roots[self.screen_num].root;
        if let Some(target) = self.real_xdnd_child_at(root, root_x, root_y)? {
            self.log(format!(
                "XDND bridge release resolved at {root_x},{root_y} ({}): {}",
                self.preview_position_diagnostics(root_x, root_y),
                self.window_diagnostics(target)
            ));
            return Ok(target);
        }

        for target in [
            self.last_real_data_target,
            self.last_real_accepted_target,
            self.last_real_target,
        ]
        .into_iter()
        .flatten()
        {
            if self.window_contains_root_point(target, root_x, root_y) {
                self.log(format!(
                    "XDND bridge release reused target under pointer at {root_x},{root_y} ({}): {}",
                    self.preview_position_diagnostics(root_x, root_y),
                    self.window_diagnostics(target)
                ));
                return Ok(target);
            }
        }

        self.log(format!(
            "XDND release target: native Wayland target via XWayland bridge at {root_x},{root_y}; target_app={}; {}",
            self.hypr_window_at_x11_point(root_x, root_y)
                .unwrap_or_else(|| "unknown native Wayland app".to_string()),
            self.preview_position_diagnostics(root_x, root_y)
        ));
        Ok(bridge)
    }

    fn hypr_window_at_x11_point(&self, root_x: i16, root_y: i16) -> Option<String> {
        let (logical_x, logical_y) = self.preview.as_ref().and_then(|preview| {
            preview
                .desktop
                .map_x11_to_logical(i32::from(root_x), i32::from(root_y))
        })?;
        let output = Command::new("hyprctl")
            .args(["clients", "-j"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let value = serde_json::from_slice::<serde_json::Value>(&output.stdout).ok()?;
        let clients = value.as_array()?;
        clients
            .iter()
            .filter(|client| {
                !client
                    .get("hidden")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false)
            })
            .find_map(|client| {
                let at = client.get("at")?.as_array()?;
                let size = client.get("size")?.as_array()?;
                let x = at
                    .first()?
                    .as_i64()
                    .and_then(|value| i32::try_from(value).ok())?;
                let y = at
                    .get(1)?
                    .as_i64()
                    .and_then(|value| i32::try_from(value).ok())?;
                let width = size
                    .first()?
                    .as_i64()
                    .and_then(|value| i32::try_from(value).ok())?;
                let height = size
                    .get(1)?
                    .as_i64()
                    .and_then(|value| i32::try_from(value).ok())?;
                if logical_x < x
                    || logical_y < y
                    || logical_x >= x + width
                    || logical_y >= y + height
                {
                    return None;
                }
                let class = client
                    .get("class")
                    .and_then(|value| value.as_str())
                    .unwrap_or("native Wayland app");
                let title = client
                    .get("title")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                if title.is_empty() {
                    Some(class.to_string())
                } else {
                    Some(format!("{class} / {title}"))
                }
            })
    }

    fn export_fallback_hint(&self) -> String {
        let files = String::from_utf8_lossy(self.file_payload.plain_file_list());
        let files = files.trim();
        if files.is_empty() {
            "export remains in the temp folder for manual import".to_string()
        } else {
            format!("export remains available for manual import: {files}")
        }
    }

    fn session_stats(&self) -> DragSessionStats {
        DragSessionStats {
            selection_requests: self.selection_requests as u32,
            pre_drop_data_requests: self
                .data_requests
                .saturating_sub(self.post_drop_data_requests)
                as u32,
            post_drop_data_requests: self.post_drop_data_requests as u32,
            drop_target_data_requests: self.drop_target_data_requests as u32,
        }
    }

    fn preview_position_diagnostics(&self, root_x: i16, root_y: i16) -> String {
        self.preview
            .as_ref()
            .map(|preview| {
                let (x, y) = preview.preview_position(root_x, root_y);
                format!("preview={x},{y}")
            })
            .unwrap_or_else(|| "preview=none".to_string())
    }

    fn handle_selection_request(&mut self, event: SelectionRequestEvent) -> Result<(), String> {
        if event.selection != self.atoms.xdnd_selection {
            self.send_selection_notify(event, Atom::from(AtomEnum::NONE))?;
            return Ok(());
        }
        self.selection_requests += 1;

        let property = if event.property == Atom::from(AtomEnum::NONE) {
            event.target
        } else {
            event.property
        };

        if event.target == self.atoms.targets {
            let mut targets = Vec::with_capacity(12);
            targets.push(self.atoms.targets);
            targets.extend(self.offered_targets());
            self.conn
                .change_property32(
                    PropMode::REPLACE,
                    event.requestor,
                    property,
                    AtomEnum::ATOM,
                    &targets,
                )
                .map_err(|err| err.to_string())?;
            self.send_selection_notify(event, property)?;
        } else if self.is_portal_filetransfer_target(event.target) {
            self.data_requests += 1;
            if self.drop_target.is_some() {
                self.post_drop_data_requests += 1;
            }
            if self.drop_target == Some(event.requestor) {
                self.drop_target_data_requests += 1;
            }
            if self.is_real_xdnd_target(event.requestor) {
                self.last_real_data_target = Some(event.requestor);
                self.recent_real_target.note_data_request(event.requestor);
            }
            let Some(payload) = self.portal_filetransfer_key.as_ref() else {
                self.log(format!(
                    "XDND portal data requested but portal is unavailable: requestor={}, target={}",
                    self.window_diagnostics(event.requestor),
                    self.atom_name(event.target)
                ));
                self.send_selection_notify(event, Atom::from(AtomEnum::NONE))?;
                return self.conn.flush().map_err(|err| err.to_string());
            };
            if !self.logged_data_request {
                self.logged_data_request = true;
                self.log(format!(
                    "XDND {} requested by {} as {}",
                    self.payload_request_phase(),
                    self.window_diagnostics(event.requestor),
                    self.atom_name(event.target)
                ));
            } else if self.verbose_logging() {
                self.log(format!(
                    "XDND served portal file transfer: requestor={}, target={}, key {} bytes",
                    self.window_diagnostics(event.requestor),
                    self.atom_name(event.target),
                    payload.len()
                ));
            }
            self.conn
                .change_property8(
                    PropMode::REPLACE,
                    event.requestor,
                    property,
                    event.target,
                    payload,
                )
                .map_err(|err| err.to_string())?;
            self.send_selection_notify(event, property)?;
        } else if event.target == self.atoms.text_uri_list
            || event.target == self.atoms.text_uri_list_utf8
            || event.target == self.atoms.text_x_uri
            || event.target == self.atoms.application_x_kde4_urilist
            || event.target == self.atoms.text_plain
            || event.target == self.atoms.text_plain_utf8
            || event.target == self.atoms.utf8_string
            || event.target == self.atoms.string
            || event.target == self.atoms.x_special_gnome_copied_files
        {
            self.data_requests += 1;
            if self.drop_target.is_some() {
                self.post_drop_data_requests += 1;
            }
            if self.drop_target == Some(event.requestor) {
                self.drop_target_data_requests += 1;
            }
            if self.is_real_xdnd_target(event.requestor) {
                self.last_real_data_target = Some(event.requestor);
                self.recent_real_target.note_data_request(event.requestor);
            }
            if !self.logged_data_request {
                self.logged_data_request = true;
                self.log(format!(
                    "XDND {} requested by {} as {}",
                    self.payload_request_phase(),
                    self.window_diagnostics(event.requestor),
                    self.atom_name(event.target)
                ));
            } else if self.verbose_logging() {
                self.log(format!(
                    "XDND served file data: requestor={}, target={}",
                    self.window_diagnostics(event.requestor),
                    self.atom_name(event.target)
                ));
            }
            #[cfg(any(
                feature = "nice-log",
                all(feature = "tracing", not(feature = "nice-log"))
            ))]
            info!("XDND served data target atom={}", event.target);
            let payload = if event.target == self.atoms.x_special_gnome_copied_files {
                self.file_payload.gnome_copied_files()
            } else if event.target == self.atoms.text_plain_utf8
                || event.target == self.atoms.text_plain
                || event.target == self.atoms.utf8_string
                || event.target == self.atoms.string
            {
                self.file_payload.plain_file_list()
            } else {
                self.file_payload.uri_list()
            };
            self.conn
                .change_property8(
                    PropMode::REPLACE,
                    event.requestor,
                    property,
                    event.target,
                    payload,
                )
                .map_err(|err| err.to_string())?;
            self.send_selection_notify(event, property)?;
        } else {
            self.send_selection_notify(event, Atom::from(AtomEnum::NONE))?;
        }

        self.conn.flush().map_err(|err| err.to_string())
    }

    fn send_selection_notify(
        &self,
        request: SelectionRequestEvent,
        property: Atom,
    ) -> Result<(), String> {
        let event = SelectionNotifyEvent {
            response_type: x11rb::protocol::xproto::SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: request.time,
            requestor: request.requestor,
            selection: request.selection,
            target: request.target,
            property,
        };

        self.conn
            .send_event(false, request.requestor, EventMask::NO_EVENT, event)
            .map(|_| ())
            .map_err(|err| err.to_string())
    }

    fn update_target_from_pointer(
        &mut self,
    ) -> Result<x11rb::protocol::xproto::QueryPointerReply, String> {
        let root = self.conn.setup().roots[self.screen_num].root;
        let pointer = self
            .conn
            .query_pointer(root)
            .map_err(|err| err.to_string())?
            .reply()
            .map_err(|err| err.to_string())?;

        let target = if pointer.same_screen {
            self.find_xdnd_target(pointer.root_x, pointer.root_y)?
        } else {
            None
        };

        if target != self.current_target {
            self.leave_current_target()?;
            self.current_target = target;
            if let Some(target) = target {
                if self.is_real_xdnd_target(target) {
                    self.last_real_target = Some(target);
                    self.recent_real_target.note_entered_real(target);
                }
                if self.verbose_logging() {
                    self.log(format!(
                        "XDND entered target: {}",
                        self.window_diagnostics(target)
                    ));
                }
                #[cfg(any(
                    feature = "nice-log",
                    all(feature = "tracing", not(feature = "nice-log"))
                ))]
                info!("XDND enter target=0x{target:x}");
                self.send_enter(target)?;
            }
        }

        if let Some(target) = self.current_target {
            self.send_position(target, pointer.root_x, pointer.root_y)?;
        }
        if let Some(preview) = &self.preview {
            let _ = preview.update(&self.conn, pointer.root_x, pointer.root_y);
        }

        Ok(pointer)
    }

    fn find_xdnd_target(&self, root_x: i16, root_y: i16) -> Result<Option<XWindow>, String> {
        let root = self.conn.setup().roots[self.screen_num].root;
        let mut window = self.window_at(root, root_x, root_y)?;

        while let Some(candidate) = window {
            if self.is_origin_window(candidate) {
                return Ok(None);
            }
            if self.is_xdnd_aware(candidate)? {
                if self.is_anonymous_xdnd_bridge(candidate) {
                    if let Some(real_target) = self.real_xdnd_child_at(root, root_x, root_y)? {
                        return Ok(Some(real_target));
                    }
                }
                return self
                    .xdnd_proxy(candidate)
                    .map(|proxy| Some(proxy.unwrap_or(candidate)));
            }
            window = self.parent_of(candidate)?;
        }

        if let Some(target) = self.xdnd_bridge_child(root, root_x, root_y)? {
            return Ok(Some(target));
        }

        self.active_xdnd_target()
    }

    fn xdnd_bridge_child(
        &self,
        root: XWindow,
        root_x: i16,
        root_y: i16,
    ) -> Result<Option<XWindow>, String> {
        let tree = self
            .conn
            .query_tree(root)
            .map_err(|err| err.to_string())?
            .reply()
            .map_err(|err| err.to_string())?;

        for &candidate in tree.children.iter().rev() {
            if candidate == self.source_window || self.is_origin_window(candidate) {
                continue;
            }
            if !self.is_xdnd_aware(candidate).unwrap_or(false) {
                continue;
            }

            let Some(geometry) = self
                .conn
                .get_geometry(candidate)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
            else {
                continue;
            };

            if !contains_point(
                geometry.x,
                geometry.y,
                geometry.width,
                geometry.height,
                root_x,
                root_y,
            ) {
                continue;
            }

            if self.has_client_identity(candidate) {
                continue;
            }

            return self
                .xdnd_proxy(candidate)
                .map(|proxy| Some(proxy.unwrap_or(candidate)));
        }

        Ok(None)
    }

    fn real_xdnd_child_at(
        &self,
        root: XWindow,
        root_x: i16,
        root_y: i16,
    ) -> Result<Option<XWindow>, String> {
        let tree = self
            .conn
            .query_tree(root)
            .map_err(|err| err.to_string())?
            .reply()
            .map_err(|err| err.to_string())?;

        for &candidate in tree.children.iter().rev() {
            if candidate == self.source_window || self.is_origin_window(candidate) {
                continue;
            }
            if !self.is_xdnd_aware(candidate).unwrap_or(false)
                || !self.has_client_identity(candidate)
            {
                continue;
            }

            let Some(geometry) = self
                .conn
                .get_geometry(candidate)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
            else {
                continue;
            };

            if !contains_point(
                geometry.x,
                geometry.y,
                geometry.width,
                geometry.height,
                root_x,
                root_y,
            ) {
                continue;
            }

            return self
                .xdnd_proxy(candidate)
                .map(|proxy| Some(proxy.unwrap_or(candidate)));
        }

        Ok(None)
    }

    fn active_xdnd_target(&self) -> Result<Option<XWindow>, String> {
        let root = self.conn.setup().roots[self.screen_num].root;
        let property = self
            .conn
            .get_property(
                false,
                root,
                self.atoms.net_active_window,
                AtomEnum::WINDOW,
                0,
                1,
            )
            .map_err(|err| err.to_string())?
            .reply()
            .map_err(|err| err.to_string())?;
        let Some(window) = property.value32().and_then(|mut values| values.next()) else {
            return Ok(None);
        };
        if self.is_xdnd_aware(window)? {
            if self.is_origin_window(window) {
                return Ok(None);
            }
            return self
                .xdnd_proxy(window)
                .map(|proxy| Some(proxy.unwrap_or(window)));
        }
        Ok(None)
    }

    fn has_client_identity(&self, window: XWindow) -> bool {
        self.string_property(window, self.atoms.wm_class, self.atoms.string)
            .is_some()
            || self
                .string_property(window, self.atoms.wm_name, self.atoms.string)
                .is_some()
            || self
                .string_property(window, self.atoms.net_wm_name, self.atoms.utf8_string)
                .is_some()
    }

    fn is_anonymous_xdnd_bridge(&self, window: XWindow) -> bool {
        self.is_xdnd_aware(window).unwrap_or(false) && !self.has_client_identity(window)
    }

    fn is_real_xdnd_target(&self, window: XWindow) -> bool {
        self.is_xdnd_aware(window).unwrap_or(false) && self.has_client_identity(window)
    }

    fn window_contains_root_point(&self, window: XWindow, root_x: i16, root_y: i16) -> bool {
        self.conn
            .get_geometry(window)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .is_some_and(|geometry| {
                contains_point(
                    geometry.x,
                    geometry.y,
                    geometry.width,
                    geometry.height,
                    root_x,
                    root_y,
                )
            })
    }

    fn no_target_diagnostics(&self) -> String {
        let root = self.conn.setup().roots[self.screen_num].root;
        let pointer = self
            .conn
            .query_pointer(root)
            .ok()
            .and_then(|cookie| cookie.reply().ok());
        let pointer_position = pointer
            .as_ref()
            .map(|pointer| format!("{},{}", pointer.root_x, pointer.root_y))
            .unwrap_or_else(|| "unknown".to_string());
        let pointer_window = pointer
            .as_ref()
            .and_then(|pointer| self.window_at(root, pointer.root_x, pointer.root_y).ok())
            .flatten();
        let active_window = self
            .conn
            .get_property(
                false,
                root,
                self.atoms.net_active_window,
                AtomEnum::WINDOW,
                0,
                1,
            )
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .and_then(|property| property.value32().and_then(|mut values| values.next()));
        let pointer_aware = pointer_window.map(|window| {
            let aware = self.is_xdnd_aware(window).unwrap_or(false);
            (window, aware)
        });
        let active_aware = active_window.map(|window| {
            let aware = self.is_xdnd_aware(window).unwrap_or(false);
            (window, aware)
        });

        let pointer_chain = pointer_window
            .map(|window| self.window_chain(window, root).join(" <- "))
            .unwrap_or_else(|| "none".to_string());
        let bridge_candidates = pointer
            .as_ref()
            .map(|pointer| self.xdnd_bridge_diagnostics(root, pointer.root_x, pointer.root_y))
            .unwrap_or_else(|| "unknown".to_string());

        format!(
            "drop released with no XdndAware target under pointer; pos={pointer_position}; pointer={}; active={}; ancestry={pointer_chain}; bridge_candidates={bridge_candidates}",
            pointer_aware
                .map(|(window, aware)| format!("{}/aware={aware}", self.window_diagnostics(window)))
                .unwrap_or_else(|| "none".to_string()),
            active_aware
                .map(|(window, aware)| format!("{}/aware={aware}", self.window_diagnostics(window)))
                .unwrap_or_else(|| "none".to_string()),
        )
    }

    fn xdnd_bridge_diagnostics(&self, root: XWindow, root_x: i16, root_y: i16) -> String {
        let Some(tree) = self
            .conn
            .query_tree(root)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
        else {
            return "query_tree_failed".to_string();
        };

        let candidates = tree
            .children
            .iter()
            .rev()
            .filter_map(|&candidate| {
                if candidate == self.source_window || self.is_origin_window(candidate) {
                    return None;
                }
                if !self.is_xdnd_aware(candidate).unwrap_or(false) {
                    return None;
                }
                let geometry = self
                    .conn
                    .get_geometry(candidate)
                    .ok()
                    .and_then(|cookie| cookie.reply().ok())?;
                Some(format!(
                    "{}/geom={},{} {}x{}/contains={}",
                    self.window_diagnostics(candidate),
                    geometry.x,
                    geometry.y,
                    geometry.width,
                    geometry.height,
                    contains_point(
                        geometry.x,
                        geometry.y,
                        geometry.width,
                        geometry.height,
                        root_x,
                        root_y,
                    )
                ))
            })
            .take(4)
            .collect::<Vec<_>>();

        if candidates.is_empty() {
            "none".to_string()
        } else {
            candidates.join(" | ")
        }
    }

    fn window_chain(&self, window: XWindow, root: XWindow) -> Vec<String> {
        let mut chain = Vec::new();
        let mut current = Some(window);
        let mut depth = 0;
        while let Some(window) = current {
            chain.push(format!(
                "{}/aware={}",
                self.window_diagnostics(window),
                self.is_xdnd_aware(window).unwrap_or(false)
            ));
            if window == root || depth >= 12 {
                break;
            }
            current = self.parent_of(window).ok().flatten();
            depth += 1;
        }
        chain
    }

    fn window_diagnostics(&self, window: XWindow) -> String {
        let class = self
            .string_property(window, self.atoms.wm_class, AtomEnum::STRING.into())
            .unwrap_or_else(|| "?".to_string());
        let title = self
            .string_property(window, self.atoms.net_wm_name, self.atoms.utf8_string)
            .or_else(|| self.string_property(window, self.atoms.wm_name, AtomEnum::STRING.into()))
            .unwrap_or_else(|| "?".to_string());
        format!("0x{window:x} class={class:?} title={title:?}")
    }

    fn atom_name(&self, atom: Atom) -> String {
        self.conn
            .get_atom_name(atom)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| {
                format!(
                    "{}({atom})",
                    String::from_utf8_lossy(&reply.name).into_owned()
                )
            })
            .unwrap_or_else(|| atom.to_string())
    }

    fn string_property(
        &self,
        window: XWindow,
        property: Atom,
        property_type: Atom,
    ) -> Option<String> {
        let reply = self
            .conn
            .get_property(false, window, property, property_type, 0, 256)
            .ok()?
            .reply()
            .ok()?;
        if reply.value.is_empty() {
            return None;
        }
        let mut text = String::from_utf8_lossy(&reply.value).replace('\0', "/");
        while text.ends_with('/') {
            text.pop();
        }
        Some(text)
    }

    fn window_at(
        &self,
        mut window: XWindow,
        _root_x: i16,
        _root_y: i16,
    ) -> Result<Option<XWindow>, String> {
        loop {
            let pointer = self
                .conn
                .query_pointer(window)
                .map_err(|err| err.to_string())?
                .reply()
                .map_err(|err| err.to_string())?;

            if pointer.child == x11rb::NONE {
                return Ok((window != self.source_window).then_some(window));
            }

            window = pointer.child;
            if window == self.source_window || self.is_origin_window(window) {
                return Ok(None);
            }
        }
    }

    fn is_origin_window(&self, window: XWindow) -> bool {
        self.origin_window == Some(window)
    }

    fn parent_of(&self, window: XWindow) -> Result<Option<XWindow>, String> {
        let tree = self
            .conn
            .query_tree(window)
            .map_err(|err| err.to_string())?
            .reply()
            .map_err(|err| err.to_string())?;
        Ok((tree.parent != x11rb::NONE).then_some(tree.parent))
    }

    fn is_xdnd_aware(&self, window: XWindow) -> Result<bool, String> {
        let property = self
            .conn
            .get_property(false, window, self.atoms.xdnd_aware, AtomEnum::ANY, 0, 1)
            .map_err(|err| err.to_string())?
            .reply()
            .map_err(|err| err.to_string())?;
        Ok(property.value_len > 0)
    }

    fn xdnd_proxy(&self, window: XWindow) -> Result<Option<XWindow>, String> {
        let property = self
            .conn
            .get_property(false, window, self.atoms.xdnd_proxy, AtomEnum::WINDOW, 0, 1)
            .map_err(|err| err.to_string())?
            .reply()
            .map_err(|err| err.to_string())?;
        Ok(property.value32().and_then(|mut values| values.next()))
    }

    fn send_enter(&self, target: XWindow) -> Result<(), String> {
        let targets = self.offered_targets();
        let enter_targets = self.enter_targets(&targets);
        self.conn
            .change_property32(
                PropMode::REPLACE,
                self.source_window,
                self.atoms.xdnd_type_list,
                AtomEnum::ATOM,
                &targets,
            )
            .map_err(|err| err.to_string())?;

        self.send_client_message(
            target,
            self.atoms.xdnd_enter,
            [
                self.source_window,
                (XDND_VERSION << 24) | 1,
                enter_targets[0],
                enter_targets[1],
                enter_targets[2],
            ],
        )
    }

    fn offered_targets(&self) -> Vec<Atom> {
        self.atoms
            .mime_targets()
            .offered_targets(self.portal_filetransfer_key.is_some())
    }

    fn enter_targets(&self, targets: &[Atom]) -> [Atom; 3] {
        self.atoms.mime_targets().enter_targets(targets)
    }

    fn is_portal_filetransfer_target(&self, target: Atom) -> bool {
        self.portal_filetransfer_key.is_some() && self.atoms.mime_targets().is_portal_target(target)
    }

    fn send_position(&self, target: XWindow, root_x: i16, root_y: i16) -> Result<(), String> {
        let xy = ((root_x as u32) << 16) | (root_y as u16 as u32);
        self.send_client_message(
            target,
            self.atoms.xdnd_position,
            [
                self.source_window,
                0,
                xy,
                self.last_event_time,
                self.atoms.xdnd_action_copy,
            ],
        )
    }

    fn leave_current_target(&mut self) -> Result<(), String> {
        if let Some(target) = self.current_target.take() {
            self.send_client_message(
                target,
                self.atoms.xdnd_leave,
                [self.source_window, 0, 0, 0, 0],
            )?;
        }
        self.accepted_target = None;
        Ok(())
    }

    fn send_client_message(
        &self,
        target: XWindow,
        message_type: Atom,
        data: [u32; 5],
    ) -> Result<(), String> {
        let event = ClientMessageEvent::new(32, target, message_type, data);
        self.conn
            .send_event(false, target, EventMask::NO_EVENT, event)
            .map_err(|err| err.to_string())?;
        self.conn.flush().map_err(|err| err.to_string())
    }
}

fn primary_down(mask: KeyButMask) -> bool {
    mask.contains(KeyButMask::BUTTON1)
}

fn server_time(conn: &RustConnection, window: XWindow, property: Atom) -> Result<u32, String> {
    conn.change_property8(PropMode::APPEND, window, property, AtomEnum::STRING, &[0])
        .map_err(|err| err.to_string())?;
    conn.flush().map_err(|err| err.to_string())?;

    loop {
        match conn.wait_for_event().map_err(|err| err.to_string())? {
            Event::PropertyNotify(event) if event.window == window && event.atom == property => {
                return Ok(event.time);
            }
            _ => {}
        }
    }
}

fn atom(conn: &RustConnection, name: &[u8]) -> Result<Atom, String> {
    conn.intern_atom(false, name)
        .map_err(|err| err.to_string())?
        .reply()
        .map(|reply| reply.atom)
        .map_err(|err| err.to_string())
}

fn set_window_identity(
    conn: &RustConnection,
    window: XWindow,
    atoms: &XdndAtoms,
    title: &[u8],
) -> Result<(), String> {
    conn.change_property8(
        PropMode::REPLACE,
        window,
        atoms.wm_class,
        AtomEnum::STRING,
        b"audio-plugin-dnd\0AUDIO-PLUGIN-DND\0",
    )
    .map_err(|err| err.to_string())?;
    conn.change_property8(
        PropMode::REPLACE,
        window,
        atoms.wm_name,
        atoms.utf8_string,
        title,
    )
    .map_err(|err| err.to_string())?;
    conn.change_property8(
        PropMode::REPLACE,
        window,
        atoms.net_wm_name,
        atoms.utf8_string,
        title,
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}
