//! Main window, ported from macos/app/Sources/MainWindowController.swift and
//! windows/app/MainWindow.xaml.cs: Livewire network, audio device, input patch matrix,
//! transmitted outputs, advanced settings. The network service itself is never shown: the app
//! talks about network, channels and device. Polls status at 5 Hz; configuration, sources and
//! interfaces every 2 s.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};
use serde_json::{json, Value};

use crate::client::{Client, DaemonError};
use crate::grid::{pair_label, GridActions, GridRow, InputGrid};
use crate::listener::Listener;
use crate::meter::Meter;
use crate::models::{
    patch_kind, peak, DaemonConfig, DeviceMeters, DiscoveredSource, Iface, LinkStatus, OutputPatch,
};
use crate::settings::{ManualSource, Settings};

const MAX_PAIRS: u32 = 16;
const LATENCIES: [(&str, &str); 3] = [
    ("low", "Faible : ≈ 8 ms ajoutées, réseau dédié"),
    ("normal", "Normale : ≈ 17 ms ajoutées"),
    (
        "safe",
        "Sûre : ≈ 35 ms ajoutées, réseau partagé ou ordinateur chargé",
    ),
];
const DSCPS: [(u32, &str); 3] = [
    (46, "EF (46) : défaut Livewire"),
    (34, "AF41 (34) : recommandé pour AES67"),
    (0, "Aucune (0)"),
];
const FORMATS: [(&str, &str); 3] = [
    ("standard", "Standard (5 ms)"),
    ("aes67", "AES67 (1 ms)"),
    ("livestream", "Livestream (0,25 ms)"),
];
const MANUAL_KINDS: [(&str, &str); 3] = [
    ("stereo", "Stéréo"),
    ("backfeed", "Retour (To Source)"),
    ("surround", "Surround 8 canaux"),
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
    pairs: Vec<Vec<u32>>,
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
    nodes_row: adw::ActionRow,
    nodes_icon: gtk::Image,
    in_count: adw::ComboRow,
    out_count: adw::ComboRow,
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
            "Réseau Livewire",
            "Horloge : celle de l'ordinateur. Les écarts avec les autres appareils sont compensés automatiquement.",
        );
        let state_dot = gtk::Label::new(Some("●"));
        state_dot.add_css_class("dim-label");
        let state_row = adw::ActionRow::builder()
            .title("Connexion au service OpenLW…")
            .build();
        state_row.add_prefix(&state_dot);
        let iface_model = gtk::StringList::new(&[]);
        let iface_row = adw::ComboRow::builder()
            .title("Interface")
            .model(&iface_model)
            .build();
        let advertise_row = adw::SwitchRow::builder()
            .title("Annoncer les sorties sur le réseau")
            .subtitle("Les autres appareils Livewire voient les canaux diffusés et leur nom.")
            .build();
        network.add(&state_row);
        network.add(&iface_row);
        network.add(&advertise_row);

        // ---------- Audio device (PipeWire nodes) ----------
        let device = group(
            "Périphérique audio",
            "Les applications PipeWire, PulseAudio et JACK voient les mêmes périphériques.",
        );
        let nodes_icon = gtk::Image::from_icon_name("content-loading-symbolic");
        let nodes_row = adw::ActionRow::builder()
            .title("Périphériques audio")
            .build();
        nodes_row.add_prefix(&nodes_icon);
        device.add(&nodes_row);
        for (title, subtitle) in [
            (
                "OpenLW Out",
                "Choisissez-le comme sortie : ce que les applications y jouent est diffusé sur le réseau, selon les sorties réglées plus bas.",
            ),
            (
                "OpenLW In",
                "Choisissez-le comme entrée : il reçoit l'audio du réseau, selon la grille des entrées.",
            ),
        ] {
            device.add(&adw::ActionRow::builder().title(title).subtitle(subtitle).build());
        }

        // ---------- Inputs ----------
        let inputs = group(
            "Entrées (réseau vers ordinateur)",
            "Cliquez une case pour envoyer la source sur cette paire d'entrées d'OpenLW In. Cliquez à nouveau pour la libérer. Le bouton casque fait écouter la source sur la sortie audio de l'ordinateur, sans la patcher.",
        );
        let in_count = count_row("Canaux Livewire reçus", "entrées");
        inputs.add(&in_count);
        let grid = InputGrid::new(GridActions {
            toggle: Box::new(on!(weak, |w, r, c| w.toggle_input(r, c))),
            listen: Box::new(on!(weak, |w, r| w.toggle_listen(r))),
            remove: Box::new(on!(weak, |w, r| w.remove_row(r))),
        });
        let grid_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .child(grid.widget())
            .build();
        let grid_card = gtk::Box::new(gtk::Orientation::Vertical, 0);
        grid_card.add_css_class("card");
        grid_card.append(&grid_scroll);
        let manual = adw::PreferencesGroup::new();
        let manual_entry = adw::EntryRow::builder()
            .title("Source non annoncée : canal (1 à 32766)")
            .input_purpose(gtk::InputPurpose::Digits)
            .build();
        let manual_kind = gtk::DropDown::from_strings(&MANUAL_KINDS.map(|(_, t)| t));
        manual_kind.set_valign(gtk::Align::Center);
        let add = gtk::Button::with_label("Ajouter à la grille");
        add.set_valign(gtk::Align::Center);
        manual_entry.add_suffix(&manual_kind);
        manual_entry.add_suffix(&add);
        manual.add(&manual_entry);
        add.connect_clicked(on!(weak, |w, _| w.add_manual()));
        manual_entry.connect_entry_activated(on!(weak, |w, _| w.add_manual()));

        // ---------- Outputs ----------
        let outputs_group = group(
            "Sorties (ordinateur vers réseau)",
            "Chaque paire de canaux d'OpenLW Out est diffusée sur le canal Livewire de votre choix, sous le nom indiqué. Une modification s'applique quand la diffusion est activée.",
        );
        let out_count = count_row("Canaux Livewire diffusés", "sorties");
        outputs_group.add(&out_count);

        // ---------- Advanced settings ----------
        let advanced_group = adw::PreferencesGroup::new();
        let advanced = adw::ExpanderRow::builder()
            .title("Réglages avancés")
            .subtitle("Nom annoncé, latence de réception, priorité réseau")
            .expanded(settings.advanced_visible)
            .build();
        let terminal_row = adw::EntryRow::builder()
            .title("Nom annoncé (vide : nom de l'ordinateur)")
            .show_apply_button(true)
            .tooltip_text("Nom affiché par les autres appareils Livewire, 32 caractères au plus, accents remplacés.")
            .build();
        let latency_row = adw::ComboRow::builder()
            .title("Latence de réception")
            .subtitle("Ajoutée à celle du logiciel qui enregistre")
            .tooltip_text("Tampons ajoutés à celui du logiciel qui enregistre. Plus la latence est faible, plus un retard du réseau ou de l'ordinateur risque de provoquer une coupure brève.")
            .model(&gtk::StringList::new(&LATENCIES.map(|(_, t)| t)))
            .build();
        let dscp_row = adw::ComboRow::builder()
            .title("Priorité réseau (DSCP)")
            .subtitle("Selon la QoS des commutateurs")
            .tooltip_text("Marquage des flux audio émis. Choisissez la valeur prévue par la QoS de vos commutateurs.")
            .model(&gtk::StringList::new(&DSCPS.map(|(_, t)| t)))
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
        input_box.append(&inputs);
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
            .button_label("Démarrer le service")
            .build();
        let menu = gio::Menu::new();
        menu.append(Some("À propos d'OpenLW"), Some("app.about"));
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .tooltip_text("Menu principal")
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
            nodes_row,
            nodes_icon,
            in_count,
            out_count,
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
        self.iface_row
            .connect_selected_notify(on!(weak, |w, _| w.iface_changed()));
        self.advertise_row
            .connect_active_notify(on!(weak, |w, _| w.advertise_changed()));
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
        self.rebuild_output_rows(1);

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
            self.state_row.set_title("Service OpenLW injoignable");
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
                self.state_row.set_title("Recherche du réseau Livewire");
                self.state_row.set_subtitle(
                    "Branchez l'ordinateur sur le réseau Livewire, ou choisissez l'interface.",
                );
            } else {
                self.state_row
                    .set_title(&format!("Interface « {} » indisponible", st.config.iface));
                self.state_row
                    .set_subtitle("Branchez-la, ou choisissez Automatique.");
            }
            return;
        }
        set_dot(&self.state_dot, "success");
        let name = if link.friendly == link.iface {
            link.iface.clone()
        } else {
            format!("{} ({})", link.friendly, link.iface)
        };
        self.state_row
            .set_title(&format!("Connecté au réseau Livewire par {name}"));
        self.state_row.set_subtitle(&if link.auto {
            format!("{} · interface choisie automatiquement", link.ipv4)
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
                "Périphériques publiés dans PipeWire",
                "OpenLW Out et OpenLW In apparaissent dans les réglages du son et dans les logiciels audio.",
            ),
            (Some(true), Some(false)) => (
                "dialog-warning-symbolic",
                "PipeWire injoignable",
                "Les périphériques OpenLW apparaîtront dès que PipeWire répondra (nouvelle tentative toutes les 5 s).",
            ),
            (Some(true), None) => (
                "dialog-warning-symbolic",
                "Aucun périphérique audio",
                "Ce service OpenLW a été compilé sans PipeWire. Installez le paquet OpenLW de votre distribution.",
            ),
            _ => (
                "content-loading-symbolic",
                "Périphériques audio",
                "Disponibles quand le service OpenLW fonctionne.",
            ),
        };
        self.nodes_icon.set_icon_name(Some(icon));
        self.nodes_row.set_title(title);
        self.nodes_row.set_subtitle(subtitle);
    }

    // ---------- Display updates ----------

    fn apply_config(self: &Rc<Self>, c: DaemonConfig) {
        let (pairs_changed, editing_name) = {
            let st = self.st.borrow();
            (
                !st.config_loaded || c.channels_to_net != st.config.channels_to_net,
                self.editing(&self.terminal_row),
            )
        };
        self.updating.set(true);
        self.advertise_row.set_active(c.advertise);
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
        if pairs_changed {
            self.rebuild_output_rows((c.channels_to_net / 2).max(1));
        }
        for row in self.output_rows.borrow().iter() {
            row.show(
                c.outputs
                    .iter()
                    .find(|o| o.device_channels.as_deref() == Some(&row.pair[..])),
                self,
            );
        }
        {
            let mut st = self.st.borrow_mut();
            st.pairs = (0..c.channels_from_net / 2)
                .map(|i| vec![2 * i + 1, 2 * i + 2])
                .collect();
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
                "Automatique".to_string()
            } else if link.searching {
                "Automatique · recherche en cours".to_string()
            } else {
                format!("Automatique · {}", link.friendly)
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
                    format!("{} (indisponible)", config.iface),
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
        let (rows, pairs, listening) = {
            let st = self.st.borrow();
            let settings = self.settings.borrow();
            let patched = |channel: u16, kind: &str| {
                st.config
                    .inputs
                    .iter()
                    .find(|i| i.channel == Some(channel) && i.kind == kind)
                    .and_then(|i| i.device_channels.first())
                    .map(|first| (first.saturating_sub(1) / 2) as usize)
            };
            let mut rows: Vec<GridRow> = Vec::new();
            let mut seen = HashSet::new();
            let mut add = |s: DiscoveredSource, origin: &str, removable: bool| {
                if seen.insert((s.channel, s.patch_kind().to_string())) {
                    rows.push(GridRow {
                        patched_column: patched(s.channel, s.patch_kind()),
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
                add(DiscoveredSource::manual(m.channel, &m.kind), "saisi", true);
            }
            for p in &st.config.inputs {
                if let Some(ch) = p.channel {
                    add(DiscoveredSource::manual(ch, &p.kind), "non annoncé", true);
                }
            }
            let listening = st.listening.as_ref().and_then(|(ch, kind)| {
                rows.iter()
                    .position(|r| r.source.channel == *ch && r.source.patch_kind() == kind)
            });
            (rows, st.pairs.clone(), listening)
        };
        self.st.borrow_mut().grid_rows = rows.clone();
        self.grid.update(rows, pairs, listening);
        self.update_meters();
    }

    fn update_meters(&self) {
        let st = self.st.borrow();
        let columns: Vec<Vec<Option<f64>>> = st
            .pairs
            .iter()
            .map(|p| p.iter().map(|&c| peak(&st.meters.from_net, c)).collect())
            .collect();
        let status: Vec<&str> = st
            .pairs
            .iter()
            .map(|p| {
                let first = p.first().copied().unwrap_or(0);
                match st
                    .meters
                    .inputs
                    .iter()
                    .find(|(chs, _)| chs.contains(&first))
                {
                    None => "libre",
                    Some((_, true)) => "audio reçu",
                    Some((_, false)) => "en attente",
                }
            })
            .collect();
        let listen = if st.listening.is_some() {
            self.listener.borrow().take_peak()
        } else {
            None
        };
        self.grid.show_levels(&columns, &status, listen);
        for row in self.output_rows.borrow().iter() {
            let levels: Vec<Option<f64>> = row
                .pair
                .iter()
                .map(|&c| peak(&st.meters.to_net, c))
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

    fn rebuild_output_rows(self: &Rc<Self>, count: u32) {
        for row in self.output_rows.borrow_mut().drain(..) {
            self.outputs_group.remove(&row.row);
        }
        let weak = Rc::downgrade(self);
        for i in 0..count {
            let row = OutputRow::new(vec![2 * i + 1, 2 * i + 2], &weak);
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

    fn channel_count_changed(self: &Rc<Self>) {
        if self.updating.get() || !self.st.borrow().config_loaded {
            return;
        }
        let to_net = 2 * (self.out_count.selected() + 1);
        let from_net = 2 * (self.in_count.selected() + 1);
        let (lost_out, lost_in) = {
            let c = &self.st.borrow().config;
            if to_net == c.channels_to_net && from_net == c.channels_from_net {
                return;
            }
            let lost_out = c
                .outputs
                .iter()
                .filter(|o| {
                    o.device_channels
                        .as_ref()
                        .and_then(|d| d.iter().max())
                        .is_some_and(|&m| m > to_net)
                })
                .count();
            let lost_in = c
                .inputs
                .iter()
                .filter(|i| {
                    i.device_channels
                        .iter()
                        .max()
                        .is_some_and(|&m| m > from_net)
                })
                .count();
            (lost_out, lost_in)
        };
        let request = json!({"cmd": "set_device_channels", "to_net": to_net, "from_net": from_net});
        if lost_out + lost_in == 0 {
            self.mutate(request);
            return;
        }
        let mut lost = Vec::new();
        if lost_out > 0 {
            let s = if lost_out > 1 { "s" } else { "" };
            lost.push(format!("{lost_out} diffusion{s} arrêtée{s}"));
        }
        if lost_in > 0 {
            let s = if lost_in > 1 { "s" } else { "" };
            lost.push(format!("{lost_in} source{s} retirée{s} des entrées"));
        }
        let dialog = adw::MessageDialog::new(
            Some(&self.win),
            Some("Réduire le nombre de canaux ?"),
            Some(&format!(
                "{}. Les logiciels qui utilisent OpenLW In ou OpenLW Out retrouvent les périphériques après quelques secondes.",
                capitalize(&lost.join(", "))
            )),
        );
        dialog.add_responses(&[("cancel", "Annuler"), ("reduce", "Réduire")]);
        dialog.set_response_appearance("reduce", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let answer = dialog.choose_future().await;
            let Some(w) = weak.upgrade() else { return };
            if answer == "reduce" {
                w.mutate(request);
            } else {
                let c = w.st.borrow().config.clone();
                w.apply_config(c);
            }
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
            self.show_error(Some(
                DaemonError::Refused("canal invalide. Saisissez un nombre de 1 à 32766.".into())
                    .message(),
            ));
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

    fn toggle_input(self: &Rc<Self>, row: usize, column: usize) {
        let (r, pair, from_net) = {
            let st = self.st.borrow();
            match (st.grid_rows.get(row), st.pairs.get(column)) {
                (Some(r), Some(p)) => (r.clone(), p.clone(), st.config.channels_from_net),
                _ => return,
            }
        };
        if r.patched_column == Some(column) {
            self.mutate(json!({"cmd": "unpatch_input", "device_channels": pair}));
            return;
        }
        let width = if r.source.patch_kind() == "surround" {
            8
        } else {
            2
        };
        let first = pair.first().copied().unwrap_or(1);
        if first + width - 1 > from_net {
            self.show_error(Some(
                DaemonError::Refused(format!(
                    "une source surround occupe 8 entrées. Choisissez une paire de 1-2 à {}-{}.",
                    from_net.saturating_sub(7),
                    from_net.saturating_sub(6)
                ))
                .message(),
            ));
            // The clicked toggle changed state on its own: show the configured state again.
            self.grid.update(Vec::new(), Vec::new(), None);
            self.update_grid();
            return;
        }
        let channels: Vec<u32> = (first..first + width).collect();
        self.mutate(json!({"cmd": "patch_input", "channel": r.source.channel,
            "kind": r.source.patch_kind(), "device_channels": channels}));
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
        // patch): without remove_input the row would come back as "non annoncé".
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
                    "l'ordinateur n'est pas encore relié au réseau Livewire. Choisissez l'interface, puis réessayez."
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
        let previous = self
            .st
            .borrow()
            .config
            .outputs
            .iter()
            .find(|o| o.device_channels.as_deref() == Some(&row.pair[..]))
            .cloned();
        if !row.emit.is_active() {
            if let Some(p) = previous {
                self.mutate(json!({"cmd": "unpatch_output", "channel": p.channel}));
            }
            return;
        }
        let Some(ch) = row.channel() else {
            self.show_error(Some(
                DaemonError::Refused("canal invalide. Saisissez un nombre de 1 à 32766.".into())
                    .message(),
            ));
            // Nothing transmitted: the switch shows the configured state, even while editing.
            row.set_emit(previous.is_some());
            row.show(previous.as_ref(), self);
            return;
        };
        let patch = json!({"cmd": "patch_output", "channel": ch, "name": row.stream_name(),
            "format": row.format(), "device_channels": row.pair});
        match previous {
            // Channel change: stop this pair's previous stream first.
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
                self.show_error(Some(format!("Démarrage impossible : {e}.")));
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
                    w.show_error(Some(format!(
                        "Démarrage impossible{}{detail}. Consultez le journal : journalctl --user -u openlw.",
                        if detail.is_empty() { "" } else { " : " }
                    )));
                }
                Err(e) => w.show_error(Some(format!("Démarrage impossible : {e}."))),
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
            .comments("Audio Livewire® et AES67 sur l'ordinateur, par PipeWire.\n\nProjet expérimental, fourni « en l'état », sans aucune garantie. Ne convient pas aux environnements critiques (chaîne d'antenne, systèmes de sécurité).\n\nLivewire est une marque de TLS Corp.")
            .build();
        about.present();
    }
}

/// Output row: device pair, meter, channel, advertised name, format, transmission.
struct OutputRow {
    pair: Vec<u32>,
    row: adw::PreferencesRow,
    meter: Meter,
    channel: gtk::Entry,
    name: gtk::Entry,
    format: gtk::DropDown,
    emit: gtk::Switch,
    showing: Cell<bool>,
}

impl OutputRow {
    fn new(pair: Vec<u32>, weak: &Weak<Window>) -> Rc<Self> {
        // Explicit layout rather than an action row: the suffixes would squeeze the title.
        let row = adw::PreferencesRow::builder()
            .title(pair_label("Sorties", &pair))
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
            .label(pair_label("Sorties", &pair))
            .xalign(0.0)
            .width_chars(10)
            .build();
        let meter = Meter::new(2, 110);
        let channel = gtk::Entry::builder()
            .placeholder_text("canal")
            .width_chars(6)
            .max_width_chars(6)
            .input_purpose(gtk::InputPurpose::Digits)
            .valign(gtk::Align::Center)
            .build();
        channel.add_css_class("monospace");
        let name = gtk::Entry::builder()
            .placeholder_text("nom annoncé")
            .width_chars(12)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();
        let format = gtk::DropDown::from_strings(&FORMATS.map(|(_, t)| t));
        format.set_valign(gtk::Align::Center);
        let emit = gtk::Switch::builder()
            .valign(gtk::Align::Center)
            .tooltip_text("Diffuser")
            .build();
        let emit_label = gtk::Label::new(Some("Diffuser"));
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

    fn stream_name(&self) -> String {
        let n = self.name.text().trim().to_string();
        if n.is_empty() {
            pair_label("PC", &self.pair)
        } else {
            n
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

fn count_row(title: &str, unit: &str) -> adw::ComboRow {
    let items: Vec<String> = (1..=MAX_PAIRS)
        .map(|n| {
            format!(
                "{n} {} ({} {unit})",
                if n == 1 { "canal" } else { "canaux" },
                2 * n
            )
        })
        .collect();
    let refs: Vec<&str> = items.iter().map(String::as_str).collect();
    adw::ComboRow::builder()
        .title(title)
        .model(&gtk::StringList::new(&refs))
        .build()
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
