//! XdndProxy drop router for plugin editors embedded in XWayland hosts.
//!
//! Compositor Wayland-to-X11 DnD bridges (Hyprland's `CX11DataDevice`, and
//! the wlroots lineage it derives from) send XdndEnter/Position/Drop only to
//! the toplevel X11 window of the XWayland surface under the cursor — the
//! host DAW's window. Unlike real X11 drag sources, they never descend the
//! window tree, so an embedded plugin editor with its own `XdndAware` child
//! window never sees bridged drags from native Wayland apps.
//!
//! The bridges do, however, honor the `XdndProxy` property on that toplevel
//! (Hyprland verifies the XDND-spec self-pointer). This router exploits that:
//!
//! 1. Create a hidden 1x1 router window with `XdndAware` and a self-pointing
//!    `XdndProxy`, plus a marker property identifying it as ours.
//! 2. Set `XdndProxy` on the host toplevel to the router window.
//! 3. Relay each XdndEnter/Position/Leave/Drop the bridge sends to the router
//!    onto the deepest `XdndAware` window under the drag coordinates — the
//!    plugin editor when the drag is over it, the host toplevel otherwise —
//!    switching targets with proper Leave/Enter pairs like a real source.
//!
//! XdndStatus and XdndFinished flow directly between the real target and the
//! bridge's source window (`data.l[0]` is preserved), so the router never has
//! to proxy replies. Host-internal drags are unaffected: real X11 sources
//! find embedded `XdndAware` children through their own tree descent and
//! never consult the toplevel's proxy.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use raw_window_handle::RawWindowHandle;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageEvent, ConnectionExt, CreateWindowAux, EventMask, PropMode,
    Window as XWindow, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use super::atom;
use crate::backend::emit_backend_event;
use crate::backend::{DragWindow, ExternalDragError};

const XDND_VERSION: u32 = 5;
const CLAIM_INTERVAL: Duration = Duration::from_secs(2);
const POLL_SLEEP: Duration = Duration::from_millis(10);

/// Handle for an installed drop router. Uninstalls on drop.
#[derive(Debug)]
pub struct XdndDropRouter {
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl XdndDropRouter {
    /// Install a drop router for the given plugin editor window.
    ///
    /// Only meaningful for X11/XWayland editor windows running inside a
    /// Wayland session; other configurations return `BackendUnavailable` and
    /// adapters should treat that as a no-op.
    ///
    /// # Errors
    ///
    /// Returns an error when the window handle is not X11 or the router
    /// thread cannot be spawned.
    pub fn install(window: DragWindow) -> Result<Self, ExternalDragError> {
        let editor_window = match window.window() {
            RawWindowHandle::Xlib(handle) if handle.window != 0 => handle.window as XWindow,
            RawWindowHandle::Xcb(handle) if handle.window != 0 => handle.window,
            other => {
                return Err(ExternalDragError::BackendUnavailable(format!(
                    "drop router requires an X11/XWayland editor window, got {other:?}"
                )));
            }
        };

        if std::env::var_os("WAYLAND_DISPLAY").is_none() {
            return Err(ExternalDragError::BackendUnavailable(
                "drop router is only needed under XWayland (no WAYLAND_DISPLAY)".to_string(),
            ));
        }
        if std::env::var("AUDIO_PLUGIN_DND_ROUTER")
            .map(|value| matches!(value.to_ascii_lowercase().as_str(), "0" | "false" | "off"))
            .unwrap_or(false)
        {
            return Err(ExternalDragError::BackendUnavailable(
                "drop router disabled by AUDIO_PLUGIN_DND_ROUTER".to_string(),
            ));
        }

        let running = Arc::new(AtomicBool::new(true));
        let thread_running = Arc::clone(&running);
        let thread = thread::Builder::new()
            .name("audio-plugin-dnd-drop-router".to_string())
            .spawn(move || match RouterWorker::new(editor_window) {
                Ok(worker) => worker.run(&thread_running),
                Err(err) => {
                    emit_backend_event(format!("[dnd-router] install failed: {err}"));
                }
            })
            .map_err(|err| ExternalDragError::StartFailed(err.to_string()))?;

        Ok(Self {
            running,
            thread: Some(thread),
        })
    }
}

impl Drop for XdndDropRouter {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct RouterAtoms {
    xdnd_aware: Atom,
    xdnd_proxy: Atom,
    xdnd_enter: Atom,
    xdnd_position: Atom,
    xdnd_leave: Atom,
    xdnd_drop: Atom,
    marker: Atom,
    wm_class: Atom,
    wm_name: Atom,
}

impl RouterAtoms {
    fn new(conn: &RustConnection) -> Result<Self, String> {
        Ok(Self {
            xdnd_aware: atom(conn, b"XdndAware")?,
            xdnd_proxy: atom(conn, b"XdndProxy")?,
            xdnd_enter: atom(conn, b"XdndEnter")?,
            xdnd_position: atom(conn, b"XdndPosition")?,
            xdnd_leave: atom(conn, b"XdndLeave")?,
            xdnd_drop: atom(conn, b"XdndDrop")?,
            marker: atom(conn, b"AUDIO_PLUGIN_DND_ROUTER")?,
            wm_class: atom(conn, b"WM_CLASS")?,
            wm_name: atom(conn, b"WM_NAME")?,
        })
    }
}

struct RouterWorker {
    conn: RustConnection,
    root: XWindow,
    atoms: RouterAtoms,
    router_window: XWindow,
    toplevel: XWindow,
    /// True while this router owns the toplevel's XdndProxy property.
    claimed: bool,
    /// Cached XdndEnter payload from the bridge, replayed on target switches.
    active_enter: Option<[u32; 5]>,
    current_target: Option<XWindow>,
}

impl RouterWorker {
    fn new(editor_window: XWindow) -> Result<Self, String> {
        let (conn, screen_num) = RustConnection::connect(None).map_err(|err| err.to_string())?;
        let atoms = RouterAtoms::new(&conn)?;
        let screen = &conn.setup().roots[screen_num];
        let root = screen.root;

        let toplevel = toplevel_of(&conn, root, editor_window)?;

        let router_window = conn.generate_id().map_err(|err| err.to_string())?;
        conn.create_window(
            screen.root_depth,
            router_window,
            root,
            -100,
            -100,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            screen.root_visual,
            &CreateWindowAux::new().override_redirect(1),
        )
        .map_err(|err| err.to_string())?;

        conn.change_property32(
            PropMode::REPLACE,
            router_window,
            atoms.xdnd_aware,
            AtomEnum::ATOM,
            &[XDND_VERSION],
        )
        .map_err(|err| err.to_string())?;
        // XDND spec: a proxy window must carry XdndProxy pointing at itself.
        // Hyprland's getProxyWindow verifies exactly this before honoring it.
        conn.change_property32(
            PropMode::REPLACE,
            router_window,
            atoms.xdnd_proxy,
            AtomEnum::WINDOW,
            &[router_window],
        )
        .map_err(|err| err.to_string())?;
        conn.change_property32(
            PropMode::REPLACE,
            router_window,
            atoms.marker,
            AtomEnum::CARDINAL,
            &[1],
        )
        .map_err(|err| err.to_string())?;
        conn.change_property8(
            PropMode::REPLACE,
            router_window,
            atoms.wm_class,
            AtomEnum::STRING,
            b"audio-plugin-dnd-router\0AUDIO-PLUGIN-DND-ROUTER\0",
        )
        .map_err(|err| err.to_string())?;
        conn.change_property8(
            PropMode::REPLACE,
            router_window,
            atoms.wm_name,
            AtomEnum::STRING,
            b"Audio Plugin DND Router",
        )
        .map_err(|err| err.to_string())?;
        conn.flush().map_err(|err| err.to_string())?;

        Ok(Self {
            conn,
            root,
            atoms,
            router_window,
            toplevel,
            claimed: false,
            active_enter: None,
            current_target: None,
        })
    }

    fn run(mut self, running: &AtomicBool) {
        self.claim_proxy_if_available();
        emit_backend_event(format!(
            "[dnd-router] installed: router=0x{:x}, toplevel=0x{:x}, claimed={}",
            self.router_window, self.toplevel, self.claimed
        ));

        let mut last_claim_check = Instant::now();
        while running.load(Ordering::Acquire) {
            match self.conn.poll_for_event() {
                Ok(Some(Event::ClientMessage(event))) if event.window == self.router_window => {
                    if let Err(err) = self.handle_client_message(&event) {
                        emit_backend_event(format!("[dnd-router] relay error: {err}"));
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => {
                    if last_claim_check.elapsed() >= CLAIM_INTERVAL {
                        last_claim_check = Instant::now();
                        if !self.toplevel_alive() {
                            emit_backend_event(
                                "[dnd-router] host toplevel destroyed; stopping".to_string(),
                            );
                            break;
                        }
                        self.claim_proxy_if_available();
                    }
                    thread::sleep(POLL_SLEEP);
                }
                Err(err) => {
                    emit_backend_event(format!("[dnd-router] connection lost: {err}"));
                    return;
                }
            }
        }

        self.uninstall();
    }

    fn handle_client_message(&mut self, event: &ClientMessageEvent) -> Result<(), String> {
        let data = event.data.as_data32();

        if super::has_active_outbound_drag()
            && matches!(
                event.type_,
                atom if atom == self.atoms.xdnd_enter
                    || atom == self.atoms.xdnd_position
                    || atom == self.atoms.xdnd_leave
                    || atom == self.atoms.xdnd_drop
            )
        {
            if event.type_ == self.atoms.xdnd_drop {
                emit_backend_event(format!(
                    "[dnd-router] ignored outbound self-drop from source 0x{:x}",
                    data[0]
                ));
            }
            self.active_enter = None;
            self.current_target = None;
            return Ok(());
        }

        if event.type_ == self.atoms.xdnd_enter {
            // Cache and forward lazily: Enter carries no coordinates, so the
            // real target is unknown until the first Position arrives.
            self.active_enter = Some(data);
            self.current_target = None;
            return Ok(());
        }

        if event.type_ == self.atoms.xdnd_position {
            let packed = data[2];
            let x = (packed >> 16) as i16;
            let y = (packed & 0xffff) as i16;
            let target = self.resolve_target(x, y)?;

            if self.current_target != Some(target) {
                self.switch_target(target, data[0])?;
            }
            return self.forward(target, self.atoms.xdnd_position, data);
        }

        if event.type_ == self.atoms.xdnd_leave {
            if let Some(target) = self.current_target.take() {
                self.forward(target, self.atoms.xdnd_leave, data)?;
            }
            self.active_enter = None;
            return Ok(());
        }

        if event.type_ == self.atoms.xdnd_drop {
            if let Some(target) = self.current_target {
                emit_backend_event(format!(
                    "[dnd-router] drop relayed to 0x{target:x} (source 0x{:x})",
                    data[0]
                ));
                self.forward(target, self.atoms.xdnd_drop, data)?;
            }
            self.active_enter = None;
            self.current_target = None;
            return Ok(());
        }

        Ok(())
    }

    fn switch_target(&mut self, target: XWindow, source: u32) -> Result<(), String> {
        if let Some(previous) = self.current_target.take() {
            self.forward(previous, self.atoms.xdnd_leave, [source, 0, 0, 0, 0])?;
        }
        if let Some(enter) = self.active_enter {
            self.forward(target, self.atoms.xdnd_enter, enter)?;
        }
        self.current_target = Some(target);
        Ok(())
    }

    /// Deepest `XdndAware` window under the given root coordinates, the way a
    /// real XDND source would find it — never the router itself.
    fn resolve_target(&self, root_x: i16, root_y: i16) -> Result<XWindow, String> {
        let mut window = self.root;
        loop {
            let reply = self
                .conn
                .translate_coordinates(self.root, window, root_x, root_y)
                .map_err(|err| err.to_string())?
                .reply()
                .map_err(|err| err.to_string())?;
            if reply.child == x11rb::NONE || reply.child == self.router_window {
                break;
            }
            window = reply.child;
        }

        let mut candidate = window;
        loop {
            if candidate != self.router_window && self.is_xdnd_aware(candidate)? {
                return Ok(candidate);
            }
            if candidate == self.root {
                break;
            }
            let tree = self
                .conn
                .query_tree(candidate)
                .map_err(|err| err.to_string())?
                .reply()
                .map_err(|err| err.to_string())?;
            if tree.parent == x11rb::NONE {
                break;
            }
            candidate = tree.parent;
        }

        Ok(self.toplevel)
    }

    fn forward(&self, target: XWindow, message_type: Atom, data: [u32; 5]) -> Result<(), String> {
        let event = ClientMessageEvent::new(32, target, message_type, data);
        self.conn
            .send_event(false, target, EventMask::NO_EVENT, event)
            .map_err(|err| err.to_string())?;
        self.conn.flush().map_err(|err| err.to_string())
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

    fn read_toplevel_proxy(&self) -> Option<XWindow> {
        self.conn
            .get_property(
                false,
                self.toplevel,
                self.atoms.xdnd_proxy,
                AtomEnum::WINDOW,
                0,
                1,
            )
            .ok()?
            .reply()
            .ok()?
            .value32()
            .and_then(|mut values| values.next())
    }

    fn window_alive(&self, window: XWindow) -> bool {
        self.conn
            .get_geometry(window)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .is_some()
    }

    fn toplevel_alive(&self) -> bool {
        self.window_alive(self.toplevel)
    }

    /// Claim the toplevel's XdndProxy unless a live router (ours or a
    /// sibling plugin instance's) or a foreign proxy already holds it.
    fn claim_proxy_if_available(&mut self) {
        match self.read_toplevel_proxy() {
            Some(proxy) if proxy == self.router_window => {
                self.claimed = true;
            }
            Some(proxy) if self.window_alive(proxy) => {
                // A sibling instance's router (or the host's own proxy) is
                // active; it forwards by tree descent, which covers this
                // editor too. Stand by until it disappears.
                self.claimed = false;
            }
            _ => {
                let claim = self
                    .conn
                    .change_property32(
                        PropMode::REPLACE,
                        self.toplevel,
                        self.atoms.xdnd_proxy,
                        AtomEnum::WINDOW,
                        &[self.router_window],
                    )
                    .and_then(|_| self.conn.flush());
                if claim.is_ok() {
                    if !self.claimed {
                        emit_backend_event(format!(
                            "[dnd-router] claimed XdndProxy on toplevel 0x{:x}",
                            self.toplevel
                        ));
                    }
                    self.claimed = true;
                }
            }
        }
    }

    fn uninstall(&mut self) {
        if self.read_toplevel_proxy() == Some(self.router_window) {
            let _ = self
                .conn
                .delete_property(self.toplevel, self.atoms.xdnd_proxy);
        }
        let _ = self.conn.destroy_window(self.router_window);
        let _ = self.conn.flush();
        emit_backend_event(format!(
            "[dnd-router] uninstalled from toplevel 0x{:x}",
            self.toplevel
        ));
    }
}

/// Walk up the tree to the window whose parent is the root: the host
/// toplevel the compositor bridge targets.
fn toplevel_of(
    conn: &RustConnection,
    root: XWindow,
    editor_window: XWindow,
) -> Result<XWindow, String> {
    let mut window = editor_window;
    for _ in 0..64 {
        let tree = conn
            .query_tree(window)
            .map_err(|err| err.to_string())?
            .reply()
            .map_err(|err| err.to_string())?;
        if tree.parent == root || tree.parent == x11rb::NONE {
            return Ok(window);
        }
        window = tree.parent;
    }
    Ok(window)
}
