use std::fmt;
use std::time::Duration;

use smithay_client_toolkit::{
    data_device_manager::{
        data_device::{DataDevice, DataDeviceHandler},
        data_offer::{DataOfferHandler, DragOffer},
        data_source::DataSourceHandler,
        DataDeviceManagerState, WritePipe,
    },
    delegate_data_device, delegate_pointer, delegate_seat,
    seat::{
        pointer::{PointerEvent, PointerEventKind, PointerHandler, BTN_LEFT},
        Capability, SeatHandler, SeatState,
    },
};
use wayland_client::{
    globals::{registry_queue_init, GlobalList, GlobalListContents},
    protocol::{
        wl_data_device::WlDataDevice, wl_data_device_manager::DndAction,
        wl_data_source::WlDataSource, wl_pointer::WlPointer, wl_registry, wl_seat,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
};

use crate::request::{WaylandExternalDragError, WaylandExternalDragRequest};

use crate::{foreign::ForeignWaylandParent, state::WaylandDragState};

#[derive(Debug)]
pub enum WaylandRuntimeError {
    RegistryInit(String),
    MissingDataDeviceManager(String),
    Dispatch(String),
}

impl fmt::Display for WaylandRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RegistryInit(err) => write!(formatter, "Wayland registry init failed: {err}"),
            Self::MissingDataDeviceManager(err) => {
                write!(
                    formatter,
                    "Wayland data-device manager is unavailable: {err}"
                )
            }
            Self::Dispatch(err) => write!(formatter, "Wayland event dispatch failed: {err}"),
        }
    }
}

impl std::error::Error for WaylandRuntimeError {}

pub struct WaylandRuntime {
    parent: ForeignWaylandParent,
    globals: GlobalList,
    event_queue: EventQueue<WaylandRuntimeState>,
    state: WaylandRuntimeState,
    data_device_manager: DataDeviceManagerState,
}

impl WaylandRuntime {
    pub fn new(parent: ForeignWaylandParent) -> Result<Self, WaylandRuntimeError> {
        let connection = parent.connection().clone();
        let (globals, event_queue) = registry_queue_init::<WaylandRuntimeState>(&connection)
            .map_err(|err| WaylandRuntimeError::RegistryInit(err.to_string()))?;
        let queue = event_queue.handle();
        let data_device_manager = DataDeviceManagerState::bind(&globals, &queue)
            .map_err(|err| WaylandRuntimeError::MissingDataDeviceManager(err.to_string()))?;
        let seat_state = SeatState::new(&globals, &queue);
        let mut state = WaylandRuntimeState::new(seat_state, parent.surface().clone());

        state.drag_state.note_surface_available(true);

        let mut runtime = Self {
            parent,
            globals,
            event_queue,
            state,
            data_device_manager,
        };
        runtime.sync_data_device_for_active_seat();
        Ok(runtime)
    }

    pub fn connection(&self) -> &Connection {
        self.parent.connection()
    }

    pub fn data_device_manager(&self) -> &DataDeviceManagerState {
        &self.data_device_manager
    }

    pub fn data_device(&self) -> Option<&DataDevice> {
        self.state.data_device.as_ref()
    }

    pub fn queue_handle(&self) -> QueueHandle<WaylandRuntimeState> {
        self.event_queue.handle()
    }

    pub fn drag_state(&self) -> &WaylandDragState {
        &self.state.drag_state
    }

    pub fn drag_state_mut(&mut self) -> &mut WaylandDragState {
        &mut self.state.drag_state
    }

    pub fn can_start_external_drag(&self) -> bool {
        self.state.drag_state.can_start_drag() && self.state.data_device.is_some()
    }

    pub fn start_external_drag(
        &mut self,
        request: WaylandExternalDragRequest,
    ) -> Result<(), WaylandExternalDragError> {
        self.sync_data_device_for_active_seat();
        let queue = self.event_queue.handle();
        let Some(data_device) = self.state.data_device.as_ref() else {
            return Err(WaylandExternalDragError::MissingSeat);
        };
        self.state.drag_state.start_drag(
            &self.data_device_manager,
            &queue,
            data_device,
            self.parent.surface(),
            None,
            request,
        )
    }

    pub fn dispatch_pending(&mut self) -> Result<(), WaylandRuntimeError> {
        self.event_queue
            .dispatch_pending(&mut self.state)
            .map_err(|err| WaylandRuntimeError::Dispatch(err.to_string()))?;
        self.sync_data_device_for_active_seat();
        Ok(())
    }

    pub fn blocking_dispatch(&mut self, timeout: Duration) -> Result<(), WaylandRuntimeError> {
        self.event_queue
            .blocking_dispatch(&mut self.state)
            .map_err(|err| {
                let _ = timeout;
                WaylandRuntimeError::Dispatch(err.to_string())
            })?;
        self.sync_data_device_for_active_seat();
        Ok(())
    }

    pub fn globals(&self) -> &GlobalList {
        &self.globals
    }

    fn sync_data_device_for_active_seat(&mut self) {
        if self.state.data_device.is_some() {
            return;
        }

        let seat = self
            .state
            .active_seat
            .clone()
            .or_else(|| self.state.seat_state.seats().next());
        let Some(seat) = seat else {
            self.state.drag_state.note_seat_available(false);
            self.state.drag_state.note_data_device_available(false);
            return;
        };

        let queue = self.event_queue.handle();
        let data_device = self.data_device_manager.get_data_device(&queue, &seat);
        self.state.active_seat = Some(seat);
        self.state.data_device = Some(data_device);
        self.state.drag_state.note_seat_available(true);
        self.state.drag_state.note_data_device_available(true);
    }
}

pub struct WaylandRuntimeState {
    drag_state: WaylandDragState,
    seat_state: SeatState,
    origin_surface: WlSurface,
    active_seat: Option<wl_seat::WlSeat>,
    data_device: Option<DataDevice>,
    pointer: Option<WlPointer>,
}

impl WaylandRuntimeState {
    fn new(seat_state: SeatState, origin_surface: WlSurface) -> Self {
        Self {
            drag_state: WaylandDragState::default(),
            seat_state,
            origin_surface,
            active_seat: None,
            data_device: None,
            pointer: None,
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for WaylandRuntimeState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: <wl_registry::WlRegistry as Proxy>::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
    }
}

impl SeatHandler for WaylandRuntimeState {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        if self.active_seat.is_none() {
            self.active_seat = Some(seat);
            self.drag_state.note_seat_available(true);
        }
    }

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            if self.pointer.is_none() {
                if let Ok(pointer) = self.seat_state.get_pointer(qh, &_seat) {
                    self.pointer = Some(pointer);
                }
            }
            self.drag_state.note_seat_available(true);
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            self.pointer = None;
            self.drag_state.clear_pointer_button_serial();
        }
    }

    fn remove_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        if self.active_seat.as_ref() == Some(&seat) {
            self.active_seat = None;
            self.data_device = None;
            self.pointer = None;
            self.drag_state.note_seat_available(false);
            self.drag_state.note_data_device_available(false);
        }
    }
}

impl PointerHandler for WaylandRuntimeState {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _pointer: &WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            if event.surface != self.origin_surface {
                continue;
            }

            match event.kind {
                PointerEventKind::Press { button, serial, .. } if button == BTN_LEFT => {
                    self.drag_state.note_pointer_button_serial(serial);
                }
                PointerEventKind::Release { button, .. } if button == BTN_LEFT => {
                    self.drag_state.clear_pointer_button_serial();
                }
                _ => {}
            }
        }
    }
}

impl DataDeviceHandler for WaylandRuntimeState {
    fn enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _data_device: &WlDataDevice,
        _x: f64,
        _y: f64,
        _surface: &wayland_client::protocol::wl_surface::WlSurface,
    ) {
    }

    fn leave(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _data_device: &WlDataDevice) {}

    fn motion(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _data_device: &WlDataDevice,
        _x: f64,
        _y: f64,
    ) {
    }

    fn selection(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _data_device: &WlDataDevice,
    ) {
    }

    fn drop_performed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _data_device: &WlDataDevice,
    ) {
    }
}

impl DataOfferHandler for WaylandRuntimeState {
    fn source_actions(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        offer: &mut DragOffer,
        actions: DndAction,
    ) {
        offer.set_actions(actions, DndAction::Copy);
    }

    fn selected_action(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _offer: &mut DragOffer,
        _actions: DndAction,
    ) {
    }
}

impl DataSourceHandler for WaylandRuntimeState {
    fn accept_mime(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        source: &WlDataSource,
        mime: Option<String>,
    ) {
        self.drag_state.handle_accept_mime(source, mime);
    }

    fn send_request(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        source: &WlDataSource,
        mime: String,
        pipe: WritePipe,
    ) {
        self.drag_state.handle_send_request(source, &mime, pipe);
    }

    fn cancelled(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, source: &WlDataSource) {
        self.drag_state.handle_cancelled(source);
    }

    fn dnd_dropped(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, source: &WlDataSource) {
        self.drag_state.handle_drop_performed(source);
    }

    fn dnd_finished(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, source: &WlDataSource) {
        self.drag_state.handle_finished(source);
    }

    fn action(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _source: &WlDataSource,
        _action: DndAction,
    ) {
    }
}

delegate_seat!(WaylandRuntimeState);
delegate_pointer!(WaylandRuntimeState);
delegate_data_device!(WaylandRuntimeState);
