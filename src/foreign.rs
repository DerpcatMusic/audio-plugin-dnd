use std::ffi::c_void;
use std::fmt;

use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
use wayland_client::{
    backend::{Backend, ObjectId},
    protocol::wl_surface::WlSurface,
    Connection, Proxy,
};

#[derive(Debug)]
pub enum ForeignWaylandParentError {
    MissingDisplay,
    MissingSurface,
    InvalidDisplay,
    InvalidSurface,
    UnsupportedDisplayHandle(RawDisplayHandle),
    UnsupportedWindowHandle(RawWindowHandle),
}

impl fmt::Display for ForeignWaylandParentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingDisplay => formatter.write_str("Wayland parent is missing wl_display"),
            Self::MissingSurface => formatter.write_str("Wayland parent is missing wl_surface"),
            Self::InvalidDisplay => formatter.write_str("Wayland parent wl_display is invalid"),
            Self::InvalidSurface => formatter.write_str("Wayland parent wl_surface is invalid"),
            Self::UnsupportedDisplayHandle(handle) => {
                write!(formatter, "unsupported parent display handle: {handle:?}")
            }
            Self::UnsupportedWindowHandle(handle) => {
                write!(formatter, "unsupported parent window handle: {handle:?}")
            }
        }
    }
}

impl std::error::Error for ForeignWaylandParentError {}

#[derive(Debug, Clone)]
pub struct ForeignWaylandParent {
    connection: Connection,
    surface: WlSurface,
    display_ptr: *mut c_void,
    surface_ptr: *mut c_void,
}

impl ForeignWaylandParent {
    pub fn from_raw_handles(
        display_handle: RawDisplayHandle,
        window_handle: RawWindowHandle,
    ) -> Result<Self, ForeignWaylandParentError> {
        let RawDisplayHandle::Wayland(display_handle) = display_handle else {
            return Err(ForeignWaylandParentError::UnsupportedDisplayHandle(
                display_handle,
            ));
        };
        let RawWindowHandle::Wayland(window_handle) = window_handle else {
            return Err(ForeignWaylandParentError::UnsupportedWindowHandle(
                window_handle,
            ));
        };

        let display_ptr = display_handle.display;
        if display_ptr.is_null() {
            return Err(ForeignWaylandParentError::MissingDisplay);
        }

        let surface_ptr = window_handle.surface;
        if surface_ptr.is_null() {
            return Err(ForeignWaylandParentError::MissingSurface);
        }

        // SAFETY: The host owns this wl_display and promises through raw-window-handle
        // that the pointer remains valid for the lifetime of the parented editor. The
        // resulting Backend borrows the foreign display; baseview must not disconnect it.
        let backend = unsafe { Backend::from_foreign_display(display_ptr.cast()) };
        let connection = Connection::from_backend(backend);

        // SAFETY: The host owns this wl_surface. We only create a typed proxy handle
        // for protocol requests that need to reference the parent/origin surface.
        let surface_id = unsafe { ObjectId::from_ptr(WlSurface::interface(), surface_ptr.cast()) }
            .map_err(|_| ForeignWaylandParentError::InvalidSurface)?;
        let surface = WlSurface::from_id(&connection, surface_id)
            .map_err(|_| ForeignWaylandParentError::InvalidSurface)?;

        Ok(Self {
            connection,
            surface,
            display_ptr,
            surface_ptr,
        })
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn surface(&self) -> &WlSurface {
        &self.surface
    }

    pub fn display_ptr(&self) -> *mut c_void {
        self.display_ptr
    }

    pub fn surface_ptr(&self) -> *mut c_void {
        self.surface_ptr
    }
}

/// Return the Wayland display backing a raw `wl_surface` pointer.
///
/// CLAP's Wayland GUI handle supplies a surface pointer, while baseview's raw
/// parent handle needs both display and surface. `wl_surface` is a proxy object,
/// so libwayland can recover the display associated with that proxy.
///
/// # Safety
///
/// `surface` must be either null or a valid live Wayland proxy pointer for a
/// `wl_surface`. Passing any other pointer is undefined behavior in libwayland.
pub unsafe fn display_from_surface_ptr(surface: *mut std::ffi::c_void) -> *mut std::ffi::c_void {
    if surface.is_null() {
        return std::ptr::null_mut();
    }

    let Some(client) = wayland_sys::client::wayland_client_option() else {
        return std::ptr::null_mut();
    };

    unsafe { (client.wl_proxy_get_display)(surface.cast::<wayland_sys::client::wl_proxy>()).cast() }
}
