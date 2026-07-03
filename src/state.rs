use smithay_client_toolkit::data_device_manager::{
    data_device::DataDevice, data_source::DataSourceData, DataDeviceManagerState, WritePipe,
};
use wayland_client::{
    protocol::{wl_data_source::WlDataSource, wl_surface::WlSurface},
    Dispatch, QueueHandle,
};

use crate::request::{WaylandExternalDragError, WaylandExternalDragRequest};

use crate::data_device::ActiveWaylandDrag;

#[derive(Debug, Default)]
pub struct WaylandDragState {
    active: Option<ActiveWaylandDrag>,
    has_surface: bool,
    has_seat: bool,
    has_data_device: bool,
    pointer_button_serial: Option<u32>,
}

impl WaylandDragState {
    pub fn note_surface_available(&mut self, available: bool) {
        self.has_surface = available;
        if !available {
            self.pointer_button_serial = None;
            self.active = None;
        }
    }

    pub fn note_seat_available(&mut self, available: bool) {
        self.has_seat = available;
        if !available {
            self.pointer_button_serial = None;
            self.active = None;
        }
    }

    pub fn note_data_device_available(&mut self, available: bool) {
        self.has_data_device = available;
        if !available {
            self.active = None;
        }
    }

    pub fn note_pointer_button_serial(&mut self, serial: u32) {
        self.pointer_button_serial = Some(serial);
    }

    pub fn clear_pointer_button_serial(&mut self) {
        self.pointer_button_serial = None;
    }

    pub fn can_start_drag(&self) -> bool {
        self.has_surface
            && self.has_seat
            && self.has_data_device
            && self.pointer_button_serial.is_some()
            && self.active.is_none()
    }

    pub fn start_drag<State>(
        &mut self,
        manager: &DataDeviceManagerState,
        queue: &QueueHandle<State>,
        data_device: &DataDevice,
        origin: &WlSurface,
        icon: Option<&WlSurface>,
        request: WaylandExternalDragRequest,
    ) -> Result<(), WaylandExternalDragError>
    where
        State: Dispatch<WlDataSource, DataSourceData> + 'static,
    {
        if !self.has_surface {
            return Err(WaylandExternalDragError::MissingSurface);
        }
        if !self.has_seat || !self.has_data_device {
            return Err(WaylandExternalDragError::MissingSeat);
        }
        let Some(serial) = self.pointer_button_serial else {
            return Err(WaylandExternalDragError::MissingPointerButtonSerial);
        };

        let active = ActiveWaylandDrag::create(manager, queue, request)?;
        active.start(data_device, origin, icon, serial);
        self.active = Some(active);
        Ok(())
    }

    pub fn handle_accept_mime(&mut self, source: &WlDataSource, mime_type: Option<String>) {
        if let Some(active) = self.active_for_source_mut(source) {
            active.note_accepted_mime(mime_type);
        }
    }

    pub fn handle_send_request(&mut self, source: &WlDataSource, mime_type: &str, pipe: WritePipe) {
        if let Some(active) = self.active_for_source_mut(source) {
            let _ = active.write_payload(mime_type, pipe);
        }
    }

    pub fn handle_drop_performed(&mut self, source: &WlDataSource) {
        if let Some(active) = self.active_for_source_mut(source) {
            active.note_drop_performed();
        }
    }

    pub fn handle_finished(&mut self, source: &WlDataSource) {
        let Some(active) = self.active.take() else {
            return;
        };
        if active.matches_source(source) {
            let mut active = active;
            active.note_finished();
            active.destroy();
        } else {
            self.active = Some(active);
        }
    }

    pub fn handle_cancelled(&mut self, source: &WlDataSource) {
        let Some(active) = self.active.take() else {
            return;
        };
        if active.matches_source(source) {
            active.destroy();
        } else {
            self.active = Some(active);
        }
    }

    fn active_for_source_mut(&mut self, source: &WlDataSource) -> Option<&mut ActiveWaylandDrag> {
        self.active
            .as_mut()
            .filter(|active| active.matches_source(source))
    }
}
