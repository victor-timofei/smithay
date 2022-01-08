//! Utilities for manipulating the data devices
//!
//! The data device is wayland's abstraction to represent both selection (copy/paste) and
//! drag'n'drop actions. This module provides logic to handle this part of the protocol.
//! Selection and drag'n'drop are per-seat notions.
//!
//! This module provides 2 main freestanding functions:
//!
//! - [`init_data_device`]: this function must be called
//!   during the compositor startup to initialize the data device logic
//! - [`set_data_device_focus`]: this function sets
//!   the data device focus for a given seat; you'd typically call it whenever the keyboard focus
//!   changes, to follow it (for example in the focus hook of your keyboards)
//!
//! Using these two functions is enough for your clients to be able to interact with each other using
//! the data devices.
//!
//! The module also provides additional mechanisms allowing your compositor to see and interact with
//! the contents of the data device:
//!
//! - You can provide a callback closure to [`init_data_device`]
//!   to peek into the the actions of your clients
//! - the freestanding function [`set_data_device_selection`]
//!   allows you to set the contents of the selection for your clients
//! - the freestanding function [`start_dnd`] allows you to initiate a drag'n'drop event from the compositor
//!   itself and receive interactions of clients with it via an other dedicated callback.
//!
//! The module defines the role `"dnd_icon"` that is assigned to surfaces used as drag'n'drop icons.
//!
//! ## Initialization
//!
//! ```
//! # extern crate wayland_server;
//! use smithay::wayland::data_device::{init_data_device, default_action_chooser};
//! # use smithay::wayland::compositor::compositor_init;
//!
//! # let mut display = wayland_server::Display::new();
//! // init the data device:
//! init_data_device(
//!     &mut display,            // the display
//!     |dnd_event| { /* a callback to react to client DnD/selection actions */ },
//!     default_action_chooser,  // a closure to choose the DnD action depending on clients
//!                              // negociation
//!     None                     // insert a logger here
//! );
//! ```

use std::{cell::RefCell, ops::Deref as _, os::unix::io::RawFd, rc::Rc};

use wayland_server::{
    protocol::{
        wl_data_device,
        wl_data_device_manager::{self, DndAction},
        wl_data_offer, wl_data_source, wl_surface,
    },
    Client, Display, Filter, Global, Main,
};

use slog::{debug, error, o};

use crate::wayland::{
    compositor,
    seat::{PointerGrabStartData, Seat},
    Serial,
};

mod data_source;
mod dnd_grab;
mod server_dnd_grab;

pub use self::data_source::{with_source_metadata, SourceMetadata};
pub use self::server_dnd_grab::ServerDndEvent;

static DND_ICON_ROLE: &str = "dnd_icon";

/// Events that are generated by interactions of the clients with the data device
#[derive(Debug)]
pub enum DataDeviceEvent {
    /// A client has set the selection
    NewSelection(Option<wl_data_source::WlDataSource>),
    /// A client started a drag'n'drop as response to a user pointer action
    DnDStarted {
        /// The data source provided by the client
        ///
        /// If it is `None`, this means the DnD is restricted to surfaces of the
        /// same client and the client will manage data transfer by itself.
        source: Option<wl_data_source::WlDataSource>,
        /// The icon the client requested to be used to be associated with the cursor icon
        /// during the drag'n'drop.
        icon: Option<wl_surface::WlSurface>,
    },
    /// The drag'n'drop action was finished by the user releasing the buttons
    ///
    /// At this point, any pointer icon should be removed.
    ///
    /// Note that this event will only be generated for client-initiated drag'n'drop session.
    DnDDropped,
    /// A client requested to read the server-set selection
    SendSelection {
        /// the requested mime type
        mime_type: String,
        /// the fd to write into
        fd: RawFd,
    },
}

enum Selection {
    Empty,
    Client(wl_data_source::WlDataSource),
    Compositor(SourceMetadata),
}

struct SeatData {
    known_devices: Vec<wl_data_device::WlDataDevice>,
    selection: Selection,
    log: ::slog::Logger,
    current_focus: Option<Client>,
}

impl SeatData {
    fn set_selection(&mut self, new_selection: Selection) {
        self.selection = new_selection;
        self.send_selection();
    }

    fn set_focus(&mut self, new_focus: Option<Client>) {
        self.current_focus = new_focus;
        self.send_selection();
    }

    fn send_selection(&mut self) {
        let client = match self.current_focus.as_ref() {
            Some(c) => c,
            None => return,
        };
        // first sanitize the selection, reseting it to null if the client holding
        // it dropped it
        let cleanup = if let Selection::Client(ref data_source) = self.selection {
            !data_source.as_ref().is_alive()
        } else {
            false
        };
        if cleanup {
            self.selection = Selection::Empty;
        }
        // then send it if appropriate
        match self.selection {
            Selection::Empty => {
                // send an empty selection
                for dd in &self.known_devices {
                    // skip data devices not belonging to our client
                    if dd.as_ref().client().map(|c| !c.equals(client)).unwrap_or(true) {
                        continue;
                    }
                    dd.selection(None);
                }
            }
            Selection::Client(ref data_source) => {
                for dd in &self.known_devices {
                    // skip data devices not belonging to our client
                    if dd.as_ref().client().map(|c| !c.equals(client)).unwrap_or(true) {
                        continue;
                    }
                    let source = data_source.clone();
                    let log = self.log.clone();
                    // create a corresponding data offer
                    let offer = client
                        .create_resource::<wl_data_offer::WlDataOffer>(dd.as_ref().version())
                        .unwrap();
                    offer.quick_assign(move |_offer, req, _| {
                        // selection data offers only care about the `receive` event
                        if let wl_data_offer::Request::Receive { fd, mime_type } = req {
                            // check if the source and associated mime type is still valid
                            let valid =
                                with_source_metadata(&source, |meta| meta.mime_types.contains(&mime_type))
                                    .unwrap_or(false)
                                    && source.as_ref().is_alive();
                            if !valid {
                                // deny the receive
                                debug!(log, "Denying a wl_data_offer.receive with invalid source.");
                            } else {
                                source.send(mime_type, fd);
                            }
                            let _ = ::nix::unistd::close(fd);
                        }
                    });
                    // advertize the offer to the client
                    dd.data_offer(&offer);
                    with_source_metadata(data_source, |meta| {
                        for mime_type in meta.mime_types.iter().cloned() {
                            offer.offer(mime_type);
                        }
                    })
                    .unwrap();
                    dd.selection(Some(&offer));
                }
            }
            Selection::Compositor(ref meta) => {
                for dd in &self.known_devices {
                    // skip data devices not belonging to our client
                    if dd.as_ref().client().map(|c| !c.equals(client)).unwrap_or(true) {
                        continue;
                    }
                    let log = self.log.clone();
                    let offer_meta = meta.clone();
                    let callback = dd
                        .as_ref()
                        .user_data()
                        .get::<DataDeviceData>()
                        .unwrap()
                        .callback
                        .clone();
                    // create a corresponding data offer
                    let offer = client
                        .create_resource::<wl_data_offer::WlDataOffer>(dd.as_ref().version())
                        .unwrap();
                    offer.quick_assign(move |_offer, req, _| {
                        // selection data offers only care about the `receive` event
                        if let wl_data_offer::Request::Receive { fd, mime_type } = req {
                            // check if the associated mime type is valid
                            if !offer_meta.mime_types.contains(&mime_type) {
                                // deny the receive
                                debug!(log, "Denying a wl_data_offer.receive with invalid source.");
                                let _ = ::nix::unistd::close(fd);
                            } else {
                                (&mut *callback.borrow_mut())(DataDeviceEvent::SendSelection {
                                    mime_type,
                                    fd,
                                });
                            }
                        }
                    });
                    // advertize the offer to the client
                    dd.data_offer(&offer);
                    for mime_type in meta.mime_types.iter().cloned() {
                        offer.offer(mime_type);
                    }
                    dd.selection(Some(&offer));
                }
            }
        }
    }
}

impl SeatData {
    fn new(log: ::slog::Logger) -> SeatData {
        SeatData {
            known_devices: Vec::new(),
            selection: Selection::Empty,
            log,
            current_focus: None,
        }
    }
}

/// Initialize the data device global
///
/// You can provide a callback to peek into the actions of your clients over the data devices
/// (allowing you to retrieve the current selection buffer, or intercept DnD data). See the
/// [`DataDeviceEvent`] type for details about what notifications you can receive. Note that this
/// closure will not receive notifications about dnd actions the compositor initiated, see
/// [`start_dnd`] for details about that.
///
/// You also need to provide a `(DndAction, DndAction) -> DndAction` closure that will arbitrate
/// the choice of action resulting from a drag'n'drop session. Its first argument is the set of
/// available actions (which is the intersection of the actions supported by the source and targets)
/// and the second argument is the preferred action reported by the target. If no action should be
/// chosen (and thus the drag'n'drop should abort on drop), return
/// [`DndAction::empty()`](wayland_server::protocol::wl_data_device_manager::DndAction::empty).
pub fn init_data_device<F, C, L>(
    display: &mut Display,
    callback: C,
    action_choice: F,
    logger: L,
) -> Global<wl_data_device_manager::WlDataDeviceManager>
where
    F: FnMut(DndAction, DndAction) -> DndAction + 'static,
    C: FnMut(DataDeviceEvent) + 'static,
    L: Into<Option<::slog::Logger>>,
{
    let log = crate::slog_or_fallback(logger).new(o!("smithay_module" => "data_device_mgr"));
    let action_choice = Rc::new(RefCell::new(action_choice));
    let callback = Rc::new(RefCell::new(callback));
    display.create_global(
        3,
        Filter::new(move |(ddm, _version), _, _| {
            implement_ddm(ddm, callback.clone(), action_choice.clone(), log.clone());
        }),
    )
}

/// Set the data device focus to a certain client for a given seat
pub fn set_data_device_focus(seat: &Seat, client: Option<Client>) {
    // ensure the seat user_data is ready
    // TODO: find a better way to retrieve a logger without requiring the user
    // to provide one ?
    // This should be a rare path anyway, it is unlikely that a client gets focus
    // before initializing its data device, which would already init the user_data.
    seat.user_data().insert_if_missing(|| {
        RefCell::new(SeatData::new(
            seat.arc.log.new(o!("smithay_module" => "data_device_mgr")),
        ))
    });
    let seat_data = seat.user_data().get::<RefCell<SeatData>>().unwrap();
    seat_data.borrow_mut().set_focus(client);
}

/// Set a compositor-provided selection for this seat
///
/// You need to provide the available mime types for this selection.
///
/// Whenever a client requests to read the selection, your callback will
/// receive a [`DataDeviceEvent::SendSelection`] event.
pub fn set_data_device_selection(seat: &Seat, mime_types: Vec<String>) {
    // TODO: same question as in set_data_device_focus
    seat.user_data().insert_if_missing(|| {
        RefCell::new(SeatData::new(
            seat.arc.log.new(o!("smithay_module" => "data_device_mgr")),
        ))
    });
    let seat_data = seat.user_data().get::<RefCell<SeatData>>().unwrap();
    seat_data
        .borrow_mut()
        .set_selection(Selection::Compositor(SourceMetadata {
            mime_types,
            dnd_action: DndAction::empty(),
        }));
}

/// Start a drag'n'drop from a resource controlled by the compositor
///
/// You'll receive events generated by the interaction of clients with your
/// drag'n'drop in the provided callback. See [`ServerDndEvent`] for details about
/// which events can be generated and what response is expected from you to them.
pub fn start_dnd<C>(
    seat: &Seat,
    serial: Serial,
    start_data: PointerGrabStartData,
    metadata: SourceMetadata,
    callback: C,
) where
    C: FnMut(ServerDndEvent) + 'static,
{
    // TODO: same question as in set_data_device_focus
    seat.user_data().insert_if_missing(|| {
        RefCell::new(SeatData::new(
            seat.arc.log.new(o!("smithay_module" => "data_device_mgr")),
        ))
    });
    if let Some(pointer) = seat.get_pointer() {
        pointer.set_grab(
            server_dnd_grab::ServerDnDGrab::new(
                start_data,
                metadata,
                seat.clone(),
                Rc::new(RefCell::new(callback)),
            ),
            serial,
        );
    }
}

fn implement_ddm<F, C>(
    ddm: Main<wl_data_device_manager::WlDataDeviceManager>,
    callback: Rc<RefCell<C>>,
    action_choice: Rc<RefCell<F>>,
    log: ::slog::Logger,
) -> wl_data_device_manager::WlDataDeviceManager
where
    F: FnMut(DndAction, DndAction) -> DndAction + 'static,
    C: FnMut(DataDeviceEvent) + 'static,
{
    use self::wl_data_device_manager::Request;
    ddm.quick_assign(move |_ddm, req, _data| match req {
        Request::CreateDataSource { id } => {
            self::data_source::implement_data_source(id);
        }
        Request::GetDataDevice { id, seat } => match Seat::from_resource(&seat) {
            Some(seat) => {
                // ensure the seat user_data is ready
                seat.user_data()
                    .insert_if_missing(|| RefCell::new(SeatData::new(log.clone())));
                let seat_data = seat.user_data().get::<RefCell<SeatData>>().unwrap();
                let data_device = implement_data_device(
                    id,
                    seat.clone(),
                    callback.clone(),
                    action_choice.clone(),
                    log.clone(),
                );
                seat_data.borrow_mut().known_devices.push(data_device);
            }
            None => {
                error!(log, "Unmanaged seat given to a data device.");
            }
        },
        _ => unreachable!(),
    });

    ddm.deref().clone()
}

struct DataDeviceData {
    callback: Rc<RefCell<dyn FnMut(DataDeviceEvent) + 'static>>,
    action_choice: Rc<RefCell<dyn FnMut(DndAction, DndAction) -> DndAction + 'static>>,
}

fn implement_data_device<F, C>(
    dd: Main<wl_data_device::WlDataDevice>,
    seat: Seat,
    callback: Rc<RefCell<C>>,
    action_choice: Rc<RefCell<F>>,
    log: ::slog::Logger,
) -> wl_data_device::WlDataDevice
where
    F: FnMut(DndAction, DndAction) -> DndAction + 'static,
    C: FnMut(DataDeviceEvent) + 'static,
{
    use self::wl_data_device::Request;
    let dd_data = DataDeviceData {
        callback: callback.clone(),
        action_choice,
    };
    dd.quick_assign(move |dd, req, _| match req {
        Request::StartDrag {
            source,
            origin,
            icon,
            serial,
        } => {
            /* TODO: handle the icon */
            let serial = Serial::from(serial);
            if let Some(pointer) = seat.get_pointer() {
                if pointer.has_grab(serial) {
                    if let Some(ref icon) = icon {
                        if compositor::give_role(icon, DND_ICON_ROLE).is_err() {
                            dd.as_ref().post_error(
                                wl_data_device::Error::Role as u32,
                                "Given surface already has an other role".into(),
                            );
                            return;
                        }
                    }
                    // The StartDrag is in response to a pointer implicit grab, all is good
                    (&mut *callback.borrow_mut())(DataDeviceEvent::DnDStarted {
                        source: source.clone(),
                        icon: icon.clone(),
                    });
                    let start_data = pointer.grab_start_data().unwrap();
                    pointer.set_grab(
                        dnd_grab::DnDGrab::new(
                            start_data,
                            source,
                            origin,
                            seat.clone(),
                            icon,
                            callback.clone(),
                        ),
                        serial,
                    );
                    return;
                }
            }
            debug!(log, "denying drag from client without implicit grab");
        }
        Request::SetSelection { source, .. } => {
            if let Some(keyboard) = seat.get_keyboard() {
                if dd
                    .as_ref()
                    .client()
                    .as_ref()
                    .map(|c| keyboard.has_focus(c))
                    .unwrap_or(false)
                {
                    let seat_data = seat.user_data().get::<RefCell<SeatData>>().unwrap();
                    (&mut *callback.borrow_mut())(DataDeviceEvent::NewSelection(source.clone()));
                    // The client has kbd focus, it can set the selection
                    seat_data
                        .borrow_mut()
                        .set_selection(source.map(Selection::Client).unwrap_or(Selection::Empty));
                    return;
                }
            }
            debug!(log, "denying setting selection by a non-focused client");
        }
        Request::Release => {
            // Clean up the known devices
            seat.user_data()
                .get::<RefCell<SeatData>>()
                .unwrap()
                .borrow_mut()
                .known_devices
                .retain(|ndd| ndd.as_ref().is_alive() && (!ndd.as_ref().equals(dd.as_ref())))
        }
        _ => unreachable!(),
    });
    dd.as_ref().user_data().set(|| dd_data);

    dd.deref().clone()
}

/// A simple action chooser for DnD negociation
///
/// If the preferred action is available, it'll pick it. Otherwise, it'll pick the first
/// available in the following order: Ask, Copy, Move.
pub fn default_action_chooser(available: DndAction, preferred: DndAction) -> DndAction {
    // if the preferred action is valid (a single action) and in the available actions, use it
    // otherwise, follow a fallback stategy
    if [DndAction::Move, DndAction::Copy, DndAction::Ask].contains(&preferred)
        && available.contains(preferred)
    {
        preferred
    } else if available.contains(DndAction::Ask) {
        DndAction::Ask
    } else if available.contains(DndAction::Copy) {
        DndAction::Copy
    } else if available.contains(DndAction::Move) {
        DndAction::Move
    } else {
        DndAction::empty()
    }
}
