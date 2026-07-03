use std::fmt;
use std::path::PathBuf;

/// A single native Wayland drag data offer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WaylandDragOffer {
    mime_type: String,
    data: Vec<u8>,
}

impl WaylandDragOffer {
    /// Build a Wayland data-device offer for one MIME type.
    #[must_use]
    pub fn new(mime_type: impl Into<String>, data: impl Into<Vec<u8>>) -> Self {
        Self {
            mime_type: mime_type.into(),
            data: data.into(),
        }
    }

    /// MIME type advertised through `wl_data_source.offer`.
    #[must_use]
    pub fn mime_type(&self) -> &str {
        &self.mime_type
    }

    /// Bytes to write when the compositor calls `wl_data_source.send`.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

/// Native Wayland drag request owned by the windowing backend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WaylandExternalDragRequest {
    offers: Vec<WaylandDragOffer>,
    fallback_file_paths: Vec<PathBuf>,
}

impl WaylandExternalDragRequest {
    /// Build a native Wayland file drag request.
    #[must_use]
    pub fn new(offers: Vec<WaylandDragOffer>, fallback_file_paths: Vec<PathBuf>) -> Self {
        Self {
            offers,
            fallback_file_paths,
        }
    }

    /// MIME offers that should be advertised by `wl_data_source`.
    #[must_use]
    pub fn offers(&self) -> &[WaylandDragOffer] {
        &self.offers
    }

    /// Original file paths retained for diagnostics and manual import fallback.
    #[must_use]
    pub fn fallback_file_paths(&self) -> &[PathBuf] {
        &self.fallback_file_paths
    }
}

/// Why the current window cannot start a native Wayland external drag.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaylandExternalDragError {
    /// The active window platform is not a native Wayland backend.
    UnsupportedBackend,
    /// The backend has no active `wl_surface` for the plugin editor.
    MissingSurface,
    /// The backend has no active `wl_seat`/data-device pair.
    MissingSeat,
    /// The backend does not have the pointer button serial that initiated the drag.
    MissingPointerButtonSerial,
    /// The request did not contain any MIME offers.
    EmptyOfferSet,
}

impl fmt::Display for WaylandExternalDragError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedBackend => "native Wayland drag is not supported by this backend",
            Self::MissingSurface => "native Wayland drag is missing wl_surface",
            Self::MissingSeat => "native Wayland drag is missing wl_seat/data-device",
            Self::MissingPointerButtonSerial => {
                "native Wayland drag is missing the initiating pointer button serial"
            }
            Self::EmptyOfferSet => "native Wayland drag has no MIME offers",
        })
    }
}

impl std::error::Error for WaylandExternalDragError {}
