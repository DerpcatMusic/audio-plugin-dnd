use std::collections::VecDeque;
use std::io::{self, Write};

use smithay_client_toolkit::data_device_manager::{
    data_device::DataDevice,
    data_source::{DataSourceData, DragSource},
    DataDeviceManagerState, WritePipe,
};
use wayland_client::{
    protocol::{
        wl_data_device_manager::DndAction, wl_data_source::WlDataSource, wl_surface::WlSurface,
    },
    Dispatch, QueueHandle,
};

use crate::request::{WaylandDragOffer, WaylandExternalDragError, WaylandExternalDragRequest};

#[derive(Clone, Debug, Default)]
pub struct PendingWaylandDrag {
    offers: Vec<WaylandDragOffer>,
    accepted_mime: Option<String>,
    sent_mimes: VecDeque<String>,
    drop_performed: bool,
    finished: bool,
}

impl PendingWaylandDrag {
    pub fn from_request(
        request: WaylandExternalDragRequest,
    ) -> Result<Self, WaylandExternalDragError> {
        if request.offers().is_empty() {
            return Err(WaylandExternalDragError::EmptyOfferSet);
        }

        Ok(Self {
            offers: request.offers().to_vec(),
            accepted_mime: None,
            sent_mimes: VecDeque::new(),
            drop_performed: false,
            finished: false,
        })
    }

    pub fn mime_types(&self) -> impl Iterator<Item = &str> {
        self.offers.iter().map(WaylandDragOffer::mime_type)
    }

    pub fn mime_type_strings(&self) -> Vec<String> {
        self.mime_types().map(str::to_owned).collect()
    }

    pub fn payload_for_mime(&self, mime_type: &str) -> Option<&[u8]> {
        self.offers
            .iter()
            .find(|offer| offer.mime_type() == mime_type)
            .map(WaylandDragOffer::data)
    }

    pub fn note_accepted_mime(&mut self, mime_type: Option<String>) {
        self.accepted_mime = mime_type;
    }

    pub fn note_send(&mut self, mime_type: String) {
        self.sent_mimes.push_back(mime_type);
    }

    pub fn note_drop_performed(&mut self) {
        self.drop_performed = true;
    }

    pub fn note_finished(&mut self) {
        self.finished = true;
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    pub fn write_payload(&mut self, mime_type: &str, mut pipe: WritePipe) -> io::Result<()> {
        if let Some(payload) = self.payload_for_mime(mime_type) {
            pipe.write_all(payload)?;
            pipe.flush()?;
            self.note_send(mime_type.to_owned());
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct ActiveWaylandDrag {
    source: DragSource,
    pending: PendingWaylandDrag,
}

impl ActiveWaylandDrag {
    pub fn create<State>(
        manager: &DataDeviceManagerState,
        queue: &QueueHandle<State>,
        request: WaylandExternalDragRequest,
    ) -> Result<Self, WaylandExternalDragError>
    where
        State: Dispatch<WlDataSource, DataSourceData> + 'static,
    {
        let pending = PendingWaylandDrag::from_request(request)?;
        let source = manager.create_drag_and_drop_source(
            queue,
            pending.mime_type_strings(),
            DndAction::Copy,
        );

        Ok(Self { source, pending })
    }

    pub fn start(
        &self,
        data_device: &DataDevice,
        origin: &WlSurface,
        icon: Option<&WlSurface>,
        serial: u32,
    ) {
        self.source.start_drag(data_device, origin, icon, serial);
    }

    pub fn source(&self) -> &WlDataSource {
        self.source.inner()
    }

    pub fn matches_source(&self, source: &WlDataSource) -> bool {
        self.source.inner() == source
    }

    pub fn note_accepted_mime(&mut self, mime_type: Option<String>) {
        self.pending.note_accepted_mime(mime_type);
    }

    pub fn note_drop_performed(&mut self) {
        self.pending.note_drop_performed();
    }

    pub fn note_finished(&mut self) {
        self.pending.note_finished();
    }

    pub fn is_finished(&self) -> bool {
        self.pending.is_finished()
    }

    pub fn write_payload(&mut self, mime_type: &str, pipe: WritePipe) -> io::Result<()> {
        self.pending.write_payload(mime_type, pipe)
    }

    pub fn destroy(self) {
        self.source().destroy();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_wayland_drag_rejects_empty_offer_set() {
        let request = WaylandExternalDragRequest::new(Vec::new(), Vec::new());

        let err = PendingWaylandDrag::from_request(request)
            .expect_err("empty Wayland offer sets must be rejected");

        assert_eq!(err, WaylandExternalDragError::EmptyOfferSet);
    }
}
