//! Main window, ported from macos/app/Sources/MainWindowController.swift and
//! windows/app/MainWindow.xaml.cs: Livewire network, audio device (layout), input patch matrix,
//! transmitted outputs, advanced settings. The network service itself is never shown: the app
//! talks about network, channels and devices. Polls status at 5 Hz; configuration, sources and
//! interfaces every 2 s.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashSet};
use std::net::Ipv4Addr;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};
use serde_json::{json, Value};

use crate::client::{Client, DaemonError};
use crate::grid::{GridActions, GridHeader, GridLayout, GridRow, GridSides, InputGrid};
use crate::i18n::{tr, trf};
use crate::listener::Listener;
use crate::meter::Meter;
use crate::models::{
    device_channels, patch_kind, peak, DaemonConfig, DeviceMeters, DiscoveredSource, Iface,
    InputPatch, LinkStatus, OutputPatch, Tap,
};
use crate::settings::{ManualSource, Settings};

const MAX_PAIRS: u32 = 16;
const LAYOUTS: [(&str, &str); 2] = [
    (
        "duplex",
        "Two multichannel devices, “OpenLW In” and “OpenLW Out”",
    ),
    ("multi", "Several devices, “OpenLW In n” and “OpenLW Out n”"),
];
const LATENCIES: [(&str, &str); 3] = [
    ("low", "Low: ≈ 8 ms added, dedicated network"),
    ("normal", "Normal: ≈ 17 ms added"),
    (
        "safe",
        "Safe: ≈ 35 ms added, shared network or busy computer",
    ),
];
const DSCPS: [(u32, &str); 3] = [
    (46, "EF (46): Livewire default"),
    (34, "AF41 (34): recommended for AES67"),
    (0, "None (0)"),
];
const FORMATS: [(&str, &str); 3] = [
    ("standard", "Standard (5 ms)"),
    ("aes67", "AES67 (1 ms)"),
    ("livestream", "Livestream (0.25 ms)"),
];
const MANUAL_KINDS: [(&str, &str); 3] = [
    ("stereo", "Stereo"),
    ("backfeed", "Backfeed (To Source)"),
    ("surround", "8-Channel Surround"),
];

#[derive(Default)]
struct State {
    config: DaemonConfig,
    config_loaded: bool,
    meters: DeviceMeters,
    discovered: Vec<DiscoveredSource>,
    ifaces: Vec<Iface>,
    link: LinkStatus,
    reachable: Option<bool>,
    audio_nodes: Option<bool>,
    grid_rows: Vec<GridRow>,
    grid_layout: GridLayout,
    /// Previewed source: channel, patch kind.
    listening: Option<(u16, String)>,
}

pub struct Window {
    win: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    last_toast: RefCell<Option<adw::Toast>>,
    banner: adw::Banner,
    state_dot: gtk::Label,
    state_row: adw::ActionRow,
    iface_row: adw::ComboRow,
    iface_model: gtk::StringList,
    iface_names: RefCell<Vec<String>>,
    advertise_row: adw::SwitchRow,
    /// Layout choice, in `LAYOUTS` order.
    layout_checks: Vec<gtk::CheckButton>,
    naming_row: adw::SwitchRow,
    nodes_row: adw::ActionRow,
    nodes_icon: gtk::Image,
    out_device_row: adw::ActionRow,
    in_device_row: adw::ActionRow,
    inputs_group: adw::PreferencesGroup,
    in_count: adw::ComboRow,
    in_count_model: gtk::StringList,
    out_count: adw::ComboRow,
    out_count_model: gtk::StringList,
    grid: InputGrid,
    manual_entry: adw::EntryRow,
    manual_kind: gtk::DropDown,
    outputs_group: adw::PreferencesGroup,
    output_rows: RefCell<Vec<Rc<OutputRow>>>,
    advanced: adw::ExpanderRow,
    terminal_row: adw::EntryRow,
    latency_row: adw::ComboRow,
    dscp_row: adw::ComboRow,
    client: Arc<Client>,
    listener: RefCell<Listener>,
    settings: RefCell<Settings>,
    st: RefCell<State>,
    /// Programmatic control updates must not be taken as user choices.
    updating: Cell<bool>,
    busy: RefCell<HashSet<&'static str>>,
}

/// Connects a widget signal to a window method through a weak reference.
macro_rules! on {
    ($weak:expr, |$w:ident $(, $arg:pat_param)*| $body:expr) => {{
        let weak: Weak<Window> = $weak.clone();
        move |$($arg),*| {
            if let Some($w) = weak.upgrade() {
                $body
            }
        }
    }};
}

impl Window {
    pub fn build(app: &adw::Application) -> Rc<Self> {
        let w = Rc::new_cyclic(|weak: &Weak<Window>| Self::new(app, weak));
        // Signal handlers only hold weak references: the GTK window keeps this state alive
        // until it is destroyed.
        let keep = RefCell::new(Some(w.clone()));
        w.win
            .connect_destroy(move |_| drop(keep.borrow_mut().take()));
        w.connect(app);
        w.win.present();
        w.refresh_slow();
        w.refresh_status();
        let weak = Rc::downgrade(&w);
        glib::timeout_add_local(Duration::from_millis(200), move || match weak.upgrade() {
            Some(w) => {
                w.refresh_status();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        let weak = Rc::downgrade(&w);
        glib::timeout_add_local(Duration::from_secs(2), move || match weak.upgrade() {
            Some(w) => {
                w.refresh_slow();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        w
    }

    fn new(app: &adw::Application, weak: &Weak<Window>) -> Self {
        let settings = Settings::load();

        // ---------- Livewire network ----------
        let network = group(
            tr("Livewire Network"),
            tr("Clock: the computer's own. Drift relative to other devices is compensated automatically."),
        );
        let state_dot = gtk::Label::new(Some("●"));
        state_dot.add_css_class("dim-label");
        let state_row = adw::ActionRow::builder()
            .title(tr("Connecting to the OpenLW service…"))
            .build();
        state_row.add_prefix(&state_dot);
        let iface_model = gtk::StringList::new(&[]);
        let iface_row = adw::ComboRow::builder()
            .title(tr("Interface"))
            .model(&iface_model)
            .build();
        let advertise_row = adw::SwitchRow::builder()
            .title(tr("Advertise Outputs on the Network"))
            .subtitle(tr(
                "Other Livewire devices see the transmitted channels and their names.",
            ))
            .build();
        network.add(&state_row);
        network.add(&iface_row);
        network.add(&advertise_row);

        // ---------- Audio device (PipeWire nodes) ----------
        let device = group(
            tr("Audio Device"),
            tr("Two devices: every source goes to channels of “OpenLW In”, as with a multichannel sound card; suits applications that use one device per direction. Several devices: each source gets its own device, as wide as the source (1 channel when uncoupled, 8 for surround); suits applications that pick one input, such as video calls. PipeWire, PulseAudio and JACK applications see the same devices. After changing the layout or the names, select the device again in applications that find it by name."),
        );
        // Layout: one radio row per choice (the titles are too long for a drop-down).
        let mut layout_checks: Vec<gtk::CheckButton> = Vec::new();
        let mut layout_rows = Vec::new();
        for (_, title) in LAYOUTS {
            let check = gtk::CheckButton::new();
            check.set_valign(gtk::Align::Center);
            if let Some(first) = layout_checks.first() {
                check.set_group(Some(first));
            }
            let row = adw::ActionRow::builder()
                .title(tr(title))
                .activatable_widget(&check)
                .build();
            row.add_prefix(&check);
            layout_rows.push(row);
            layout_checks.push(check);
        }
        let naming_row = adw::SwitchRow::builder()
            .title(tr("Name Devices After Their Source"))
            .subtitle(tr(
                "For example “OpenLW In - Studio A@Omnia One (ch. 2)”. Several devices only.",
            ))
            .build();
        let nodes_icon = gtk::Image::from_icon_name("content-loading-symbolic");
        let nodes_row = adw::ActionRow::builder().title(tr("Audio Devices")).build();
        nodes_row.add_prefix(&nodes_icon);
        let out_device_row = adw::ActionRow::new();
        let in_device_row = adw::ActionRow::new();
        for row in &layout_rows {
            device.add(row);
        }
        device.add(&naming_row);
        device.add(&nodes_row);
        device.add(&out_device_row);
        device.add(&in_device_row);

        // ---------- Inputs ----------
        let inputs_group = group(tr("Inputs (Network to Computer)"), "");
        let in_count_model = gtk::StringList::new(&[]);
        let in_count = adw::ComboRow::builder().model(&in_count_model).build();
        inputs_group.add(&in_count);
        let grid = InputGrid::new(GridActions {
            group: Box::new(on!(weak, |w, r, h| w.toggle_group(r, h))),
            side: Box::new(on!(weak, |w, r, c, left| w.toggle_side(r, c, left))),
            coupling: Box::new(on!(weak, |w, h| w.toggle_coupling(h))),
            listen: Box::new(on!(weak, |w, r| w.toggle_listen(r))),
            remove: Box::new(on!(weak, |w, r| w.remove_row(r))),
        });
        // The grid scrolls its cells horizontally: the card keeps the window width.
        let grid_card = gtk::Box::new(gtk::Orientation::Vertical, 0);
        grid_card.add_css_class("card");
        grid_card.append(grid.widget());
        let manual = adw::PreferencesGroup::new();
        let manual_entry = adw::EntryRow::builder()
            .title(tr("Unadvertised source, channel (1 to 32766)"))
            .input_purpose(gtk::InputPurpose::Digits)
            .build();
        let manual_kind = gtk::DropDown::from_strings(&MANUAL_KINDS.map(|(_, t)| tr(t)));
        manual_kind.set_valign(gtk::Align::Center);
        let add = gtk::Button::with_label(tr("Add to Grid"));
        add.set_valign(gtk::Align::Center);
        manual_entry.add_suffix(&manual_kind);
        manual_entry.add_suffix(&add);
        manual.add(&manual_entry);
        add.connect_clicked(on!(weak, |w, _| w.add_manual()));
        manual_entry.connect_entry_activated(on!(weak, |w, _| w.add_manual()));

        // ---------- Outputs ----------
        let outputs_group = group(tr("Outputs (Computer to Network)"), "");
        let out_count_model = gtk::StringList::new(&[]);
        let out_count = adw::ComboRow::builder().model(&out_count_model).build();
        outputs_group.add(&out_count);

        // ---------- Advanced settings ----------
        let advanced_group = adw::PreferencesGroup::new();
        let advanced = adw::ExpanderRow::builder()
            .title(tr("Advanced Settings"))
            .subtitle(tr("Advertised name, receive latency, network priority"))
            .expanded(settings.advanced_visible)
            .build();
        let terminal_row = adw::EntryRow::builder()
            .title(tr("Advertised name (empty: computer name)"))
            .show_apply_button(true)
            .tooltip_text(tr("Name shown by other Livewire devices: 32 characters at most, accented letters replaced."))
            .build();
        let latency_row = adw::ComboRow::builder()
            .title(tr("Receive Latency"))
            .subtitle(tr("Added to the recording software's own latency"))
            .tooltip_text(tr("Buffers added to the recording software's own buffer. The lower the latency, the more likely a network or computer delay causes a brief dropout."))
            .model(&gtk::StringList::new(&LATENCIES.map(|(_, t)| tr(t))))
            .build();
        let dscp_row = adw::ComboRow::builder()
            .title(tr("Network Priority (DSCP)"))
            .subtitle(tr("Depends on the switches' QoS"))
            .tooltip_text(tr("Marking of transmitted audio streams. Choose the value expected by your switches' QoS policy."))
            .model(&gtk::StringList::new(&DSCPS.map(|(_, t)| tr(t))))
            .build();
        advanced.add_row(&terminal_row);
        advanced.add_row(&latency_row);
        advanced.add_row(&dscp_row);
        advanced_group.add(&advanced);

        // ---------- Window ----------
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(24)
            .margin_top(12)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .build();
        content.append(&network);
        content.append(&device);
        let input_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
        input_box.append(&inputs_group);
        input_box.append(&grid_card);
        input_box.append(&manual);
        content.append(&input_box);
        content.append(&outputs_group);
        content.append(&advanced_group);
        let clamp = adw::Clamp::builder()
            .maximum_size(1100)
            .tightening_threshold(900)
            .child(&content)
            .build();
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&clamp)
            .vexpand(true)
            .build();
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&scroll));
        let banner = adw::Banner::builder()
            .title(DaemonError::Unreachable.message())
            .button_label(tr("Start Service"))
            .build();
        let menu = gio::Menu::new();
        menu.append(Some(tr("About OpenLW")), Some("app.about"));
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .tooltip_text(tr("Main Menu"))
            .primary(true)
            .build();
        let header = adw::HeaderBar::new();
        header.pack_end(&menu_button);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.add_top_bar(&banner);
        toolbar.set_content(Some(&toasts));
        let win = adw::ApplicationWindow::builder()
            .application(app)
            .title("OpenLW")
            .default_width(1000)
            .default_height(860)
            .content(&toolbar)
            .build();
        win.set_size_request(360, 400);

        Self {
            win,
            toasts,
            last_toast: RefCell::default(),
            banner,
            state_dot,
            state_row,
            iface_row,
            iface_model,
            iface_names: RefCell::default(),
            advertise_row,
            layout_checks,
            naming_row,
            nodes_row,
            nodes_icon,
            out_device_row,
            in_device_row,
            inputs_group,
            in_count,
            in_count_model,
            out_count,
            out_count_model,
            grid,
            manual_entry,
            manual_kind,
            outputs_group,
            output_rows: RefCell::default(),
            advanced,
            terminal_row,
            latency_row,
            dscp_row,
            client: Arc::new(Client::default()),
            listener: RefCell::default(),
            settings: RefCell::new(settings),
            st: RefCell::default(),
            updating: Cell::new(false),
            busy: RefCell::default(),
        }
    }

    /// Signals needing the finished window.
    fn connect(self: &Rc<Self>, app: &adw::Application) {
        let weak = Rc::downgrade(self);
        self.updating.set(true);
        self.update_layout_texts(false);
        self.naming_row.set_sensitive(false);
        self.updating.set(false);
        self.iface_row
            .connect_selected_notify(on!(weak, |w, _| w.iface_changed()));
        self.advertise_row
            .connect_active_notify(on!(weak, |w, _| w.advertise_changed()));
        for (check, (value, _)) in self.layout_checks.iter().zip(LAYOUTS) {
            check.connect_toggled(on!(weak, |w, c| if c.is_active() {
                w.layout_changed(value)
            }));
        }
        self.naming_row
            .connect_active_notify(on!(weak, |w, _| w.naming_changed()));
        self.in_count
            .connect_selected_notify(on!(weak, |w, _| w.channel_count_changed()));
        self.out_count
            .connect_selected_notify(on!(weak, |w, _| w.channel_count_changed()));
        self.terminal_row
            .connect_apply(on!(weak, |w, _| w.commit_terminal_name()));
        self.terminal_row
            .connect_entry_activated(on!(weak, |w, _| w.commit_terminal_name()));
        self.latency_row
            .connect_selected_notify(on!(weak, |w, _| w.latency_changed()));
        self.dscp_row
            .connect_selected_notify(on!(weak, |w, _| w.dscp_changed()));
        self.advanced.connect_expanded_notify(on!(weak, |w, row| {
            let mut s = w.settings.borrow_mut();
            s.advanced_visible = row.is_expanded();
            s.save();
        }));
        self.banner
            .connect_button_clicked(on!(weak, |w, _| w.start_service()));
        let w = weak.clone();
        self.win.connect_close_request(move |_| {
            if let Some(w) = w.upgrade() {
                w.listener.borrow_mut().stop();
            }
            glib::Propagation::Proceed
        });
        self.rebuild_output_rows(1, false);

        let about = gio::SimpleAction::new("about", None);
        about.connect_activate(on!(weak, |w, _, _| w.show_about()));
        app.add_action(&about);
    }

    // ---------- Polling ----------

    /// Sends `request` off the UI thread; `done` runs on the UI thread if the window still exists.
    fn request(
        self: &Rc<Self>,
        request: Value,
        done: impl FnOnce(&Rc<Self>, Result<Value, DaemonError>) + 'static,
    ) {
        let client = self.client.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let r = gio::spawn_blocking(move || client.call(&request))
                .await
                .unwrap_or(Err(DaemonError::Unreachable));
            if let Some(w) = weak.upgrade() {
                done(&w, r);
            }
        });
    }

    /// Sends `request` unless a request of the same kind is still pending.
    fn poll(
        self: &Rc<Self>,
        key: &'static str,
        request: Value,
        handle: impl FnOnce(&Rc<Self>, &Value) + 'static,
    ) {
        if !self.busy.borrow_mut().insert(key) {
            return;
        }
        self.request(request, move |w, r| {
            w.busy.borrow_mut().remove(key);
            match r {
                Ok(reply) => handle(w, &reply),
                Err(DaemonError::Unreachable) => w.set_reachable(false),
                Err(_) => {}
            }
        });
    }

    fn refresh_status(self: &Rc<Self>) {
        self.poll("status", json!({"cmd": "status"}), |w, reply| {
            let Some(status) = reply.get("status").filter(|s| s.is_object()) else {
                return;
            };
            w.set_reachable(true);
            let next = LinkStatus::from(status);
            let nodes = status.get("audio_nodes").and_then(Value::as_bool);
            let (stop, changed, nodes_changed) = {
                let mut st = w.st.borrow_mut();
                st.meters = DeviceMeters::from(status);
                let link = &st.link;
                // Interface changed: group membership no longer valid.
                let stop = st.listening.is_some()
                    && (next.iface != link.iface || next.ipv4 != link.ipv4 || next.searching);
                let changed = next.searching != link.searching
                    || next.iface != link.iface
                    || next.auto != link.auto;
                let nodes_changed = st.audio_nodes != nodes;
                st.link = next;
                st.audio_nodes = nodes;
                (stop, changed, nodes_changed)
            };
            if stop {
                w.stop_listening();
            }
            w.show_link();
            if changed {
                w.update_iface_combo();
            }
            if nodes_changed {
                w.show_nodes();
            }
            w.update_meters();
        });
    }

    fn refresh_slow(self: &Rc<Self>) {
        self.poll("config", json!({"cmd": "config"}), |w, reply| {
            if let Some(c) = reply.get("config").filter(|c| c.is_object()) {
                w.apply_config(DaemonConfig::from(c));
            }
        });
        self.poll("sources", json!({"cmd": "sources"}), |w, reply| {
            let discovered = reply
                .get("sources")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(DiscoveredSource::from).collect())
                .unwrap_or_default();
            w.st.borrow_mut().discovered = discovered;
            w.update_grid();
        });
        self.poll("ifaces", json!({"cmd": "ifaces"}), |w, reply| {
            let ifaces = reply
                .get("ifaces")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Iface::from).collect())
                .unwrap_or_default();
            w.st.borrow_mut().ifaces = ifaces;
            w.update_iface_combo();
        });
    }

    fn set_reachable(&self, ok: bool) {
        let was = self.st.borrow_mut().reachable.replace(ok);
        if was == Some(ok) {
            return;
        }
        self.banner.set_revealed(!ok);
        if !ok {
            set_dot(&self.state_dot, "error");
            self.state_row.set_title(tr("OpenLW service unreachable"));
            self.state_row.set_subtitle("");
            self.st.borrow_mut().audio_nodes = None;
            self.show_nodes();
        }
    }

    /// Status line: active interface, or network search.
    fn show_link(&self) {
        let st = self.st.borrow();
        let link = &st.link;
        if link.searching {
            set_dot(&self.state_dot, "warning");
            if link.auto {
                self.state_row
                    .set_title(tr("Searching for the Livewire network"));
                self.state_row.set_subtitle(tr(
                    "Connect the computer to the Livewire network, or choose the interface.",
                ));
            } else {
                self.state_row.set_title(&trf(
                    "Interface “{name}” unavailable",
                    &[("name", &st.config.iface)],
                ));
                self.state_row
                    .set_subtitle(tr("Plug it in, or choose Automatic."));
            }
            return;
        }
        set_dot(&self.state_dot, "success");
        let name = if link.friendly == link.iface {
            link.iface.clone()
        } else {
            format!("{} ({})", link.friendly, link.iface)
        };
        self.state_row.set_title(&trf(
            "Connected to the Livewire network via {name}",
            &[("name", &name)],
        ));
        self.state_row.set_subtitle(&if link.auto {
            trf(
                "{address} · interface chosen automatically",
                &[("address", &link.ipv4)],
            )
        } else {
            link.ipv4.clone()
        });
    }

    /// PipeWire node state, reported by the Linux service.
    fn show_nodes(&self) {
        let st = self.st.borrow();
        let (icon, title, subtitle) = match (st.reachable, st.audio_nodes) {
            (Some(true), Some(true)) => (
                "object-select-symbolic",
                tr("Devices published in PipeWire"),
                tr("OpenLW devices appear in the sound settings and in audio software."),
            ),
            (Some(true), Some(false)) => (
                "dialog-warning-symbolic",
                tr("PipeWire unreachable"),
                tr("OpenLW devices will appear as soon as PipeWire responds (retrying every 5 s)."),
            ),
            (Some(true), None) => (
                "dialog-warning-symbolic",
                tr("No audio devices"),
                tr("This OpenLW service was built without PipeWire. Install your distribution's OpenLW package."),
            ),
            _ => (
                "content-loading-symbolic",
                tr("Audio Devices"),
                tr("Available when the OpenLW service is running."),
            ),
        };
        self.nodes_icon.set_icon_name(Some(icon));
        self.nodes_row.set_title(title);
        self.nodes_row.set_subtitle(subtitle);
    }

    // ---------- Display updates ----------

    /// Texts that depend on the layout: count menus, hints, device rows. Call with `updating`
    /// set (the count menus lose their selection).
    fn update_layout_texts(&self, multi: bool) {
        for (model, inputs) in [(&self.in_count_model, true), (&self.out_count_model, false)] {
            let items = count_titles(multi, inputs);
            let refs: Vec<&str> = items.iter().map(String::as_str).collect();
            model.splice(0, model.n_items(), &refs);
        }
        let (in_title, out_title) = if multi {
            (tr("Input Devices"), tr("Output Devices"))
        } else {
            (
                tr("Received Livewire Channels"),
                tr("Transmitted Livewire Channels"),
            )
        };
        self.in_count.set_title(in_title);
        self.out_count.set_title(out_title);
        if multi {
            self.inputs_group.set_description(Some(tr("Click a cell to send the source to that device in stereo; click again to release it. The link button above a device uncouples it: the device becomes mono and each cell offers the left (L) and right (R) sides of the source, both for L+R. Applications record from “OpenLW In n”. The headphone button plays the source on the computer's audio output, without patching it.")));
            self.outputs_group.set_description(Some(tr("Each “OpenLW Out n” device is transmitted on the Livewire channel of your choice, under the name given. Changes take effect while transmission is on.")));
            self.out_device_row.set_title(tr("OpenLW Out n"));
            self.out_device_row.set_subtitle(tr("Choose one as the output: what applications play to it is transmitted to the network, according to the outputs set below."));
            self.in_device_row.set_title(tr("OpenLW In n"));
            self.in_device_row.set_subtitle(tr("Choose one as the input: each device receives the source patched to it in the input grid."));
        } else {
            self.inputs_group.set_description(Some(tr("Click a cell to send the source in stereo to that pair of “OpenLW In”; click again to release it. The link button above a pair uncouples it: each input then offers the left (L) and right (R) sides of the source, both for L+R, and a source can feed several inputs. The headphone button plays the source on the computer's audio output, without patching it.")));
            self.outputs_group.set_description(Some(tr("Each OpenLW Out channel pair is transmitted on the Livewire channel of your choice, under the name given. Changes take effect while transmission is on.")));
            self.out_device_row.set_title(tr("OpenLW Out"));
            self.out_device_row.set_subtitle(tr("Choose it as the output: what applications play to it is transmitted to the network, according to the outputs set below."));
            self.in_device_row.set_title(tr("OpenLW In"));
            self.in_device_row.set_subtitle(tr(
                "Choose it as the input: it receives audio from the network, according to the input grid.",
            ));
        }
    }

    fn apply_config(self: &Rc<Self>, c: DaemonConfig) {
        let (rows_changed, layout_changed, editing_name) = {
            let st = self.st.borrow();
            let layout_changed = !st.config_loaded || c.layout != st.config.layout;
            (
                layout_changed || c.channels_to_net != st.config.channels_to_net,
                layout_changed,
                self.editing(&self.terminal_row),
            )
        };
        self.updating.set(true);
        self.advertise_row.set_active(c.advertise);
        if layout_changed {
            self.update_layout_texts(c.multi());
        }
        for (check, (value, _)) in self.layout_checks.iter().zip(LAYOUTS) {
            check.set_active(value == c.layout);
        }
        self.naming_row.set_active(c.custom_names);
        self.naming_row.set_sensitive(c.multi());
        if !editing_name && self.terminal_row.text() != c.terminal_name {
            self.terminal_row.set_text(&c.terminal_name);
        }
        self.latency_row.set_selected(
            LATENCIES
                .iter()
                .position(|(v, _)| *v == c.latency)
                .map_or(gtk::INVALID_LIST_POSITION, |i| i as u32),
        );
        // Value outside the list: no selection.
        self.dscp_row.set_selected(
            DSCPS
                .iter()
                .position(|(d, _)| d << 2 == c.tos)
                .map_or(gtk::INVALID_LIST_POSITION, |i| i as u32),
        );
        self.in_count
            .set_selected((c.channels_from_net / 2).clamp(1, MAX_PAIRS) - 1);
        self.out_count
            .set_selected((c.channels_to_net / 2).clamp(1, MAX_PAIRS) - 1);
        self.updating.set(false);
        if rows_changed {
            let count = if c.multi() {
                c.out_devices()
            } else {
                c.channels_to_net / 2
            };
            self.rebuild_output_rows(count.max(1), c.multi());
        }
        for row in self.output_rows.borrow().iter() {
            row.show(output_patch(&c, row), self);
        }
        {
            let mut st = self.st.borrow_mut();
            st.grid_layout = grid_layout(&c);
            st.config = c;
            st.config_loaded = true;
        }
        self.update_iface_combo();
        self.update_grid();
    }

    /// Menu: automatic selection, then Ethernet interfaces (plus the configured one if not
    /// Ethernet).
    fn update_iface_combo(&self) {
        let (items, wanted) = {
            let st = self.st.borrow();
            let (config, link) = (&st.config, &st.link);
            let auto = config.auto_iface();
            let auto_title = if !auto {
                tr("Automatic").to_string()
            } else if link.searching {
                tr("Automatic · searching").to_string()
            } else {
                trf("Automatic · {name}", &[("name", &link.friendly)])
            };
            let mut items = vec![(auto_title, "auto".to_string())];
            let mut found: Vec<&Iface> = st
                .ifaces
                .iter()
                .filter(|i| i.candidate || (!auto && i.name == config.iface))
                .collect();
            found.sort_by_key(|i| (!i.livewire, i.name.clone()));
            items.extend(found.iter().map(|i| (i.title(), i.name.clone())));
            if !auto && st.ifaces.iter().all(|i| i.name != config.iface) {
                items.push((
                    trf("{name} (unavailable)", &[("name", &config.iface)]),
                    config.iface.clone(),
                ));
            }
            let wanted = if auto {
                "auto".to_string()
            } else {
                config.iface.clone()
            };
            (items, wanted)
        };
        self.updating.set(true);
        let titles: Vec<&str> = items.iter().map(|(t, _)| t.as_str()).collect();
        let names: Vec<String> = items.iter().map(|(_, n)| n.clone()).collect();
        let current: Vec<String> = (0..self.iface_model.n_items())
            .filter_map(|i| self.iface_model.string(i).map(String::from))
            .collect();
        if current != titles || *self.iface_names.borrow() != names {
            self.iface_model
                .splice(0, self.iface_model.n_items(), &titles);
            *self.iface_names.borrow_mut() = names;
        }
        let idx = self.iface_names.borrow().iter().position(|n| *n == wanted);
        self.iface_row
            .set_selected(idx.map_or(gtk::INVALID_LIST_POSITION, |i| i as u32));
        self.updating.set(false);
    }

    /// Matrix rows: discovered sources, then manual entries, then configured unadvertised
    /// streams.
    fn update_grid(&self) {
        let (rows, layout, listening) = {
            let st = self.st.borrow();
            let settings = self.settings.borrow();
            let mut rows: Vec<GridRow> = Vec::new();
            let mut seen = HashSet::new();
            let mut add = |s: DiscoveredSource, origin: &str, removable: bool| {
                if seen.insert((s.channel, s.patch_kind().to_string())) {
                    let (groups, sides) =
                        grid_cells(&st.config, &st.grid_layout, s.channel, s.patch_kind());
                    rows.push(GridRow {
                        groups,
                        sides,
                        source: s,
                        origin: origin.into(),
                        removable,
                    });
                }
            };
            for s in &st.discovered {
                add(s.clone(), &s.terminal.clone(), false);
            }
            for m in &settings.manual {
                add(
                    DiscoveredSource::manual(m.channel, &m.kind),
                    tr("manual"),
                    true,
                );
            }
            for p in &st.config.inputs {
                if let Some(ch) = p.channel {
                    add(
                        DiscoveredSource::manual(ch, &p.kind),
                        tr("not advertised"),
                        true,
                    );
                }
            }
            let listening = st.listening.as_ref().and_then(|(ch, kind)| {
                rows.iter()
                    .position(|r| r.source.channel == *ch && r.source.patch_kind() == kind)
            });
            (rows, st.grid_layout.clone(), listening)
        };
        self.st.borrow_mut().grid_rows = rows.clone();
        self.grid.update(rows, layout, listening);
        self.update_meters();
    }

    fn update_meters(&self) {
        let st = self.st.borrow();
        let c = &st.config;
        // Concatenated channels (meters, route state) of each header group.
        let groups: Vec<Vec<u32>> = if c.multi() {
            let widths = if st.meters.in_widths.len() == c.in_devices() as usize {
                st.meters.in_widths.clone()
            } else {
                DaemonConfig::in_widths(&c.inputs, &c.uncoupled, c.in_devices())
            };
            (1..=c.in_devices().max(1))
                .map(|n| device_channels(n, &widths))
                .collect()
        } else {
            st.grid_layout
                .headers
                .iter()
                .map(|h| (h.columns.start as u32 + 1..=h.columns.end as u32).collect())
                .collect()
        };
        let levels: Vec<Vec<Option<f64>>> = groups
            .iter()
            .map(|chs| {
                chs.iter()
                    .take(2)
                    .map(|&ch| peak(&st.meters.from_net, ch))
                    .collect()
            })
            .collect();
        let status: Vec<&str> = groups
            .iter()
            .map(|chs| {
                match st
                    .meters
                    .inputs
                    .iter()
                    .find(|(routed, _)| routed.iter().any(|r| chs.contains(r)))
                {
                    None => tr("free"),
                    Some((_, true)) => tr("receiving audio"),
                    Some((_, false)) => tr("waiting"),
                }
            })
            .collect();
        let listen = if st.listening.is_some() {
            self.listener.borrow().take_peak()
        } else {
            None
        };
        self.grid.show_levels(&levels, &status, listen);
        let rows = self.output_rows.borrow();
        let out_widths = if st.meters.out_widths.len() == rows.len() {
            st.meters.out_widths.clone()
        } else {
            vec![2; rows.len()]
        };
        for row in rows.iter() {
            let chs = row
                .device
                .map_or_else(|| row.pair.clone(), |d| device_channels(d, &out_widths));
            let levels: Vec<Option<f64>> = chs
                .iter()
                .take(2)
                .map(|&ch| peak(&st.meters.to_net, ch))
                .collect();
            row.meter.show(&levels);
        }
    }

    fn show_error(&self, message: Option<String>) {
        if let Some(t) = self.last_toast.borrow_mut().take() {
            t.dismiss();
        }
        if let Some(m) = message {
            let toast = adw::Toast::builder().title(m).timeout(8).build();
            self.toasts.add_toast(toast.clone());
            *self.last_toast.borrow_mut() = Some(toast);
        }
    }

    /// Output rows: one per pair (duplex layout) or per output device (multi layout).
    fn rebuild_output_rows(self: &Rc<Self>, count: u32, multi: bool) {
        for row in self.output_rows.borrow_mut().drain(..) {
            self.outputs_group.remove(&row.row);
        }
        let weak = Rc::downgrade(self);
        for i in 0..count {
            let row = if multi {
                OutputRow::new(vec![1, 2], Some(i + 1), &weak)
            } else {
                OutputRow::new(vec![2 * i + 1, 2 * i + 2], None, &weak)
            };
            self.outputs_group.add(&row.row);
            self.output_rows.borrow_mut().push(row);
        }
    }

    /// Whether the user is typing in `widget` (focus inside it).
    fn editing(&self, widget: &impl IsA<gtk::Widget>) -> bool {
        gtk::prelude::GtkWindowExt::focus(&self.win)
            .is_some_and(|f| f.is_ancestor(widget) || f == *widget.upcast_ref())
    }

    // ---------- Actions ----------

    /// Modification request; the returned configuration replaces the displayed state.
    fn mutate(self: &Rc<Self>, request: Value) {
        self.request(request, |w, r| match r {
            Ok(reply) => {
                w.show_error(None);
                if let Some(c) = reply.get("config").filter(|c| c.is_object()) {
                    w.apply_config(DaemonConfig::from(c));
                }
            }
            Err(e) => {
                w.show_error(Some(e.message()));
                w.refresh_slow();
            }
        });
    }

    /// Two-button confirmation; `proceed` runs on `accept`, otherwise the configured state is
    /// shown again. `dismissable`: “Do not ask again” check box (width warning).
    fn confirm(
        self: &Rc<Self>,
        heading: &str,
        body: &str,
        accept: &str,
        destructive: bool,
        dismissable: bool,
        proceed: impl FnOnce(&Rc<Self>) + 'static,
    ) {
        let dialog = adw::MessageDialog::new(Some(&self.win), Some(heading), Some(body));
        dialog.add_responses(&[("cancel", tr("Cancel")), ("accept", accept)]);
        dialog.set_response_appearance(
            "accept",
            if destructive {
                adw::ResponseAppearance::Destructive
            } else {
                adw::ResponseAppearance::Suggested
            },
        );
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let skip = dismissable.then(|| {
            let check = gtk::CheckButton::with_label(tr("Do not ask again"));
            check.set_halign(gtk::Align::Center);
            dialog.set_extra_child(Some(&check));
            check
        });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let answer = dialog.choose_future().await;
            let Some(w) = weak.upgrade() else { return };
            if skip.is_some_and(|c| c.is_active()) {
                let mut s = w.settings.borrow_mut();
                s.skip_width_warning = true;
                s.save();
            }
            if answer == "accept" {
                proceed(&w);
            } else {
                let c = w.st.borrow().config.clone();
                w.apply_config(c);
            }
        });
    }

    fn iface_changed(self: &Rc<Self>) {
        if self.updating.get() {
            return;
        }
        let idx = self.iface_row.selected() as usize;
        let Some(name) = self.iface_names.borrow().get(idx).cloned() else {
            return;
        };
        let same = {
            let c = &self.st.borrow().config;
            if name == "auto" {
                c.auto_iface()
            } else {
                name == c.iface
            }
        };
        if !same {
            self.mutate(json!({"cmd": "set_iface", "iface": name}));
        }
    }

    fn advertise_changed(self: &Rc<Self>) {
        if self.updating.get() {
            return;
        }
        self.mutate(json!({"cmd": "set_advertise", "advertise": self.advertise_row.is_active()}));
    }

    fn layout_changed(self: &Rc<Self>, value: &'static str) {
        if self.updating.get() || !self.st.borrow().config_loaded {
            return;
        }
        if value == self.st.borrow().config.layout {
            return;
        }
        let heading = if value == "multi" {
            tr("Switch to Several Devices?")
        } else {
            tr("Switch to “OpenLW In” and “OpenLW Out”?")
        };
        let request = json!({"cmd": "set_device_layout", "layout": value});
        self.confirm(
            heading,
            tr("Patches move between input pair n and device n (a mono patch on input c goes to device ⌈c/2⌉); those that no longer fit are released. Audio on OpenLW devices stops for a moment while PipeWire publishes them again."),
            tr("Switch"),
            false,
            false,
            move |w| w.mutate(request),
        );
    }

    fn naming_changed(self: &Rc<Self>) {
        if self.updating.get() {
            return;
        }
        let on = self.naming_row.is_active();
        if on != self.st.borrow().config.custom_names {
            self.mutate(json!({"cmd": "set_device_naming", "enabled": on}));
        }
    }

    fn channel_count_changed(self: &Rc<Self>) {
        if self.updating.get() || !self.st.borrow().config_loaded {
            return;
        }
        let to_net = 2 * (self.out_count.selected() + 1);
        let from_net = 2 * (self.in_count.selected() + 1);
        let (lost_out, lost_in, multi) = {
            let c = &self.st.borrow().config;
            if to_net == c.channels_to_net && from_net == c.channels_from_net {
                return;
            }
            let multi = c.multi();
            let lost_out = c
                .outputs
                .iter()
                .filter(|o| {
                    if multi {
                        o.device.unwrap_or(0) > to_net.div_ceil(2)
                    } else {
                        o.device_channels
                            .as_ref()
                            .and_then(|d| d.iter().max())
                            .is_some_and(|&m| m > to_net)
                    }
                })
                .count();
            let lost_in = lost_inputs(c, from_net);
            (lost_out, lost_in, multi)
        };
        let request = json!({"cmd": "set_device_channels", "to_net": to_net, "from_net": from_net});
        if lost_out + lost_in == 0 {
            self.mutate(request);
            return;
        }
        let mut lost = Vec::new();
        if lost_out == 1 {
            lost.push(tr("1 transmission stopped").to_string());
        } else if lost_out > 1 {
            lost.push(trf("{n} transmissions stopped", &[("n", &lost_out)]));
        }
        if lost_in == 1 {
            lost.push(tr("1 source removed from the inputs").to_string());
        } else if lost_in > 1 {
            lost.push(trf(
                "{n} sources removed from the inputs",
                &[("n", &lost_in)],
            ));
        }
        let body = trf(
            "{lost}. Software using OpenLW devices finds them again after a few seconds.",
            &[("lost", &capitalize(&lost.join(", ")))],
        );
        let heading = if multi {
            tr("Reduce the Number of Devices?")
        } else {
            tr("Reduce the Number of Channels?")
        };
        self.confirm(heading, &body, tr("Reduce"), true, false, move |w| {
            w.mutate(request)
        });
    }

    fn add_manual(&self) {
        let text = self.manual_entry.text();
        let Some(ch) = text
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|c| (1..=32766).contains(c))
        else {
            self.show_error(Some(invalid_channel()));
            return;
        };
        let kind = MANUAL_KINDS
            .get(self.manual_kind.selected() as usize)
            .map_or("stereo", |(k, _)| k);
        {
            let mut s = self.settings.borrow_mut();
            if !s.manual.iter().any(|m| m.channel == ch && m.kind == kind) {
                s.manual.push(ManualSource {
                    channel: ch,
                    kind: kind.into(),
                });
                s.save();
            }
        }
        self.manual_entry.set_text("");
        self.show_error(None);
        self.update_grid();
    }

    /// Click on a coupled group (or on a group of a surround source): patches the source in
    /// stereo (surround: 8 channels from the group start; uncoupled device: L+R), or releases
    /// it from the group.
    fn toggle_group(self: &Rc<Self>, row: usize, h: usize) {
        let (r, c, layout) = {
            let st = self.st.borrow();
            match st.grid_rows.get(row) {
                Some(r) => (r.clone(), st.config.clone(), st.grid_layout.clone()),
                None => return,
            }
        };
        let Some(g) = layout.headers.get(h) else {
            return;
        };
        let s = &r.source;
        let kind = s.patch_kind();
        let surround = kind == "surround";
        let n = g.number;
        if r.groups.contains_key(&h) || r.sides.keys().any(|k| g.columns.contains(k)) {
            // Release: the group's inputs, or the whole surround block.
            if c.multi() {
                let w = DaemonConfig::in_widths(&c.inputs, &c.uncoupled, c.in_devices())
                    .get(n as usize - 1)
                    .copied()
                    .unwrap_or(2);
                let request = json!({"cmd": "unpatch_input", "device": n,
                    "device_channels": (1..=w).collect::<Vec<u32>>()});
                self.send(request, Some(n), None);
            } else {
                let block: Vec<u32> = if surround {
                    input_patch(&c, s.channel, kind)
                        .map(|p| p.taps.iter().map(|t| t.channel).collect())
                        .unwrap_or_default()
                } else {
                    (g.columns.start as u32 + 1..=g.columns.end as u32).collect()
                };
                self.mutate(json!({"cmd": "unpatch_input", "device_channels": block}));
            }
            return;
        }
        match group_taps(&c, g, surround) {
            Ok(taps) => {
                let width = taps.len();
                let request = json!({"cmd": "patch_input", "channel": s.channel, "kind": kind,
                    "taps": taps.iter().map(Tap::json).collect::<Vec<_>>()});
                self.send(request, c.multi().then_some(n), Some(width));
            }
            Err(e) => self.show_error(Some(DaemonError::Refused(e).message())),
        }
    }

    /// Click on one side of an uncoupled column: adds or removes that side of the source on
    /// this input (both sides: L+R).
    fn toggle_side(self: &Rc<Self>, row: usize, column: usize, left: bool) {
        let (s, c) = {
            let st = self.st.borrow();
            match st.grid_rows.get(row) {
                Some(r) => (r.source.clone(), st.config.clone()),
                None => return,
            }
        };
        let multi = c.multi();
        let device = multi.then_some(column as u32 + 1);
        let channel = if multi { 1 } else { column as u32 + 1 };
        let current = input_patch(&c, s.channel, s.patch_kind())
            .and_then(|p| {
                p.taps
                    .iter()
                    .find(|t| t.device == device && t.channel == channel)
            })
            .map(|t| t.from.clone())
            .unwrap_or_default();
        let tap = Tap {
            device,
            channel,
            from: toggled_side(&current, if left { 1 } else { 2 }),
        };
        // An uncoupled device stays 1 channel wide: no width warning.
        self.mutate(
            json!({"cmd": "patch_input", "channel": s.channel, "kind": s.patch_kind(),
            "taps": [tap.json()]}),
        );
    }

    /// Link button: couples or uncouples a pair (multi layout: a device, whose width changes).
    fn toggle_coupling(self: &Rc<Self>, h: usize) {
        let (c, g, skip) = {
            let st = self.st.borrow();
            match st.grid_layout.headers.get(h) {
                Some(g) => (
                    st.config.clone(),
                    g.clone(),
                    self.settings.borrow().skip_width_warning,
                ),
                None => return,
            }
        };
        let n = g.number;
        let coupled = c.coupled(n);
        let request = json!({"cmd": "set_coupling", "pair": n, "coupled": !coupled});
        if c.multi() {
            if skip {
                self.mutate(request);
                return;
            }
            let detail = if coupled {
                trf("“OpenLW In {n}” becomes a mono device.", &[("n", &n)])
            } else {
                trf("“OpenLW In {n}” becomes a stereo device.", &[("n", &n)])
            };
            let body = trf(
                "{detail} Audio on all OpenLW devices stops for a moment while PipeWire publishes them again.",
                &[("detail", &detail)],
            );
            let (heading, accept) = if coupled {
                (tr("Uncouple This Device?"), tr("Uncouple"))
            } else {
                (tr("Couple This Device?"), tr("Couple"))
            };
            self.confirm(heading, &body, accept, false, true, move |w| {
                w.mutate(request)
            });
            return;
        }
        // Coupling releases what does not fit a stereo patch of the first input's source.
        if !coupled && coupling_releases(&c, &g) {
            let heading = trf(
                "Couple Inputs {a}-{b}?",
                &[("a", &(g.columns.start + 1)), ("b", &g.columns.end)],
            );
            self.confirm(
                &heading,
                tr("The source of the first input becomes stereo on the pair; the other patches on these inputs are released."),
                tr("Couple"),
                true,
                false,
                move |w| w.mutate(request),
            );
            return;
        }
        self.mutate(request);
    }

    /// Multi layout: sends an input patch to device `device` (`width` `None`: release),
    /// warning first if it changes a device's width (the daemon then recreates the shared
    /// region).
    fn send(self: &Rc<Self>, request: Value, device: Option<u32>, width: Option<usize>) {
        let (c, skip) = (
            self.st.borrow().config.clone(),
            self.settings.borrow().skip_width_warning,
        );
        let Some(n) = device.filter(|_| c.multi() && !skip) else {
            self.mutate(request);
            return;
        };
        let Some((device, from, to)) = width_change(&c, n, width) else {
            self.mutate(request);
            return;
        };
        let detail = if to == 1 {
            trf(
                "“OpenLW In {n}” changes from {from} channels to 1 channel.",
                &[("n", &device), ("from", &from)],
            )
        } else {
            trf(
                "“OpenLW In {n}” changes from {from} to {to} channels.",
                &[("n", &device), ("from", &from), ("to", &to)],
            )
        };
        let body = trf(
            "{detail} Audio on all OpenLW devices stops for a moment while PipeWire publishes them again.",
            &[("detail", &detail)],
        );
        let heading = if width.is_none() {
            tr("Release This Input?")
        } else {
            tr("Patch This Source?")
        };
        self.confirm(heading, &body, tr("Continue"), false, true, move |w| {
            w.mutate(request)
        });
    }

    /// Removes a manual or unadvertised row; releases its inputs if patched.
    fn remove_row(self: &Rc<Self>, row: usize) {
        let Some(s) = self
            .st
            .borrow()
            .grid_rows
            .get(row)
            .map(|r| r.source.clone())
        else {
            return;
        };
        let previewed = self
            .st
            .borrow()
            .listening
            .as_ref()
            .is_some_and(|(ch, k)| *ch == s.channel && k == s.patch_kind());
        if previewed {
            self.stop_listening();
        }
        {
            let mut set = self.settings.borrow_mut();
            set.manual
                .retain(|m| !(m.channel == s.channel && patch_kind(&m.kind) == s.patch_kind()));
            set.save();
        }
        // A received stream stays in the configuration, even unpatched (displaced by another
        // patch): without remove_input the row would come back as "not advertised".
        let received = self
            .st
            .borrow()
            .config
            .inputs
            .iter()
            .any(|i| i.channel == Some(s.channel) && i.kind == s.patch_kind());
        if received {
            self.mutate(json!({"cmd": "remove_input", "channel": s.channel,
                "kind": s.patch_kind()}));
        } else {
            self.update_grid();
        }
    }

    fn toggle_listen(&self, row: usize) {
        let Some(s) = self
            .st
            .borrow()
            .grid_rows
            .get(row)
            .map(|r| r.source.clone())
        else {
            return;
        };
        let (same, ipv4) = {
            let st = self.st.borrow();
            let same = st
                .listening
                .as_ref()
                .is_some_and(|(ch, k)| *ch == s.channel && k == s.patch_kind());
            let ipv4 = (!st.link.searching)
                .then(|| st.link.ipv4.parse::<Ipv4Addr>().ok())
                .flatten();
            (same, ipv4)
        };
        if same {
            self.stop_listening();
            return;
        }
        let Some(ipv4) = ipv4 else {
            self.show_error(Some(
                DaemonError::Refused(
                    tr("the computer is not connected to the Livewire network yet. Choose the interface, then try again.")
                        .into(),
                )
                .message(),
            ));
            return;
        };
        let Some(group) = s.group() else {
            return;
        };
        let channels = if s.patch_kind() == "surround" { 8 } else { 2 };
        let bits = if s.kind == "stereo-l16" { 16 } else { 24 };
        let started = self
            .listener
            .borrow_mut()
            .start(group, ipv4, channels, bits);
        match started {
            Ok(()) => {
                self.st.borrow_mut().listening = Some((s.channel, s.patch_kind().into()));
                self.show_error(None);
            }
            Err(e) => {
                self.st.borrow_mut().listening = None;
                self.show_error(Some(e));
            }
        }
        self.update_grid();
    }

    fn stop_listening(&self) {
        self.listener.borrow_mut().stop();
        self.st.borrow_mut().listening = None;
        self.update_grid();
    }

    fn apply_output(self: &Rc<Self>, row: &OutputRow) {
        let previous = output_patch(&self.st.borrow().config, row).cloned();
        if !row.emit.is_active() {
            if let Some(p) = previous {
                self.mutate(json!({"cmd": "unpatch_output", "channel": p.channel}));
            }
            return;
        }
        let Some(ch) = row.channel() else {
            self.show_error(Some(invalid_channel()));
            // Nothing transmitted: the switch shows the configured state, even while editing.
            row.set_emit(previous.is_some());
            row.show(previous.as_ref(), self);
            return;
        };
        let mut patch = json!({"cmd": "patch_output", "channel": ch, "name": row.stream_name(),
            "format": row.format(), "device_channels": row.pair});
        if let (Some(d), Some(o)) = (row.device, patch.as_object_mut()) {
            o.insert("device".into(), d.into());
        }
        match previous {
            // Channel change: stop this row's previous stream first.
            Some(p) if p.channel != ch => {
                self.request(
                    json!({"cmd": "unpatch_output", "channel": p.channel}),
                    move |w, r| match r {
                        Ok(_) => w.mutate(patch),
                        Err(e) => w.show_error(Some(e.message())),
                    },
                );
            }
            _ => self.mutate(patch),
        }
    }

    fn commit_terminal_name(self: &Rc<Self>) {
        let name = self.terminal_row.text().trim().to_string();
        let changed = {
            let st = self.st.borrow();
            st.config_loaded && name != st.config.terminal_name
        };
        if changed {
            self.mutate(json!({"cmd": "set_advanced", "terminal_name": name}));
        }
    }

    fn latency_changed(self: &Rc<Self>) {
        if self.updating.get() {
            return;
        }
        let Some((value, _)) = LATENCIES.get(self.latency_row.selected() as usize) else {
            return;
        };
        if *value != self.st.borrow().config.latency {
            self.mutate(json!({"cmd": "set_advanced", "latency": value}));
        }
    }

    fn dscp_changed(self: &Rc<Self>) {
        if self.updating.get() {
            return;
        }
        let Some((dscp, _)) = DSCPS.get(self.dscp_row.selected() as usize) else {
            return;
        };
        if dscp << 2 != self.st.borrow().config.tos {
            self.mutate(json!({"cmd": "set_advanced", "dscp": dscp}));
        }
    }

    /// Enables and starts the user service (packages install it without enabling it for
    /// existing sessions).
    fn start_service(self: &Rc<Self>) {
        let proc = gio::Subprocess::newv(
            &[
                "systemctl".as_ref(),
                "--user".as_ref(),
                "enable".as_ref(),
                "--now".as_ref(),
                "openlw.service".as_ref(),
            ],
            gio::SubprocessFlags::STDERR_PIPE,
        );
        let proc = match proc {
            Ok(p) => p,
            Err(e) => {
                self.show_error(Some(trf(
                    "Could not start the service: {error}.",
                    &[("error", &e)],
                )));
                return;
            }
        };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = proc.communicate_utf8_future(None).await;
            let Some(w) = weak.upgrade() else { return };
            let ok = proc.is_successful();
            match result {
                Ok(_) if ok => {
                    w.show_error(None);
                    w.refresh_slow();
                }
                Ok((_, err)) => {
                    let detail = err.as_deref().map(str::trim).unwrap_or_default();
                    w.show_error(Some(if detail.is_empty() {
                        tr("Could not start the service. Check the log: journalctl --user -u openlw.").into()
                    } else {
                        trf(
                            "Could not start the service: {error}. Check the log: journalctl --user -u openlw.",
                            &[("error", &detail)],
                        )
                    }));
                }
                Err(e) => w.show_error(Some(trf(
                    "Could not start the service: {error}.",
                    &[("error", &e)],
                ))),
            }
        });
    }

    fn show_about(&self) {
        let about = adw::AboutWindow::builder()
            .transient_for(&self.win)
            .application_name("OpenLW")
            .application_icon(crate::APP_ID)
            .developer_name("François Brille")
            .version(env!("CARGO_PKG_VERSION"))
            .license_type(gtk::License::Apache20)
            .comments(tr("Livewire® and AES67 audio on the computer, through PipeWire.\n\nExperimental project, provided “as is”, without any warranty. Not suitable for critical environments (on-air chains, safety systems).\n\nLivewire is a trademark of TLS Corp."))
            .build();
        about.present();
    }
}

/// Multi layout: first input device whose width changes (device, from, to) when device `n`
/// gets `width` channels of a source (`None`: released), as the daemon computes it: the
/// device's other crosspoints go, a surround patch makes it 8 wide.
fn width_change(c: &DaemonConfig, n: u32, width: Option<usize>) -> Option<(u32, u32, u32)> {
    let devices = c.in_devices();
    let before = DaemonConfig::in_widths(&c.inputs, &c.uncoupled, devices);
    let mut after: Vec<InputPatch> = c
        .inputs
        .iter()
        .map(|i| InputPatch {
            taps: i
                .taps
                .iter()
                .filter(|t| t.device != Some(n))
                .cloned()
                .collect(),
            ..i.clone()
        })
        .collect();
    if width == Some(8) {
        after.push(InputPatch {
            channel: None,
            kind: "surround".into(),
            taps: straight_taps(Some(n), 1, 8),
        });
    }
    let widths = DaemonConfig::in_widths(&after, &c.uncoupled, devices);
    (1..)
        .zip(before.iter().zip(&widths))
        .find(|(_, (a, b))| a != b)
        .map(|(d, (&a, &b))| (d, a, b))
}

/// Taps of a source patched “as is”: stream channel i on input `first + i`.
fn straight_taps(device: Option<u32>, first: u32, channels: u32) -> Vec<Tap> {
    (0..channels)
        .map(|i| Tap {
            device,
            channel: first + i,
            from: vec![i + 1],
        })
        .collect()
}

/// Crosspoints of a click on group `g`: stereo (surround: 8 channels from the group start);
/// an uncoupled device takes L+R. Error message when the source does not fit.
fn group_taps(c: &DaemonConfig, g: &GridHeader, surround: bool) -> Result<Vec<Tap>, String> {
    let n = g.number;
    if c.multi() {
        if !c.coupled(n) {
            if surround {
                return Err(trf(
                    "a surround source needs a coupled device. Couple “OpenLW In {n}” first.",
                    &[("n", &n)],
                ));
            }
            return Ok(vec![Tap {
                device: Some(n),
                channel: 1,
                from: vec![1, 2],
            }]);
        }
        return Ok(straight_taps(Some(n), 1, if surround { 8 } else { 2 }));
    }
    let from_net = c.channels_from_net;
    let first = g.columns.start as u32 + 1;
    let width = if surround {
        8
    } else {
        (g.columns.len() as u32).clamp(1, 2)
    };
    if first + width - 1 > from_net {
        return Err(trf(
            "a surround source takes 8 inputs. Choose a pair from 1-2 to {a}-{b}.",
            &[
                ("a", &from_net.saturating_sub(7)),
                ("b", &from_net.saturating_sub(6)),
            ],
        ));
    }
    Ok(straight_taps(None, first, width))
}

/// Stream channels of a tap after a click on side `side` (1 left, 2 right): toggled.
fn toggled_side(current: &[u32], side: u32) -> Vec<u32> {
    let mut from: Vec<u32> = current.iter().copied().filter(|&k| k != side).collect();
    if from.len() == current.len() {
        from.push(side);
        from.sort_unstable();
    }
    from
}

/// Duplex layout: coupling pair `g` releases patches (several sources feed its inputs).
fn coupling_releases(c: &DaemonConfig, g: &GridHeader) -> bool {
    let inputs = g.columns.start as u32 + 1..=g.columns.end as u32;
    c.inputs
        .iter()
        .filter(|i| {
            i.taps
                .iter()
                .any(|t| t.device.is_none() && inputs.contains(&t.channel))
        })
        .count()
        > 1
}

/// Received sources left without crosspoints with `from_net` input channels (multi layout:
/// `from_net / 2` devices).
fn lost_inputs(c: &DaemonConfig, from_net: u32) -> usize {
    let multi = c.multi();
    c.inputs
        .iter()
        .filter(|i| {
            !i.taps.is_empty()
                && i.taps.iter().all(|t| {
                    if multi {
                        t.device.unwrap_or(0) > from_net.div_ceil(2)
                    } else {
                        t.channel > from_net
                    }
                })
        })
        .count()
}

/// Configured received stream of a source, if any.
fn input_patch<'a>(c: &'a DaemonConfig, channel: u16, kind: &str) -> Option<&'a InputPatch> {
    c.inputs
        .iter()
        .find(|i| i.channel == Some(channel) && i.kind == kind)
}

/// Tag of a stereo tap on a coupled group: L, R, L+R, or the stream channels.
fn side_tag(from: &[u32]) -> String {
    match from {
        [1] => tr("L").into(),
        [2] => tr("R").into(),
        [1, 2] => tr("L+R").into(),
        _ => from
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join("+"),
    }
}

/// Crosspoints of a source as drawn: coupled groups (header → tag, empty for plain stereo;
/// every group of a surround source) and uncoupled columns (column → sides).
fn grid_cells(
    c: &DaemonConfig,
    layout: &GridLayout,
    channel: u16,
    kind: &str,
) -> (BTreeMap<usize, String>, BTreeMap<usize, GridSides>) {
    let (mut groups, mut sides) = (BTreeMap::new(), BTreeMap::new());
    let Some(p) = input_patch(c, channel, kind) else {
        return (groups, sides);
    };
    let multi = c.multi();
    let stereo = p.kind != "surround";
    for (h, g) in layout.headers.iter().enumerate() {
        // Taps of this group (multi: the device's).
        let taps: Vec<&Tap> = p
            .taps
            .iter()
            .filter(|t| {
                if multi {
                    t.device == Some(g.number)
                } else {
                    t.device.is_none()
                        && (t.channel as usize)
                            .checked_sub(1)
                            .is_some_and(|i| g.columns.contains(&i))
                }
            })
            .collect();
        if taps.is_empty() {
            continue;
        }
        if g.coupled || !stereo {
            let first = if multi { 1 } else { g.columns.start as u32 + 1 };
            let plain = stereo
                && taps.len() == 2
                && taps.iter().all(|t| {
                    t.from.len() == 1
                        && t.channel.checked_sub(first).map(|d| d + 1) == t.from.first().copied()
                });
            let tag = if plain {
                String::new()
            } else if stereo {
                taps.iter()
                    .map(|t| side_tag(&t.from))
                    .collect::<Vec<_>>()
                    .join("·")
            } else {
                let firsts = taps.iter().filter_map(|t| t.from.first().copied());
                let (lo, hi) = (firsts.clone().min(), firsts.max());
                match (lo, hi) {
                    (Some(lo), Some(hi)) if lo != hi => format!("{lo}-{hi}"),
                    (Some(lo), _) => lo.to_string(),
                    _ => String::new(),
                }
            };
            groups.insert(h, tag);
        } else {
            for t in taps {
                let col = if multi {
                    g.columns.start
                } else {
                    t.channel as usize - 1
                };
                sides.insert(
                    col,
                    GridSides {
                        left: t.from.contains(&1),
                        right: t.from.contains(&2),
                    },
                );
            }
        }
    }
    (groups, sides)
}

/// Grid columns and header groups: device channels in pairs (duplex), or devices (multi).
fn grid_layout(c: &DaemonConfig) -> GridLayout {
    if c.multi() {
        let n = c.in_devices().max(1);
        return GridLayout {
            multi: true,
            columns: n as usize,
            headers: (1..=n)
                .map(|d| GridHeader {
                    columns: d as usize - 1..d as usize,
                    title: trf("In {n}", &[("n", &d)]),
                    number: d,
                    coupled: c.coupled(d),
                })
                .collect(),
        };
    }
    let ch = c.channels_from_net as usize;
    GridLayout {
        multi: false,
        columns: ch,
        headers: (1..=ch)
            .step_by(2)
            .map(|a| {
                let b = (a + 1).min(ch);
                let number = a.div_ceil(2) as u32;
                GridHeader {
                    columns: a - 1..b,
                    title: if a == b {
                        trf("Input {n}", &[("n", &a)])
                    } else {
                        trf("Inputs {a}-{b}", &[("a", &a), ("b", &b)])
                    },
                    number,
                    coupled: c.coupled(number),
                }
            })
            .collect(),
    }
}

/// Configured transmitted stream of an output row.
fn output_patch<'a>(c: &'a DaemonConfig, row: &OutputRow) -> Option<&'a OutputPatch> {
    c.outputs.iter().find(|o| match row.device {
        Some(d) => o.device == Some(d),
        None => o.device.is_none() && o.device_channels.as_deref() == Some(&row.pair[..]),
    })
}

fn invalid_channel() -> String {
    DaemonError::Refused(tr("invalid channel. Enter a number from 1 to 32766.").into()).message()
}

/// Output row: device pair (duplex layout) or output device (multi layout), meter, channel,
/// advertised name, format, transmission.
struct OutputRow {
    /// Device channels sent in the patch: the pair, or 1-2 of `device`.
    pair: Vec<u32>,
    /// Multi layout: output device number.
    device: Option<u32>,
    row: adw::PreferencesRow,
    meter: Meter,
    channel: gtk::Entry,
    name: gtk::Entry,
    format: gtk::DropDown,
    emit: gtk::Switch,
    showing: Cell<bool>,
}

impl OutputRow {
    fn new(pair: Vec<u32>, device: Option<u32>, weak: &Weak<Window>) -> Rc<Self> {
        let title = match device {
            Some(n) => trf("Out {n}", &[("n", &n)]),
            None => trf(
                "Outputs {a}-{b}",
                &[
                    ("a", &pair.first().copied().unwrap_or(0)),
                    ("b", &pair.last().copied().unwrap_or(0)),
                ],
            ),
        };
        // Explicit layout rather than an action row: the suffixes would squeeze the title.
        let row = adw::PreferencesRow::builder()
            .title(&title)
            .activatable(false)
            .build();
        let line = gtk::Box::builder()
            .spacing(10)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(12)
            .margin_end(12)
            .build();
        let label = gtk::Label::builder()
            .label(&title)
            .xalign(0.0)
            .width_chars(10)
            .build();
        let meter = Meter::new(2, 110);
        let channel = gtk::Entry::builder()
            .placeholder_text(tr("channel"))
            .width_chars(7)
            .max_width_chars(7)
            .input_purpose(gtk::InputPurpose::Digits)
            .valign(gtk::Align::Center)
            .build();
        channel.add_css_class("monospace");
        let name = gtk::Entry::builder()
            .placeholder_text(tr("advertised name"))
            .width_chars(12)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();
        let format = gtk::DropDown::from_strings(&FORMATS.map(|(_, t)| tr(t)));
        format.set_valign(gtk::Align::Center);
        let emit = gtk::Switch::builder()
            .valign(gtk::Align::Center)
            .tooltip_text(tr("Transmit"))
            .build();
        let emit_label = gtk::Label::new(Some(tr("Transmit")));
        line.append(&label);
        line.append(meter.widget());
        line.append(&channel);
        line.append(&name);
        line.append(&format);
        line.append(&emit_label);
        line.append(&emit);
        row.set_child(Some(&line));
        let this = Rc::new(Self {
            pair,
            device,
            row,
            meter,
            channel,
            name,
            format,
            emit,
            showing: Cell::new(false),
        });
        let apply = |this: &Rc<Self>, weak: &Weak<Window>| {
            let me = Rc::downgrade(this);
            let weak = weak.clone();
            move || {
                if let (Some(me), Some(w)) = (me.upgrade(), weak.upgrade()) {
                    if !me.showing.get() {
                        w.apply_output(&me);
                    }
                }
            }
        };
        for entry in [&this.channel, &this.name] {
            let f = apply(&this, weak);
            let me = Rc::downgrade(&this);
            entry.connect_activate(move |_| {
                if me.upgrade().is_some_and(|m| m.emit.is_active()) {
                    f();
                }
            });
        }
        let f = apply(&this, weak);
        let me = Rc::downgrade(&this);
        this.format.connect_selected_notify(move |_| {
            if me.upgrade().is_some_and(|m| m.emit.is_active()) {
                f();
            }
        });
        let f = apply(&this, weak);
        this.emit.connect_active_notify(move |_| f());
        this
    }

    fn channel(&self) -> Option<u16> {
        self.channel
            .text()
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|c| (1..=32766).contains(c))
    }

    /// Advertised name; default “PC 1-2” or “PC n” (network name, not translated).
    fn stream_name(&self) -> String {
        let n = self.name.text().trim().to_string();
        if !n.is_empty() {
            return n;
        }
        match self.device {
            Some(d) => format!("PC {d}"),
            None => format!(
                "PC {}-{}",
                self.pair.first().copied().unwrap_or(0),
                self.pair.last().copied().unwrap_or(0)
            ),
        }
    }

    fn format(&self) -> &'static str {
        FORMATS
            .get(self.format.selected() as usize)
            .map_or("standard", |(v, _)| v)
    }

    fn set_emit(&self, on: bool) {
        self.showing.set(true);
        self.emit.set_active(on);
        self.showing.set(false);
    }

    /// Shows the configured state, except while the user is editing a field.
    fn show(&self, patch: Option<&OutputPatch>, w: &Window) {
        if w.editing(&self.channel) || w.editing(&self.name) {
            return;
        }
        self.showing.set(true);
        self.emit.set_active(patch.is_some());
        if let Some(p) = patch {
            let ch = p.channel.to_string();
            if self.channel.text() != ch {
                self.channel.set_text(&ch);
            }
            if self.name.text() != p.name {
                self.name.set_text(&p.name);
            }
            if let Some(i) = FORMATS.iter().position(|(v, _)| *v == p.format) {
                self.format.set_selected(i as u32);
            }
        }
        self.showing.set(false);
    }
}

fn group(title: &str, description: &str) -> adw::PreferencesGroup {
    adw::PreferencesGroup::builder()
        .title(title)
        .description(description)
        .build()
}

/// Count menu entries: “n channels (2n inputs)”, or “n devices” in multi layout.
fn count_titles(multi: bool, inputs: bool) -> Vec<String> {
    (1..=MAX_PAIRS)
        .map(|n| match (multi, inputs, n) {
            (true, _, 1) => tr("1 device").to_string(),
            (true, _, _) => trf("{n} devices", &[("n", &n)]),
            (false, true, 1) => tr("1 channel (2 inputs)").to_string(),
            (false, true, _) => trf("{n} channels ({m} inputs)", &[("n", &n), ("m", &(2 * n))]),
            (false, false, 1) => tr("1 channel (2 outputs)").to_string(),
            (false, false, _) => trf("{n} channels ({m} outputs)", &[("n", &n), ("m", &(2 * n))]),
        })
        .collect()
}

fn set_dot(dot: &gtk::Label, class: &str) {
    for c in ["dim-label", "success", "warning", "error"] {
        dot.remove_css_class(c);
    }
    dot.add_css_class(class);
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tap(device: Option<u32>, channel: u32, from: &[u32]) -> Tap {
        Tap {
            device,
            channel,
            from: from.to_vec(),
        }
    }

    #[test]
    fn grid_geometry_and_crosspoints() {
        let duplex = DaemonConfig::from(&json!({
            "device": {"channels_from_net": 6},
            "uncoupled_inputs": [3],
            "destinations": [
                {"channel": 1, "taps": [{"channel": 3, "from": [1]}, {"channel": 4, "from": [2]}]},
                {"channel": 2, "taps": [{"channel": 1, "from": [2]}, {"channel": 2, "from": [1]},
                                        {"channel": 5, "from": [1]}, {"channel": 6, "from": [1, 2]}]},
                {"channel": 3, "kind": "surround", "taps": []}],
        }));
        let l = grid_layout(&duplex);
        assert_eq!((l.multi, l.columns, l.headers.len()), (false, 6, 3));
        assert_eq!(l.headers[1].columns, 2..4);
        assert_eq!(
            l.headers
                .iter()
                .map(|h| (h.number, h.coupled))
                .collect::<Vec<_>>(),
            vec![(1, true), (2, true), (3, false)]
        );
        // Plain stereo on pair 3-4.
        let (groups, sides) = grid_cells(&duplex, &l, 1, "stereo");
        assert_eq!(groups, BTreeMap::from([(1, String::new())]));
        assert!(sides.is_empty());
        // Swapped on coupled pair 1-2; L on input 5 and L+R on input 6 (uncoupled pair).
        let (groups, sides) = grid_cells(&duplex, &l, 2, "stereo");
        assert_eq!(groups, BTreeMap::from([(0, "R·L".to_string())]));
        let side = |left, right| GridSides { left, right };
        assert_eq!(
            sides,
            BTreeMap::from([(4, side(true, false)), (5, side(true, true))])
        );
        // Received, unpatched.
        let (groups, sides) = grid_cells(&duplex, &l, 3, "surround");
        assert!(groups.is_empty() && sides.is_empty());

        let mut wide = DaemonConfig::from(&json!({
            "device": {"channels_from_net": 10},
            "uncoupled_inputs": [2],
            "destinations": [{"channel": 9, "kind": "surround", "taps": (1..=8)
                .map(|k| json!({"channel": k + 2, "from": [k]})).collect::<Vec<_>>()}],
        }));
        let l = grid_layout(&wide);
        // Surround: one target per group, even uncoupled, tagged with its stream channels.
        let (groups, sides) = grid_cells(&wide, &l, 9, "surround");
        assert_eq!(
            groups.into_iter().collect::<Vec<_>>(),
            vec![
                (1, "1-2".to_string()),
                (2, "3-4".to_string()),
                (3, "5-6".to_string()),
                (4, "7-8".to_string())
            ]
        );
        assert!(sides.is_empty());
        // Surround from pair 7-8 does not fit in 10 inputs; stereo on a pair does.
        assert!(group_taps(&wide, &l.headers[3], true).is_err());
        assert_eq!(
            group_taps(&wide, &l.headers[1], true).unwrap(),
            straight_taps(None, 3, 8)
        );
        assert_eq!(
            group_taps(&wide, &l.headers[4], false).unwrap(),
            vec![tap(None, 9, &[1]), tap(None, 10, &[2])]
        );
        // Coupling pair 3-4 releases patches only when several sources feed it.
        assert!(!coupling_releases(&wide, &l.headers[1]));
        wide.inputs.push(InputPatch {
            channel: Some(7),
            kind: "stereo".into(),
            taps: vec![tap(None, 4, &[1, 2])],
        });
        assert!(coupling_releases(&wide, &l.headers[1]));
        assert!(!coupling_releases(&wide, &l.headers[0]));

        let multi = DaemonConfig::from(&json!({
            "device_layout": "multi",
            "device": {"channels_from_net": 6},
            "uncoupled_inputs": [3],
            "destinations": [
                {"channel": 5, "kind": "surround", "taps": (1..=8)
                    .map(|k| json!({"device": 2, "channel": k, "from": [k]})).collect::<Vec<_>>()},
                {"channel": 6, "taps": [{"device": 1, "channel": 1, "from": [1]},
                                        {"device": 1, "channel": 2, "from": [2]},
                                        {"device": 3, "channel": 1, "from": [2]}]}],
        }));
        let l = grid_layout(&multi);
        assert_eq!((l.multi, l.columns), (true, 3));
        assert_eq!(l.headers[1].title, "In 2");
        assert!(l.headers[0].coupled && !l.headers[2].coupled);
        let (groups, _) = grid_cells(&multi, &l, 5, "surround");
        assert_eq!(groups, BTreeMap::from([(1, "1-8".to_string())]));
        let (groups, sides) = grid_cells(&multi, &l, 6, "stereo");
        assert_eq!(groups, BTreeMap::from([(0, String::new())]));
        assert_eq!(sides, BTreeMap::from([(2, side(false, true))]));
        // Clicks: stereo on a coupled device, L+R on an uncoupled one, no surround there.
        assert_eq!(
            group_taps(&multi, &l.headers[0], false).unwrap(),
            straight_taps(Some(1), 1, 2)
        );
        assert_eq!(
            group_taps(&multi, &l.headers[2], false).unwrap(),
            vec![tap(Some(3), 1, &[1, 2])]
        );
        assert!(group_taps(&multi, &l.headers[2], true).is_err());
        assert_eq!(group_taps(&multi, &l.headers[1], true).unwrap().len(), 8);
    }

    #[test]
    fn side_toggles() {
        assert_eq!(toggled_side(&[], 1), vec![1]);
        assert_eq!(toggled_side(&[2], 1), vec![1, 2]);
        assert_eq!(toggled_side(&[1, 2], 1), vec![2]);
        assert_eq!(toggled_side(&[2], 2), Vec::<u32>::new());
    }

    #[test]
    fn width_warning() {
        let c = DaemonConfig::from(&json!({
            "device_layout": "multi",
            "device": {"channels_from_net": 6},
            "uncoupled_inputs": [2],
            "destinations": [
                {"channel": 101, "taps": [{"device": 2, "channel": 1, "from": [1]}]},
                {"channel": 2204, "taps": [{"device": 1, "channel": 1, "from": [1]},
                                           {"device": 1, "channel": 2, "from": [2]}]},
                {"channel": 300, "kind": "surround", "taps": (1..=8)
                    .map(|k| json!({"device": 3, "channel": k, "from": [k]})).collect::<Vec<_>>()}],
        }));
        assert_eq!(
            DaemonConfig::in_widths(&c.inputs, &c.uncoupled, 3),
            vec![2, 1, 8]
        );
        // Stereo on a stereo device, or anything on an uncoupled device: geometry kept.
        assert_eq!(width_change(&c, 1, Some(2)), None);
        assert_eq!(width_change(&c, 2, Some(1)), None);
        assert_eq!(width_change(&c, 2, None), None);
        // Surround on In 1: 2 to 8 channels; stereo replacing the surround on In 3: 8 to 2.
        assert_eq!(width_change(&c, 1, Some(8)), Some((1, 2, 8)));
        assert_eq!(width_change(&c, 3, Some(2)), Some((3, 8, 2)));
        assert_eq!(width_change(&c, 3, None), Some((3, 8, 2)));
        // Fewer devices: sources left without crosspoints.
        assert_eq!(lost_inputs(&c, 4), 1);
        assert_eq!(lost_inputs(&c, 2), 2);
        let duplex = DaemonConfig::from(&json!({
            "device": {"channels_from_net": 8},
            "destinations": [
                {"channel": 1, "taps": [{"channel": 1, "from": [1]}, {"channel": 7, "from": [2]}]},
                {"channel": 2, "taps": [{"channel": 5, "from": [1]}]},
                {"channel": 3, "taps": []}],
        }));
        assert_eq!(lost_inputs(&duplex, 4), 1);
    }

    #[test]
    fn tables_are_translated() {
        let fr = crate::i18n::french_catalog();
        let texts = LAYOUTS
            .iter()
            .chain(&LATENCIES)
            .chain(&FORMATS)
            .chain(&MANUAL_KINDS)
            .map(|(_, t)| *t)
            .chain(DSCPS.iter().map(|(_, t)| *t));
        for t in texts {
            assert!(fr.contains_key(t), "missing in fr.po: {t:?}");
        }
    }

    #[test]
    fn count_menu() {
        assert_eq!(count_titles(false, true)[0], "1 channel (2 inputs)");
        assert_eq!(count_titles(false, false)[2], "3 channels (6 outputs)");
        assert_eq!(count_titles(true, true)[15], "16 devices");
    }
}
