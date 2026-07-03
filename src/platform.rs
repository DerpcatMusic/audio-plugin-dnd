//! Platform backend route names and diagnostics.

/// Native backend used to start a drag.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DragBackendKind {
    X11Xdnd,
    WaylandDataDevice,
    WindowsOle,
    MacosAppKit,
    Unsupported,
}

impl DragBackendKind {
    /// Stable machine-readable name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::X11Xdnd => "x11_xdnd",
            Self::WaylandDataDevice => "wayland_data_device",
            Self::WindowsOle => "windows_ole",
            Self::MacosAppKit => "macos_appkit",
            Self::Unsupported => "unsupported",
        }
    }

    /// Human-readable backend summary.
    #[must_use]
    pub const fn summary(self) -> &'static str {
        match self {
            Self::X11Xdnd => "X11/XWayland XDND",
            Self::WaylandDataDevice => "native Wayland data-device",
            Self::WindowsOle => "Windows OLE",
            Self::MacosAppKit => "macOS AppKit",
            Self::Unsupported => "unsupported backend",
        }
    }
}

/// Source or expected target endpoint kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DragEndpointKind {
    X11Window,
    XwaylandWindow,
    WaylandSurface,
    CompositorBridge,
    Unknown,
}

impl DragEndpointKind {
    /// Human-readable endpoint summary.
    #[must_use]
    pub const fn summary(self) -> &'static str {
        match self {
            Self::X11Window => "X11 window",
            Self::XwaylandWindow => "XWayland window",
            Self::WaylandSurface => "Wayland surface",
            Self::CompositorBridge => "compositor bridge",
            Self::Unknown => "unknown endpoint",
        }
    }
}

/// High-level route a drag is expected to take.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DragRoute {
    X11ToX11,
    XwaylandToXwayland,
    XwaylandToWaylandBridge,
    WaylandToWayland,
    WaylandToXwaylandBridge,
    Unsupported,
}

impl DragRoute {
    /// Backend responsible for this route.
    #[must_use]
    pub const fn backend(self) -> DragBackendKind {
        match self {
            Self::X11ToX11 | Self::XwaylandToXwayland | Self::XwaylandToWaylandBridge => {
                DragBackendKind::X11Xdnd
            }
            Self::WaylandToWayland | Self::WaylandToXwaylandBridge => {
                DragBackendKind::WaylandDataDevice
            }
            Self::Unsupported => DragBackendKind::Unsupported,
        }
    }

    /// Human-readable route summary.
    #[must_use]
    pub const fn summary(self) -> &'static str {
        match self {
            Self::X11ToX11 => "X11 source to X11 target through XDND",
            Self::XwaylandToXwayland => "XWayland source to XWayland target through XDND",
            Self::XwaylandToWaylandBridge => {
                "XWayland source to native Wayland target through compositor bridge"
            }
            Self::WaylandToWayland => "native Wayland source to native Wayland target",
            Self::WaylandToXwaylandBridge => {
                "native Wayland source to XWayland target through compositor bridge"
            }
            Self::Unsupported => "unsupported drag route",
        }
    }
}

/// Backend route plan for diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DragBackendPlan {
    pub route: DragRoute,
    pub source: DragEndpointKind,
    pub expected_target: DragEndpointKind,
}

impl DragBackendPlan {
    /// Build a route plan.
    #[must_use]
    pub const fn new(
        route: DragRoute,
        source: DragEndpointKind,
        expected_target: DragEndpointKind,
    ) -> Self {
        Self {
            route,
            source,
            expected_target,
        }
    }

    /// Human-readable diagnostic summary.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "{} via {}; source={}, expected_target={}",
            self.route.summary(),
            self.route.backend().summary(),
            self.source.summary(),
            self.expected_target.summary()
        )
    }
}

/// Completion status reported by a backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragCompletion {
    Confirmed,
    Inferred,
    Failed(DragFailureKind),
}

impl DragCompletion {
    /// True when the backend considers the drag successful.
    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Confirmed | Self::Inferred)
    }
}

/// Common drag failure categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragFailureKind {
    NoTarget,
    BridgeRejected,
    TargetNoData,
    BackendUnavailable,
    Cancelled,
    Other,
}
