//! Platform-neutral drag-and-drop protocol reporting types.

#![allow(dead_code)]

use std::fmt::{self, Write as _};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DragCompletion {
    Confirmed,
    Inferred,
    Failed(DragFailureKind),
}

impl DragCompletion {
    pub(super) const fn is_success(self) -> bool {
        matches!(self, Self::Confirmed | Self::Inferred)
    }

    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Inferred => "inferred",
            Self::Failed(_) => "failed",
        }
    }

    pub(super) const fn summary(self) -> &'static str {
        match self {
            Self::Confirmed => "Drop completed",
            Self::Inferred => "Drop likely completed",
            Self::Failed(kind) => kind.summary(),
        }
    }
}

impl fmt::Display for DragCompletion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DragFailureKind {
    NoTarget,
    BridgeRejected,
    TargetNoData,
    BackendUnavailable,
    Cancelled,
    Other,
}

impl DragFailureKind {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::NoTarget => "no_target",
            Self::BridgeRejected => "bridge_rejected",
            Self::TargetNoData => "target_no_data",
            Self::BackendUnavailable => "backend_unavailable",
            Self::Cancelled => "cancelled",
            Self::Other => "other",
        }
    }

    pub(super) const fn summary(self) -> &'static str {
        match self {
            Self::NoTarget => "No drop target was found",
            Self::BridgeRejected => "The drop bridge rejected the drag",
            Self::TargetNoData => "The target did not request the drag data",
            Self::BackendUnavailable => "Drag-and-drop is unavailable on this backend",
            Self::Cancelled => "Drag cancelled",
            Self::Other => "Drop failed",
        }
    }
}

impl fmt::Display for DragFailureKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DragTargetKind {
    RealXWindow,
    AnonymousBridge,
    OriginWindow,
    NativeWayland,
    Unknown,
}

impl DragTargetKind {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::RealXWindow => "real_x_window",
            Self::AnonymousBridge => "anonymous_bridge",
            Self::OriginWindow => "origin_window",
            Self::NativeWayland => "native_wayland",
            Self::Unknown => "unknown",
        }
    }

    pub(super) const fn summary(self) -> &'static str {
        match self {
            Self::RealXWindow => "X11 window",
            Self::AnonymousBridge => "anonymous bridge",
            Self::OriginWindow => "origin window",
            Self::NativeWayland => "native Wayland target",
            Self::Unknown => "unknown target",
        }
    }
}

impl fmt::Display for DragTargetKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DragPhase {
    Started,
    EnteredTarget,
    TargetInspectedData,
    TargetAccepted,
    Released,
    DropSent,
    Finished,
    Completed,
}

impl DragPhase {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::EnteredTarget => "entered_target",
            Self::TargetInspectedData => "target_inspected_data",
            Self::TargetAccepted => "target_accepted",
            Self::Released => "released",
            Self::DropSent => "drop_sent",
            Self::Finished => "finished",
            Self::Completed => "completed",
        }
    }

    pub(super) const fn summary(self) -> &'static str {
        match self {
            Self::Started => "Drag started",
            Self::EnteredTarget => "Entered drop target",
            Self::TargetInspectedData => "Target inspected drag data",
            Self::TargetAccepted => "Target accepted drag",
            Self::Released => "Drag released",
            Self::DropSent => "Drop sent",
            Self::Finished => "Drag finished",
            Self::Completed => "Drag completed",
        }
    }
}

impl fmt::Display for DragPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct DragSessionStats {
    pub(super) selection_requests: u32,
    pub(super) pre_drop_data_requests: u32,
    pub(super) post_drop_data_requests: u32,
    pub(super) drop_target_data_requests: u32,
}

impl DragSessionStats {
    pub(super) const fn total_data_requests(self) -> u32 {
        self.pre_drop_data_requests + self.post_drop_data_requests + self.drop_target_data_requests
    }

    pub(super) const fn has_target_data_request(self) -> bool {
        self.drop_target_data_requests > 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DragSessionReport {
    pub(super) completion: DragCompletion,
    pub(super) stats: DragSessionStats,
    pub(super) detail: String,
}

impl DragSessionReport {
    pub(super) fn completed_confirmed(stats: DragSessionStats, detail: impl Into<String>) -> Self {
        Self {
            completion: DragCompletion::Confirmed,
            stats,
            detail: detail.into(),
        }
    }

    pub(super) fn completed_inferred(stats: DragSessionStats, detail: impl Into<String>) -> Self {
        Self {
            completion: DragCompletion::Inferred,
            stats,
            detail: detail.into(),
        }
    }

    pub(super) fn failed(
        kind: DragFailureKind,
        stats: DragSessionStats,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            completion: DragCompletion::Failed(kind),
            stats,
            detail: detail.into(),
        }
    }

    pub(super) fn is_success(&self) -> bool {
        self.completion.is_success()
    }

    pub(super) fn summary(&self) -> String {
        let mut summary = String::from(self.completion.summary());
        if !self.detail.is_empty() {
            let _ = write!(summary, ": {}", self.detail);
        }
        summary
    }

    pub(super) fn stats_summary(&self) -> String {
        format!(
            "selection_requests={}, pre_drop_data_requests={}, post_drop_data_requests={}, drop_target_data_requests={}",
            self.stats.selection_requests,
            self.stats.pre_drop_data_requests,
            self.stats.post_drop_data_requests,
            self.stats.drop_target_data_requests
        )
    }
}
