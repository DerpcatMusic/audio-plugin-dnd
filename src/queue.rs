//! Toolkit-neutral queue for drag requests emitted by plugin UI code.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// One normalized MIDI note bar for the drag chip (0..1 time and pitch).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MidiChipNote {
    /// Note start in selection time (0 = left, 1 = right).
    pub start: f32,
    /// Note end in selection time.
    pub end: f32,
    /// Pitch (0 = low / bottom, 1 = high / top).
    pub pitch: f32,
}

/// Optional drag preview metadata for platforms/toolkits that can show it.
#[derive(Clone, Debug, PartialEq)]
pub enum ExternalDragPreview {
    /// Min/max waveform buckets in normalized audio amplitude.
    Waveform { buckets: Vec<(f32, f32)> },
    /// Compact spectral preview data (column-major energy: `column * rows + row`).
    Spectral {
        columns: usize,
        rows: usize,
        energy: Vec<f32>,
        low_hz: f32,
        high_hz: f32,
    },
    /// Piano-roll note bars for the dragged MIDI selection.
    Midi { notes: Vec<MidiChipNote> },
}

/// File drag payload passed from plugin UI code to a platform backend.
#[derive(Clone, Debug, PartialEq)]
pub struct ExternalDragPayload {
    pub id: u64,
    pub paths: Vec<PathBuf>,
    pub preview: Option<ExternalDragPreview>,
}

/// Generate a process-local monotonic drag id for diagnostics.
#[must_use]
pub fn next_external_drag_id() -> u64 {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// Single-slot queue matching plugin GUI frame/update loops.
#[derive(Clone, Debug, Default)]
pub struct ExternalDragQueue {
    pending: Option<ExternalDragPayload>,
}

impl ExternalDragQueue {
    /// Queue a file drag.
    pub fn drag_files(&mut self, paths: Vec<PathBuf>) {
        self.drag_files_with_preview(paths, None);
    }

    /// Queue a file drag with optional preview metadata.
    pub fn drag_files_with_preview(
        &mut self,
        paths: Vec<PathBuf>,
        preview: Option<ExternalDragPreview>,
    ) {
        self.pending = Some(ExternalDragPayload {
            id: next_external_drag_id(),
            paths,
            preview,
        });
    }

    /// Take the currently queued drag payload.
    pub fn take(&mut self) -> Option<ExternalDragPayload> {
        self.pending.take()
    }

    /// Borrow the currently queued drag payload.
    #[must_use]
    pub fn pending(&self) -> Option<&ExternalDragPayload> {
        self.pending.as_ref()
    }
}
