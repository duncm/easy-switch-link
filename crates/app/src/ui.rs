use crate::{ipc, storage::Paths};
use anyhow::Result;
use easy_switch_core::{
    Request, Response, Status,
    config::{Config, Group, Member, SourceMode},
};
use eframe::egui::{self, Color32, RichText};
use std::time::{Duration, Instant};

pub fn run(paths: Paths) -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Easy-Switch Link")
            .with_inner_size([920., 720.])
            .with_min_inner_size([740., 520.])
            .with_icon(egui::IconData {
                rgba: crate::tray::icon_rgba(),
                width: 32,
                height: 32,
            }),
        ..Default::default()
    };
    eframe::run_native(
        "Easy-Switch Link",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            let mut style = (*cc.egui_ctx.style()).clone();
            style.spacing.item_spacing = egui::vec2(12., 10.);
            cc.egui_ctx.set_style(style);
            Ok(Box::new(Settings::new(paths, cc.egui_ctx.clone())))
        }),
    )
    .map_err(|e| anyhow::anyhow!("Settings window: {e}"))
}

struct Settings {
    about_open: bool,
    edge_editor: crate::follow_ui::Editor,
    paths: Paths,
    status: Status,
    draft: Config,
    loaded: bool,
    dirty: bool,
    busy: bool,
    message: String,
    requests: crossbeam_channel::Sender<Request>,
    replies: crossbeam_channel::Receiver<std::result::Result<Response, String>>,
    last_refresh: Instant,
    file_path: String,
    new_group: String,
}
impl Settings {
    fn new(paths: Paths, context: egui::Context) -> Self {
        let (tx, rx) = crossbeam_channel::bounded(8);
        let (reply_tx, replies) = crossbeam_channel::bounded(8);
        let worker_paths = paths.clone();
        std::thread::spawn(move || {
            while let Ok(request) = rx.recv() {
                let reply = ipc::call(&worker_paths, request).map_err(|e| format!("{e:#}"));
                if reply_tx.send(reply).is_err() {
                    break;
                }
                context.request_repaint();
            }
        });
        let _ = tx.try_send(Request::Status);
        Self {
            about_open: false,
            edge_editor: Default::default(),
            paths,
            status: Status::default(),
            draft: Config::default(),
            loaded: false,
            dirty: false,
            busy: true,
            message: "Choose devices and save to begin linking them.".into(),
            requests: tx,
            replies,
            last_refresh: Instant::now(),
            file_path: String::new(),
            new_group: String::new(),
        }
    }
    fn send(&mut self, request: Request) {
        if self.requests.try_send(request).is_ok() {
            self.busy = true;
            self.last_refresh = Instant::now();
        }
    }
    fn update_replies(&mut self) {
        while let Ok(reply) = self.replies.try_recv() {
            self.busy = false;
            match reply {
                Ok(response) => {
                    if response.message != "Ready" {
                        self.message = response.message.clone();
                    }
                    if let Some(status) = response.status {
                        if !self.loaded || (!self.dirty && response.ok) {
                            self.draft = status.config.clone();
                        }
                        if response.message == "Configuration saved" && response.ok {
                            self.draft = status.config.clone();
                            self.dirty = false;
                        }
                        self.status = status;
                        self.loaded = true;
                    }
                }
                Err(error) => self.message = error,
            }
        }
    }
}
impl eframe::App for Settings {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::Window::new("About Easy-Switch Link")
            .open(&mut self.about_open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, about_contents);
        self.update_replies();
        // Only the visible settings process refreshes status; the monitor never polls hardware.
        if !self.busy && self.last_refresh.elapsed() >= Duration::from_secs(2) {
            self.send(Request::Status);
        }
        ctx.request_repaint_after(Duration::from_secs(2));
        egui::TopBottomPanel::top("heading").show(ctx, |ui| {
            ui.add_space(12.);
            ui.horizontal(|ui| {
                ui.heading(
                    RichText::new("Easy-Switch Link").color(Color32::from_rgb(98, 222, 193)),
                );
                ui.label("One switch. Your devices follow.");
                if ui.button("About").clicked() {
                    self.about_open = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label(if self.status.paused {
                    "Paused"
                } else if !self.status.configured {
                    "Setup needed"
                } else {
                    "Monitoring"
                });
                ui.separator();
                ui.label(format!(
                    "{} receivers · {} connected devices",
                    self.status.receivers.len(),
                    self.status.devices.iter().filter(|d| d.online).count()
                ));
                if self.busy {
                    ui.spinner();
                }
            });
            ui.add_space(8.);
            if self.status.service.mode == "system" {
                ui.label(format!(
                    "System service · owner UID {} · {}",
                    self.status.service.owner_uid.unwrap_or_default(),
                    self.status.service.seat
                ));
                ui.small(&self.status.service.observation);
                ui.small(if self.status.service.login_handoffs {
                    "Login/lock handoffs enabled; GNOME companion required"
                } else {
                    "Pre-login hardware synchronization enabled"
                });
            }
        });
        egui::TopBottomPanel::bottom("actions").show(ctx, |ui| {
            ui.add_space(8.);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        self.loaded && !self.busy,
                        egui::Button::new("Save configuration").fill(Color32::from_rgb(25, 95, 80)),
                    )
                    .clicked()
                {
                    match self.draft.validate() {
                        Ok(()) => self.send(Request::Save {
                            config: self.draft.clone(),
                        }),
                        Err(e) => self.message = format!("{e:#}"),
                    }
                }
                if ui
                    .button(if self.status.paused {
                        "Resume"
                    } else {
                        "Pause"
                    })
                    .clicked()
                {
                    self.send(if self.status.paused {
                        Request::Resume
                    } else {
                        Request::Pause
                    });
                }
                if ui.button("Rescan").clicked() {
                    self.send(Request::Rescan);
                }
                if self.dirty {
                    ui.label("Unsaved changes");
                }
            });
            ui.label(&self.message);
            ui.add_space(8.);
        });
        egui::CentralPanel::default().show(ctx, |ui| { egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("Computers");
            ui.label("Name each destination. Map channels separately for every device below.");
            for (id, label) in &mut self.draft.computers {
                ui.horizontal(|ui| { ui.label(id); self.dirty |= ui.text_edit_singleline(label).changed(); });
            }
            ui.add_space(10.); ui.separator(); ui.heading("Linked devices");
            ui.label("Select a group to link a device. Run Easy-Switch Link on every participating computer.");
            if self.status.devices.is_empty() {
                ui.label("No compatible devices identified yet. Wake your devices, check the receiver and use Rescan.");
                #[cfg(target_os = "linux")]
                ui.label("On Linux, install the supplied udev permissions rule and reconnect the receiver.");
            }
            let devices = self.status.devices.clone();
            for device in devices {
                ui.push_id(&device.id, |ui| {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.horizontal(|ui| { ui.strong(&device.name); ui.label(if device.online { format!("Connected · channel {}", device.current_host + 1) } else { "Unavailable on this computer".into() }); });
                    ui.small(&device.capability);
                    let current_group = self.draft.groups.iter().find(|g| g.devices.iter().any(|m| m.id == device.id)).map(|g| g.id.clone());
                    let mut chosen_group = current_group.clone();
                    let groups: Vec<_> = self.draft.groups.iter().map(|g| g.id.clone()).collect();
                    egui::ComboBox::from_id_salt(("group", &device.id)).selected_text(chosen_group.as_deref().unwrap_or("Not linked")).show_ui(ui, |ui| {
                        ui.selectable_value(&mut chosen_group, None, "Not linked");
                        for id in groups { ui.selectable_value(&mut chosen_group, Some(id.clone()), id); }
                        if self.draft.groups.is_empty() { ui.selectable_value(&mut chosen_group, Some("desk".into()), "desk"); }
                    });
                    if chosen_group != current_group {
                        for group in &mut self.draft.groups { group.devices.retain(|m| m.id != device.id); }
                        if let Some(id) = chosen_group {
                            if !self.draft.groups.iter().any(|g| g.id == id) { self.draft.groups.push(Group { id: id.clone(), devices: vec![] }); }
                            let channels = self.draft.computers.keys().take(device.host_count as usize).enumerate().map(|(i, pc)| (pc.clone(), i as u8 + 1)).collect();
                            self.draft.groups.iter_mut().find(|g| g.id == id).unwrap().devices.push(Member { id: device.id.clone(), channels, mode: SourceMode::Auto });
                        }
                        self.dirty = true;
                    }
                    let computers = self.draft.computers.clone();
                    if let Some(member) = self.draft.groups.iter_mut().flat_map(|g| &mut g.devices).find(|m| m.id == device.id) {
                        ui.horizontal_wrapped(|ui| {
                            for (pc, label) in computers {
                                let mut channel = member.channels.get(&pc).copied().unwrap_or(0);
                                let old = channel;
                                egui::ComboBox::from_id_salt(("channel", &device.id, &pc)).selected_text(format!("{label}: {}", if channel == 0 { "unmapped".into() } else { channel.to_string() })).show_ui(ui, |ui| {
                                    ui.selectable_value(&mut channel, 0, "Not paired");
                                    for ch in 1..=device.host_count { ui.selectable_value(&mut channel, ch, format!("Channel {ch}")); }
                                });
                                if channel != old { if channel == 0 { member.channels.remove(&pc); } else { member.channels.insert(pc, channel); } self.dirty = true; }
                            }
                        });
                        let before = member.mode;
                        egui::ComboBox::from_id_salt(("mode", &device.id)).selected_text(match member.mode { SourceMode::Auto => "Automatic", SourceMode::Intercept => "Temporarily intercept switch keys", SourceMode::FollowOnly => "Follow only" }).show_ui(ui, |ui| {
                            ui.selectable_value(&mut member.mode, SourceMode::Auto, "Automatic (passive preferred)");
                            if device.controls.iter().any(|c| c.divertable) { ui.selectable_value(&mut member.mode, SourceMode::Intercept, "Temporarily intercept switch keys"); }
                            ui.selectable_value(&mut member.mode, SourceMode::FollowOnly, "Follow only");
                        });
                        self.dirty |= before != member.mode;
                        if member.mode == SourceMode::Intercept || device.intercepted { ui.small("Pause before pairing. After a crash, use Recover reporting or power-cycle the device."); }
                    }
                    ui.collapsing("Device identity", |ui| { ui.monospace(&device.id); ui.small(&device.receiver); ui.label("Event support is unverified until tested with this firmware."); });
                });
                });
            }
            ui.horizontal(|ui| {
                ui.text_edit_singleline(&mut self.new_group);
                if ui.button("Add group").clicked() && !self.new_group.trim().is_empty() {
                    let id = self.new_group.trim().to_string();
                    if !self.draft.groups.iter().any(|g| g.id == id) { self.draft.groups.push(Group { id, devices: vec![] }); self.dirty = true; self.new_group.clear(); }
                }
            });
            if let Some(request)=self.edge_editor.show(ui,&self.status,&mut self.draft,&mut self.dirty) { self.send(request); }
            ui.separator(); ui.heading("Switch now");
            let mut switch = None;
            for group in &self.status.config.groups {
                ui.horizontal_wrapped(|ui| { ui.label(&group.id); for (pc, label) in &self.status.config.computers { if ui.add_enabled(!self.status.paused, egui::Button::new(label)).clicked() { switch = Some(Request::Switch { group: group.id.clone(), computer: pc.clone() }); } } });
            }
            if let Some(request) = switch { self.send(request); }
            ui.separator();
            ui.add_enabled_ui(self.status.service.mode!="system", |ui| {self.dirty |= ui.checkbox(&mut self.draft.start_at_login, "Start at login").changed();});
            if self.status.service.mode=="system" {ui.small("The system service starts at boot; its session companion starts at login. Use service disable to return to user-only startup.");}
            ui.collapsing("Import / export configuration", |ui| {
                ui.label("Use a full file path. Imported device identities must be discovered on this computer before saving.");
                ui.text_edit_singleline(&mut self.file_path);
                ui.horizontal(|ui| {
                    if ui.button("Export draft").clicked() {
                        let result = (|| -> Result<()> { self.draft.validate()?; let path = std::path::Path::new(&self.file_path); anyhow::ensure!(path.is_absolute(), "Use an absolute path"); crate::storage::atomic_write(path, toml::to_string_pretty(&self.draft)?.as_bytes()) })();
                        self.message = match result { Ok(()) => "Exported configuration".into(), Err(e) => format!("{e:#}") };
                    }
                    if ui.button("Import draft").clicked() {
                        let result = (|| -> Result<Config> { let config: Config = toml::from_str(&std::fs::read_to_string(&self.file_path)?)?; config.validate()?; Ok(config) })();
                        match result { Ok(config) => { self.draft = config; self.dirty = true; self.message = "Imported draft; review mappings and save".into(); }, Err(e) => self.message = format!("{e:#}") }
                    }
                });
            });
            ui.collapsing("Diagnostics and recovery", |ui| {
                ui.small(format!("Configuration: {}", self.paths.config().display()));
                for (id, state) in &self.status.receivers { ui.label(state); ui.small(id); }
                for message in self.status.messages.iter().rev() { ui.label(message); }
                if ui.button("Recover reporting and pause").clicked() { self.send(Request::Recover); }
            });
            ui.add_space(12.);
        }); });
    }
}

fn about_contents(ui: &mut egui::Ui) {
    ui.heading("Easy-Switch Link");
    ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
    ui.label("Link your Logitech Easy-Switch devices across computers.");
    ui.label("With optional Destination follows mouse.");
    ui.separator();
    ui.label(format!("License: {}", env!("CARGO_PKG_LICENSE")));
}

/// A separate short-lived UI, also available when the monitor is not running.
pub fn about() -> Result<()> {
    struct About;
    impl eframe::App for About {
        fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
            egui::CentralPanel::default().show(ctx, |ui| {
                about_contents(ui);
                ui.add_space(12.0);
                if ui.button("Close").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        }
    }
    eframe::run_native(
        "About Easy-Switch Link",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([460.0, 230.0])
                .with_resizable(false)
                .with_icon(egui::IconData {
                    rgba: crate::tray::icon_rgba(),
                    width: 32,
                    height: 32,
                }),
            ..Default::default()
        },
        Box::new(|_| Ok(Box::new(About))),
    )
    .map_err(|e| anyhow::anyhow!("About window: {e}"))
}
