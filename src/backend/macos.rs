use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSDragOperation, NSDraggingContext, NSDraggingItem, NSDraggingSession,
    NSDraggingSource, NSEvent, NSView,
};
use objc2_foundation::{
    NSArray, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSURL,
};
use raw_window_handle::RawWindowHandle;

use super::{emit_backend_event, DragWindow, ExternalDragError};
use crate::ExternalDragPayload;

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "AudioPluginDndExternalDragSource"]
    struct AudioPluginDndExternalDragSource;

    unsafe impl NSObjectProtocol for AudioPluginDndExternalDragSource {}

    #[allow(non_snake_case)]
    unsafe impl NSDraggingSource for AudioPluginDndExternalDragSource {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn draggingSession_sourceOperationMaskForDraggingContext(
            &self,
            _session: &NSDraggingSession,
            _context: NSDraggingContext,
        ) -> NSDragOperation {
            NSDragOperation::Copy
        }
    }
);

impl AudioPluginDndExternalDragSource {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        unsafe { msg_send![mtm.alloc::<Self>(), init] }
    }
}

pub(super) fn start_external_file_drag(
    window: DragWindow,
    payload: ExternalDragPayload,
) -> Result<(), ExternalDragError> {
    let ExternalDragPayload { id, paths, preview } = payload;
    let _ = preview;

    if paths.is_empty() {
        return Err(ExternalDragError::EmptyPayload);
    }
    let file_summary = validate_paths(&paths)?;

    let ns_view = match window.window() {
        RawWindowHandle::AppKit(handle) if !handle.ns_view.is_null() => handle.ns_view.cast(),
        RawWindowHandle::AppKit(_) => {
            return Err(ExternalDragError::MissingWindowHandle(
                "window does not have a valid AppKit NSView",
            ));
        }
        other => {
            return Err(ExternalDragError::UnsupportedBackend {
                backend: window.backend_kind(),
                window: format!("{other:?}"),
            });
        }
    };

    let mtm = MainThreadMarker::new().ok_or_else(|| {
        "macOS external file drag must start on the AppKit main thread".to_string()
    })?;
    let app = NSApplication::sharedApplication(mtm);
    let event = app.currentEvent().ok_or_else(|| {
        "macOS external file drag needs the AppKit mouse event that started the drag".to_string()
    })?;

    let view = unsafe { &*ns_view };
    emit_backend_event(format!(
        "[dnd#{id}] macOS AppKit drag preparing {} file(s): {}",
        paths.len(),
        file_summary.join(", ")
    ));
    start_drag_from_view(view, &event, &paths);
    Ok(())
}

fn start_drag_from_view(view: &NSView, event: &NSEvent, paths: &[PathBuf]) {
    let location = event.locationInWindow();
    let items = dragging_items(paths, location);
    let item_refs = items.iter().map(|item| &**item).collect::<Vec<_>>();
    let item_array = NSArray::from_slice(&item_refs);
    let mtm = MainThreadMarker::new().expect("AppKit drag source should still be on main thread");
    let source = AudioPluginDndExternalDragSource::new(mtm);
    let source_ref: &ProtocolObject<dyn NSDraggingSource> = ProtocolObject::from_ref(&*source);

    let _session = view.beginDraggingSessionWithItems_event_source(&item_array, event, source_ref);
    let _ = Retained::into_raw(source);
}

fn dragging_items(paths: &[PathBuf], location: NSPoint) -> Vec<Retained<NSDraggingItem>> {
    paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let absolute = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
            let path_string = NSString::from_str(&absolute.to_string_lossy());
            let file_url = NSURL::fileURLWithPath(&path_string);
            let writer: &ProtocolObject<dyn objc2_app_kit::NSPasteboardWriting> =
                ProtocolObject::from_ref(&*file_url);
            let dragging_item =
                NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), writer);
            let offset = index as f64 * 4.0;
            unsafe {
                dragging_item.setDraggingFrame_contents(
                    NSRect::new(
                        NSPoint::new(location.x - 58.0 + offset, location.y - 24.0 - offset),
                        NSSize::new(116.0, 48.0),
                    ),
                    None,
                );
            }
            dragging_item
        })
        .collect()
}

fn validate_paths(paths: &[PathBuf]) -> Result<Vec<String>, String> {
    let mut summary = Vec::with_capacity(paths.len());
    for path in paths {
        let metadata = std::fs::metadata(path)
            .map_err(|err| format!("drag file is not readable: {}: {err}", path.display()))?;
        if !metadata.is_file() {
            return Err(format!("drag path is not a file: {}", path.display()));
        }
        if metadata.len() == 0 {
            return Err(format!("drag file is empty: {}", path.display()));
        }
        summary.push(format!("{} ({} bytes)", path.display(), metadata.len()));
    }
    Ok(summary)
}
