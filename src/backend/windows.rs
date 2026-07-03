use std::mem::{size_of, ManuallyDrop};
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::ptr;

use raw_window_handle::RawWindowHandle;
use windows::core::implement;
use windows::Win32::Foundation::{
    DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, DV_E_FORMATETC, E_NOTIMPL,
    HWND, OLE_E_ADVISENOTSUPPORTED, POINT, RPC_E_CHANGED_MODE,
};
use windows::Win32::System::Com::{
    IAdviseSink, IDataObject, IDataObject_Impl, IEnumFORMATETC, IEnumSTATDATA, DATADIR_GET,
    DVASPECT_CONTENT, FORMATETC, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GHND};
use windows::Win32::System::Ole::{
    DoDragDrop, IDropSource, IDropSource_Impl, OleInitialize, OleUninitialize, CF_HDROP,
    DROPEFFECT, DROPEFFECT_COPY,
};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Shell::{
    SHCreateStdEnumFmtEtc, CFSTR_FILENAMEW, CFSTR_PREFERREDDROPEFFECT, DROPFILES,
};
use windows_core::{IUnknown, Ref, Result, BOOL, HRESULT, PCWSTR};

use super::{emit_backend_event, DragWindow, ExternalDragError};
use crate::ExternalDragPayload;

pub(super) fn start_external_file_drag(
    window: DragWindow,
    payload: ExternalDragPayload,
) -> std::result::Result<(), ExternalDragError> {
    let ExternalDragPayload { id, paths, preview } = payload;
    let _ = preview;

    if paths.is_empty() {
        return Err(ExternalDragError::EmptyPayload);
    }
    let file_summary = validate_paths(&paths)?;

    let hwnd = match window.window() {
        RawWindowHandle::Win32(handle) if !handle.hwnd.is_null() => HWND(handle.hwnd),
        RawWindowHandle::Win32(_) => {
            return Err(ExternalDragError::MissingWindowHandle(
                "window does not have a valid Win32 HWND",
            ));
        }
        other => {
            return Err(ExternalDragError::UnsupportedBackend {
                backend: window.backend_kind(),
                window: format!("{other:?}"),
            });
        }
    };

    emit_backend_event(format!(
        "[dnd#{id}] Windows OLE drag preparing {} file(s): {}",
        paths.len(),
        file_summary.join(", ")
    ));
    let _ole = OleDragApartment::initialize(id)?;
    let data_object: IDataObject = FileDataObject::new(paths)?.into();
    let drop_source: IDropSource = FileDropSource.into();
    let mut effect = DROPEFFECT(0);

    unsafe {
        let result = DoDragDrop(
            &data_object,
            &drop_source,
            DROPEFFECT_COPY,
            &mut effect as *mut DROPEFFECT,
        );
        result.ok().map_err(|err| {
            ExternalDragError::StartFailed(format!(
                "Windows OLE DoDragDrop failed for {hwnd:?}: {err}"
            ))
        })?;
    }
    emit_backend_event(format!(
        "[dnd#{id}] Windows OLE drag completed with effect=0x{:x}",
        effect.0
    ));

    Ok(())
}

struct OleDragApartment {
    drag_id: u64,
}

impl OleDragApartment {
    fn initialize(drag_id: u64) -> std::result::Result<Self, String> {
        match unsafe { OleInitialize(None) } {
            Ok(()) => {
                emit_backend_event(format!("[dnd#{drag_id}] Windows OLE initialized"));
                Ok(Self { drag_id })
            }
            Err(err) if err.code() == RPC_E_CHANGED_MODE => {
                Err("Windows OLE drag unavailable: plugin UI thread is already initialized as a multithreaded COM apartment".to_string())
            }
            Err(err) => Err(format!("Windows OLE initialize failed: {err}")),
        }
    }
}

impl Drop for OleDragApartment {
    fn drop(&mut self) {
        unsafe { OleUninitialize() };
        emit_backend_event(format!("[dnd#{}] Windows OLE uninitialized", self.drag_id));
    }
}

fn validate_paths(paths: &[PathBuf]) -> std::result::Result<Vec<String>, String> {
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

#[implement(IDataObject)]
struct FileDataObject {
    paths: Vec<PathBuf>,
    formats: ShellDragFormats,
}

impl FileDataObject {
    fn new(paths: Vec<PathBuf>) -> std::result::Result<Self, String> {
        Ok(Self {
            paths,
            formats: ShellDragFormats::new()?,
        })
    }

    fn format(clipboard_format: u16) -> FORMATETC {
        FORMATETC {
            cfFormat: clipboard_format,
            ptd: ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        }
    }

    unsafe fn requested_format(&self, pformatetc: *const FORMATETC) -> Option<ShellDragFormat> {
        let Some(format) = (unsafe { pformatetc.as_ref() }) else {
            return None;
        };

        if format.dwAspect != DVASPECT_CONTENT.0
            || format.lindex != -1
            || (format.tymed & TYMED_HGLOBAL.0 as u32) == 0
        {
            return None;
        }

        self.formats
            .formats()
            .into_iter()
            .find(|candidate| candidate.clipboard_format() == format.cfFormat)
    }

    fn hdrop_medium(&self) -> Result<STGMEDIUM> {
        let hglobal = build_hdrop(&self.paths)?;
        Ok(STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 { hGlobal: hglobal },
            pUnkForRelease: ManuallyDrop::new(None::<IUnknown>),
        })
    }

    fn preferred_drop_effect_medium(&self) -> Result<STGMEDIUM> {
        let hglobal = build_u32_hglobal(DROPEFFECT_COPY.0)?;
        Ok(STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 { hGlobal: hglobal },
            pUnkForRelease: ManuallyDrop::new(None::<IUnknown>),
        })
    }

    fn filenamew_medium(&self) -> Result<STGMEDIUM> {
        let Some(path) = self.paths.first() else {
            return Err(DV_E_FORMATETC.into());
        };
        let hglobal =
            build_wide_string_hglobal(&path.as_os_str().encode_wide().collect::<Vec<_>>())?;
        Ok(STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 { hGlobal: hglobal },
            pUnkForRelease: ManuallyDrop::new(None::<IUnknown>),
        })
    }
}

#[allow(non_snake_case)]
impl IDataObject_Impl for FileDataObject_Impl {
    fn GetData(&self, pformatetcin: *const FORMATETC) -> Result<STGMEDIUM> {
        match unsafe { self.requested_format(pformatetcin) } {
            Some(ShellDragFormat::Hdrop) => self.hdrop_medium(),
            Some(ShellDragFormat::PreferredDropEffect(_)) => self.preferred_drop_effect_medium(),
            Some(ShellDragFormat::FileNameW(_)) => self.filenamew_medium(),
            None => Err(DV_E_FORMATETC.into()),
        }
    }

    fn GetDataHere(&self, _pformatetc: *const FORMATETC, _pmedium: *mut STGMEDIUM) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn QueryGetData(&self, pformatetc: *const FORMATETC) -> HRESULT {
        if unsafe { self.requested_format(pformatetc) }.is_some() {
            HRESULT(0)
        } else {
            DV_E_FORMATETC
        }
    }

    fn GetCanonicalFormatEtc(
        &self,
        _pformatectin: *const FORMATETC,
        _pformatetcout: *mut FORMATETC,
    ) -> HRESULT {
        E_NOTIMPL
    }

    fn SetData(
        &self,
        _pformatetc: *const FORMATETC,
        _pmedium: *const STGMEDIUM,
        _frelease: BOOL,
    ) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn EnumFormatEtc(&self, dwdirection: u32) -> Result<IEnumFORMATETC> {
        if dwdirection == DATADIR_GET.0 as u32 {
            let formats = self
                .formats
                .formats()
                .into_iter()
                .map(|format| FileDataObject::format(format.clipboard_format()))
                .collect::<Vec<_>>();
            unsafe { SHCreateStdEnumFmtEtc(&formats) }
        } else {
            Err(E_NOTIMPL.into())
        }
    }

    fn DAdvise(
        &self,
        _pformatetc: *const FORMATETC,
        _advf: u32,
        _padvsink: Ref<'_, IAdviseSink>,
    ) -> Result<u32> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn DUnadvise(&self, _dwconnection: u32) -> Result<()> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn EnumDAdvise(&self) -> Result<IEnumSTATDATA> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
}

#[derive(Clone, Copy)]
struct ShellDragFormats {
    preferred_drop_effect: u16,
    filenamew: u16,
}

#[derive(Clone, Copy)]
enum ShellDragFormat {
    Hdrop,
    PreferredDropEffect(u16),
    FileNameW(u16),
}

impl ShellDragFormats {
    fn new() -> std::result::Result<Self, String> {
        let preferred_drop_effect = registered_clipboard_format(CFSTR_PREFERREDDROPEFFECT)
            .ok_or_else(|| {
                "Windows could not register Preferred DropEffect clipboard format".to_string()
            })?;
        let filenamew = registered_clipboard_format(CFSTR_FILENAMEW)
            .ok_or_else(|| "Windows could not register FileNameW clipboard format".to_string())?;
        Ok(Self {
            preferred_drop_effect,
            filenamew,
        })
    }

    fn formats(self) -> Vec<ShellDragFormat> {
        vec![
            ShellDragFormat::Hdrop,
            ShellDragFormat::PreferredDropEffect(self.preferred_drop_effect),
            ShellDragFormat::FileNameW(self.filenamew),
        ]
    }
}

impl ShellDragFormat {
    fn clipboard_format(self) -> u16 {
        match self {
            ShellDragFormat::Hdrop => CF_HDROP.0,
            ShellDragFormat::PreferredDropEffect(format) | ShellDragFormat::FileNameW(format) => {
                format
            }
        }
    }
}

fn registered_clipboard_format(name: PCWSTR) -> Option<u16> {
    let value = unsafe { RegisterClipboardFormatW(name) };
    u16::try_from(value).ok().filter(|value| *value != 0)
}

#[implement(IDropSource)]
struct FileDropSource;

#[allow(non_snake_case)]
impl IDropSource_Impl for FileDropSource_Impl {
    fn QueryContinueDrag(&self, fescapepressed: BOOL, grfkeystate: MODIFIERKEYS_FLAGS) -> HRESULT {
        if fescapepressed.as_bool() {
            DRAGDROP_S_CANCEL
        } else if (grfkeystate.0 & MK_LBUTTON.0) == 0 {
            DRAGDROP_S_DROP
        } else {
            HRESULT(0)
        }
    }

    fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

fn build_hdrop(paths: &[PathBuf]) -> Result<windows::Win32::Foundation::HGLOBAL> {
    let mut encoded_paths = Vec::with_capacity(paths.len());
    let mut wide_len = 1usize;

    for path in paths {
        let mut encoded = path.as_os_str().encode_wide().collect::<Vec<_>>();
        encoded.push(0);
        wide_len += encoded.len();
        encoded_paths.push(encoded);
    }

    let header_size = size_of::<DROPFILES>();
    let bytes = header_size + wide_len * size_of::<u16>();
    let hglobal = unsafe { GlobalAlloc(GHND, bytes)? };
    let data = unsafe { GlobalLock(hglobal) } as *mut u8;

    if data.is_null() {
        return Err(windows_core::Error::from_thread());
    }

    unsafe {
        (data as *mut DROPFILES).write(DROPFILES {
            pFiles: header_size as u32,
            pt: POINT { x: 0, y: 0 },
            fNC: BOOL(0),
            fWide: BOOL(1),
        });

        let mut cursor = data.add(header_size) as *mut u16;
        for encoded in &encoded_paths {
            ptr::copy_nonoverlapping(encoded.as_ptr(), cursor, encoded.len());
            cursor = cursor.add(encoded.len());
        }
        cursor.write(0);

        let _ = GlobalUnlock(hglobal);
    }

    Ok(hglobal)
}

fn build_u32_hglobal(value: u32) -> Result<windows::Win32::Foundation::HGLOBAL> {
    let hglobal = unsafe { GlobalAlloc(GHND, size_of::<u32>())? };
    let data = unsafe { GlobalLock(hglobal) } as *mut u32;
    if data.is_null() {
        return Err(windows_core::Error::from_thread());
    }

    unsafe {
        data.write(value);
        let _ = GlobalUnlock(hglobal);
    }

    Ok(hglobal)
}

fn build_wide_string_hglobal(wide: &[u16]) -> Result<windows::Win32::Foundation::HGLOBAL> {
    let bytes = (wide.len() + 1) * size_of::<u16>();
    let hglobal = unsafe { GlobalAlloc(GHND, bytes)? };
    let data = unsafe { GlobalLock(hglobal) } as *mut u16;
    if data.is_null() {
        return Err(windows_core::Error::from_thread());
    }

    unsafe {
        ptr::copy_nonoverlapping(wide.as_ptr(), data, wide.len());
        data.add(wide.len()).write(0);
        let _ = GlobalUnlock(hglobal);
    }

    Ok(hglobal)
}
