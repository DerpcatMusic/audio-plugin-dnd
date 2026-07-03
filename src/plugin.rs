//! Plugin drag lifecycle primitives.
//!
//! The crate does not render audio or MIDI. A plugin should render its export
//! to a temp file on the GUI/background side, then queue that path with
//! [`ExternalDragQueue`](crate::ExternalDragQueue). This module owns the shared
//! arming threshold, preview metadata, render cache slot, short drag flash, and
//! self-drop tracking behavior.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::queue::{ExternalDragPreview, ExternalDragQueue};

/// Pointer distance before an armed gesture becomes an external drag.
pub const DRAG_START_DISTANCE_POINTS: f32 = 10.0;

/// Toolkit-neutral 2D point in logical UI points.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    /// Build a point.
    #[must_use]
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    /// Euclidean distance to another point.
    #[must_use]
    pub fn distance(self, other: Self) -> f32 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        (dx * dx + dy * dy).sqrt()
    }
}

/// Cached render path for repeated audio drags.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderCache<K> {
    pub key: K,
    pub path: PathBuf,
}

/// Type of file payload being dragged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DragPayloadKind {
    Audio,
    Midi,
    Other,
}

/// Preview metadata used by UI ghosts and optional platform drag previews.
#[derive(Clone, Debug, PartialEq)]
pub enum DragPreview {
    Audio(Vec<(f32, f32)>),
    Spectral(SpectralDragPreview),
    Midi,
    None,
}

impl DragPreview {
    /// Convert to queue-level platform preview metadata.
    #[must_use]
    pub fn external_preview(&self) -> Option<ExternalDragPreview> {
        match self {
            Self::Audio(buckets) => Some(ExternalDragPreview::Waveform {
                buckets: buckets.clone(),
            }),
            Self::Spectral(preview) => Some(ExternalDragPreview::Spectral {
                columns: preview.columns,
                rows: preview.rows,
                energy: preview.energy.clone(),
                low_hz: preview.low_hz,
                high_hz: preview.high_hz,
            }),
            Self::Midi | Self::None => None,
        }
    }
}

/// Compact spectral preview for an audio selection.
#[derive(Clone, Debug, PartialEq)]
pub struct SpectralDragPreview {
    pub columns: usize,
    pub rows: usize,
    pub energy: Vec<f32>,
    pub low_hz: f32,
    pub high_hz: f32,
}

/// A pointer gesture that may become an external drag.
#[derive(Clone, Debug)]
pub struct DragArmed {
    pub started: Instant,
    pub origin: Point,
    pub kind: DragPayloadKind,
    pub label: String,
    pub preview: DragPreview,
}

/// Short UI feedback after a queued drag.
#[derive(Clone, Debug)]
pub struct DragFlash {
    pub until: Instant,
    pub reused: bool,
    pub kind: DragPayloadKind,
    pub label: String,
    pub preview: DragPreview,
}

/// Result of queueing a file drag.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DragStart {
    pub path: PathBuf,
    pub reused: bool,
}

/// Status text a plugin may show after a drag attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DragStatus {
    Queued { label: String, reused: bool },
    Failed(String),
}

/// Shared drag/export state for plugin UIs.
#[derive(Clone, Debug)]
pub struct DragExportState<K = String> {
    render_cache: Option<RenderCache<K>>,
    flash: Option<DragFlash>,
    armed: Option<DragArmed>,
    own_export_paths: Vec<PathBuf>,
    own_export_path_limit: usize,
}

impl<K> Default for DragExportState<K> {
    fn default() -> Self {
        Self {
            render_cache: None,
            flash: None,
            armed: None,
            own_export_paths: Vec::new(),
            own_export_path_limit: 8,
        }
    }
}

impl<K> DragExportState<K> {
    /// Current render cache.
    #[must_use]
    pub fn render_cache(&self) -> Option<&RenderCache<K>> {
        self.render_cache.as_ref()
    }

    /// Replace the render cache.
    pub fn set_render_cache(&mut self, cache: RenderCache<K>) {
        self.render_cache = Some(cache);
    }

    /// Clear the render cache.
    pub fn clear_cache(&mut self) {
        self.render_cache = None;
    }

    /// Current armed gesture.
    #[must_use]
    pub fn armed(&self) -> Option<&DragArmed> {
        self.armed.as_ref()
    }

    /// Current post-drag UI flash.
    #[must_use]
    pub fn flash(&self) -> Option<&DragFlash> {
        self.flash.as_ref()
    }

    /// Arm a possible drag gesture.
    pub fn arm(
        &mut self,
        origin: Point,
        kind: DragPayloadKind,
        label: impl Into<String>,
        preview: DragPreview,
    ) {
        self.armed = Some(DragArmed {
            started: Instant::now(),
            origin,
            kind,
            label: label.into(),
            preview,
        });
    }

    /// Arm an audio drag.
    pub fn arm_audio(&mut self, origin: Point, label: impl Into<String>, preview: DragPreview) {
        self.arm(origin, DragPayloadKind::Audio, label, preview);
    }

    /// Arm a MIDI drag.
    pub fn arm_midi(&mut self, origin: Point, label: impl Into<String>) {
        self.arm(origin, DragPayloadKind::Midi, label, DragPreview::Midi);
    }

    /// Cancel an armed drag.
    pub fn cancel_armed_drag(&mut self) {
        self.armed = None;
    }

    /// Check whether the pointer has moved far enough to start the drag.
    #[must_use]
    pub fn armed_drag_ready(&self, pointer: Point) -> bool {
        self.armed
            .as_ref()
            .is_some_and(|armed| pointer.distance(armed.origin) >= DRAG_START_DISTANCE_POINTS)
    }

    /// Track paths exported by this plugin so self-drop can be ignored.
    pub fn remember_export_path(&mut self, path: impl Into<PathBuf>) {
        let path = normalized_export_path(&path.into());
        if self.own_export_paths.contains(&path) {
            return;
        }
        self.own_export_paths.push(path);
        while self.own_export_paths.len() > self.own_export_path_limit {
            self.own_export_paths.remove(0);
        }
    }

    /// Returns true when `path` was recently exported by this drag source.
    #[must_use]
    pub fn is_own_export_path(&self, path: &Path) -> bool {
        let path = normalized_export_path(path);
        self.own_export_paths.contains(&path)
    }

    /// Queue an exported file path using the current armed preview.
    pub fn queue_exported_file(
        &mut self,
        queue: &mut ExternalDragQueue,
        path: PathBuf,
        reused: bool,
    ) -> DragStart {
        let armed = self.armed.take();
        let preview = armed
            .as_ref()
            .and_then(|armed| armed.preview.external_preview());
        let kind = armed
            .as_ref()
            .map_or(DragPayloadKind::Other, |armed| armed.kind);
        let drag_preview = armed
            .as_ref()
            .map_or(DragPreview::None, |armed| armed.preview.clone());
        let label = file_name(&path);

        self.remember_export_path(path.clone());
        self.flash = preview.is_none().then(|| DragFlash {
            until: Instant::now() + Duration::from_millis(1400),
            reused,
            kind,
            label,
            preview: drag_preview,
        });

        queue.drag_files_with_preview(vec![path.clone()], preview);
        DragStart { path, reused }
    }
}

impl<K: Eq> DragExportState<K> {
    /// Reuse an existing render cache if its key still matches and the file exists.
    #[must_use]
    pub fn reusable_cache_path(&self, key: &K) -> Option<&Path> {
        self.render_cache
            .as_ref()
            .filter(|cache| &cache.key == key && cache.path.exists())
            .map(|cache| cache.path.as_path())
    }
}

/// Return a display label for a drag path.
#[must_use]
pub fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map_or_else(|| path.display().to_string(), str::to_string)
}

fn normalized_export_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_own_export_paths_for_self_drop_blocking() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "audio-plugin-dnd-self-drop-{}.mid",
            std::process::id()
        ));
        std::fs::write(&path, b"").expect("test file should be writable");

        let mut state = DragExportState::<String>::default();
        state.remember_export_path(path.clone());
        let canonical = path.canonicalize().expect("test path should canonicalize");
        let _ = std::fs::remove_file(&path);

        assert!(state.is_own_export_path(&canonical));
    }

    #[test]
    fn armed_drag_ready_is_distance_only() {
        let mut state = DragExportState::<String>::default();
        state.arm_midi(Point::new(10.0, 10.0), "MIDI");

        assert!(!state.armed_drag_ready(Point::new(18.0, 10.0)));
        assert!(state.armed_drag_ready(Point::new(20.0, 10.0)));
    }

    #[test]
    fn queue_exported_file_uses_external_preview() {
        let mut state = DragExportState::<String>::default();
        state.arm_audio(
            Point::new(0.0, 0.0),
            "Audio selection",
            DragPreview::Audio(vec![(-0.5, 0.5)]),
        );
        let mut queue = ExternalDragQueue::default();
        let path = PathBuf::from("/tmp/audio-plugin-dnd-test.flac");

        let start = state.queue_exported_file(&mut queue, path.clone(), false);

        assert_eq!(start.path, path);
        assert!(queue
            .pending()
            .is_some_and(|payload| payload.preview.is_some()));
        assert!(state.flash().is_none());
    }
}
