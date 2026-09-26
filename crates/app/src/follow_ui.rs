use easy_switch_core::{Request, Status, config::Config, follow::*};
use eframe::egui::{self, Color32};
#[derive(Default)]
pub struct Editor {
    address: String,
}
impl Editor {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        status: &Status,
        draft: &mut Config,
        dirty: &mut bool,
    ) -> Option<Request> {
        let mut action = None;
        ui.separator();
        ui.heading("Destination follows mouse");
        ui.label("Push past a display edge to switch a linked group. LAN coordination places the destination cursor; radio reconnection still takes time.");
        *dirty |= ui
            .checkbox(
                &mut draft.follow.enabled,
                "Enable destination follows mouse",
            )
            .changed();
        ui.horizontal(|ui| {
            ui.label("This computer");
            let old = draft.follow.local_computer.clone();
            egui::ComboBox::from_id_salt("local-computer")
                .selected_text(
                    draft
                        .computers
                        .get(&draft.follow.local_computer)
                        .map(String::as_str)
                        .unwrap_or("Select computer"),
                )
                .show_ui(ui, |ui| {
                    for (id, name) in &draft.computers {
                        ui.selectable_value(&mut draft.follow.local_computer, id.clone(), name);
                    }
                });
            *dirty |= old != draft.follow.local_computer;
        });
        ui.horizontal(|ui| {
            *dirty |= ui
                .checkbox(&mut draft.follow.discovery, "Discover nearby computers")
                .changed();
            ui.label("LAN port");
            *dirty |= ui
                .add(egui::DragValue::new(&mut draft.follow.port).range(1024..=65535))
                .changed();
        });
        ui.small(
            "Save before pairing. Trust and private keys are stored locally and never exported.",
        );
        ui.horizontal(|ui| {
            if ui
                .button("Detect displays / grant desktop access")
                .clicked()
            {
                action = Some(Action::Authorize);
            }
            if ui.button("Open pairing (2 minutes)").clicked() {
                action = Some(Action::OpenPairing);
            }
        });
        let follow = &status.follow;
        ui.label(format!(
            "{}: {}",
            follow.desktop.backend,
            if follow.desktop.locked && !follow.desktop.lock_handoffs {
                "Locked or inactive session"
            } else if follow.desktop.ready {
                "Ready"
            } else {
                &follow.desktop.reason
            }
        ));
        ui.small(&follow.desktop.cursor_visibility);
        if !follow.desktop.session_kind.is_empty() {
            ui.small(format!(
                "Desktop controller: {} ({})",
                follow.desktop.session_kind, follow.desktop.session
            ));
        }
        if !follow.message.is_empty() {
            ui.label(&follow.message);
        }
        ui.small("Push beyond an edge to switch; touching it is not enough.");
        preview(ui, &follow.desktop.displays);
        for peer in &follow.peers {
            ui.horizontal_wrapped(|ui| {
                ui.strong(&peer.name);
                ui.label(if peer.trusted {
                    if peer.connected {
                        "Paired, connected"
                    } else {
                        "Paired, offline"
                    }
                } else {
                    "Discovered, untrusted"
                });
                ui.small(&peer.address);
                if !peer.trusted && ui.button("Pair").clicked() {
                    action = Some(Action::Connect {
                        address: peer.address.clone(),
                    });
                }
                if peer.trusted && ui.button("Revoke").clicked() {
                    action = Some(Action::Revoke {
                        peer: peer.id.clone(),
                    });
                }
            });
            if !peer.reason.is_empty() {
                ui.small(&peer.reason);
            }
        }
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut self.address);
            if ui.button("Connect to address").clicked() {
                action = Some(Action::Connect {
                    address: self.address.clone(),
                });
            }
        });
        ui.small("Manual address: hostname:24873. Open pairing on both computers first.");
        for pairing in &follow.pairing {
            ui.horizontal(|ui| {
                ui.label(format!("{} — {}", pairing.name, pairing.code));
                if ui
                    .add_enabled(
                        !pairing.confirmed,
                        egui::Button::new("Code matches on both computers"),
                    )
                    .clicked()
                {
                    action = Some(Action::Confirm {
                        peer: pairing.id.clone(),
                        code: pairing.code.clone(),
                    });
                }
            });
        }
        for proposed in &follow.proposed_rules {
            ui.horizontal(|ui| {
                ui.label(format!("Proposed reverse rule {}", proposed.id));
                if !draft.follow.rules.iter().any(|r| r.id == proposed.id)
                    && ui.button("Add to draft for review").clicked()
                {
                    draft.follow.rules.push(proposed.clone());
                    *dirty = true;
                }
            });
        }
        let mut remove = None;
        for (index, rule) in draft.follow.rules.iter_mut().enumerate() {
            ui.push_id(index, |ui| {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        *dirty |= ui.checkbox(&mut rule.enabled, "Enabled").changed();
                        *dirty |= ui.text_edit_singleline(&mut rule.id).changed();
                        if ui.button("Remove").clicked() {
                            remove = Some(index);
                        }
                        if ui.button("Add reverse rule").clicked() {
                            action = Some(Action::Reverse {
                                id: rule.id.clone(),
                            });
                        }
                    });
                    if let Some((_, reason)) = follow.rules.iter().find(|(id, _)| id == &rule.id) {
                        ui.small(reason);
                    }
                    ui.label("Source display and edge segment");
                    segment(
                        ui,
                        "source",
                        &mut rule.source,
                        &follow.desktop.displays,
                        dirty,
                    );
                    let old_peer = rule.peer.clone();
                    egui::ComboBox::from_id_salt("peer")
                        .selected_text(
                            follow
                                .peers
                                .iter()
                                .find(|p| p.id == rule.peer)
                                .map(|p| p.name.as_str())
                                .unwrap_or("Select paired computer"),
                        )
                        .show_ui(ui, |ui| {
                            for p in follow.peers.iter().filter(|p| p.trusted) {
                                ui.selectable_value(&mut rule.peer, p.id.clone(), &p.name);
                            }
                        });
                    if old_peer != rule.peer {
                        if let Some(p) = follow.peers.iter().find(|p| p.id == rule.peer) {
                            rule.computer = p.computer.clone();
                            rule.destination.display.clear();
                            rule.destination.binding.clear();
                        }
                        *dirty = true;
                    }
                    let displays = follow
                        .peers
                        .iter()
                        .find(|p| p.id == rule.peer)
                        .map(|p| p.displays.as_slice())
                        .unwrap_or(&[]);
                    ui.label("Destination display and edge segment");
                    segment(ui, "destination", &mut rule.destination, displays, dirty);
                    preview(ui, displays);
                    let old_group = rule.group.clone();
                    egui::ComboBox::from_id_salt("edge-group")
                        .selected_text(&rule.group)
                        .show_ui(ui, |ui| {
                            for g in &draft.groups {
                                ui.selectable_value(&mut rule.group, g.id.clone(), &g.id);
                            }
                        });
                    if old_group != rule.group {
                        rule.mouse.clear();
                        *dirty = true;
                    }
                    let mice: Vec<_> = status
                        .devices
                        .iter()
                        .filter(|d| {
                            d.kind == "mouse"
                                && draft.groups.iter().any(|g| {
                                    g.id == rule.group && g.devices.iter().any(|m| m.id == d.id)
                                })
                        })
                        .collect();
                    if rule.mouse.is_empty() && mice.len() == 1 {
                        rule.mouse = mice[0].id.clone();
                        *dirty = true;
                    }
                    let old = rule.mouse.clone();
                    egui::ComboBox::from_id_salt("primary-mouse")
                        .selected_text(
                            mice.iter()
                                .find(|d| d.id == rule.mouse)
                                .map(|d| d.name.as_str())
                                .unwrap_or("Select primary mouse"),
                        )
                        .show_ui(ui, |ui| {
                            for d in mice {
                                ui.selectable_value(&mut rule.mouse, d.id.clone(), &d.name);
                            }
                        });
                    *dirty |= old != rule.mouse;
                });
            });
        }
        if let Some(index) = remove {
            draft.follow.rules.remove(index);
            *dirty = true;
        }
        if ui.button("Add edge rule").clicked() {
            let source = follow
                .desktop
                .displays
                .first()
                .map(|d| Segment {
                    display: d.id.clone(),
                    binding: d.binding.clone(),
                    ..Default::default()
                })
                .unwrap_or_default();
            let peer = follow.peers.iter().find(|p| p.trusted);
            let destination = peer
                .and_then(|p| p.displays.first())
                .map(|d| Segment {
                    display: d.id.clone(),
                    binding: d.binding.clone(),
                    edge: source.edge.opposite(),
                    ..Default::default()
                })
                .unwrap_or(Segment {
                    edge: source.edge.opposite(),
                    ..Default::default()
                });
            draft.follow.rules.push(Rule {
                id: format!("edge-{}", rand::random::<u32>()),
                enabled: false,
                source,
                destination,
                peer: peer.map(|p| p.id.clone()).unwrap_or_default(),
                computer: peer.map(|p| p.computer.clone()).unwrap_or_default(),
                group: draft
                    .groups
                    .first()
                    .map(|g| g.id.clone())
                    .unwrap_or_default(),
                mouse: String::new(),
            });
            *dirty = true;
        }
        action.map(|action| Request::Follow { action })
    }
}
fn segment(ui: &mut egui::Ui, salt: &str, s: &mut Segment, displays: &[Display], dirty: &mut bool) {
    ui.push_id(salt, |ui| {
        let previous = s.clone();
        egui::ComboBox::from_id_salt("display")
            .selected_text(
                displays
                    .iter()
                    .find(|d| d.id == s.display)
                    .map(|d| d.name.as_str())
                    .unwrap_or("Select display"),
            )
            .show_ui(ui, |ui| {
                for d in displays {
                    if ui
                        .selectable_value(&mut s.display, d.id.clone(), &d.name)
                        .clicked()
                    {
                        s.binding = d.binding.clone();
                    }
                }
            });
        if let Some(d) = displays.iter().find(|d| d.id == s.display)
            && d.binding != s.binding
            && ui.button("Verify changed display layout").clicked()
        {
            s.binding = d.binding.clone();
        }
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("edge")
                .selected_text(format!("{:?}", s.edge))
                .show_ui(ui, |ui| {
                    for edge in Edge::ALL {
                        ui.selectable_value(&mut s.edge, edge, format!("{edge:?}"));
                    }
                });
            ui.label("From %");
            ui.add(egui::DragValue::new(&mut s.start).range(0.0..=100.0));
            ui.label("to %");
            ui.add(egui::DragValue::new(&mut s.end).range(0.0..=100.0));
        });
        *dirty |= *s != previous;
    });
}
fn preview(ui: &mut egui::Ui, displays: &[Display]) {
    if displays.is_empty() {
        return;
    }
    let minx = displays.iter().map(|d| d.x).fold(f64::INFINITY, f64::min);
    let miny = displays.iter().map(|d| d.y).fold(f64::INFINITY, f64::min);
    let maxx = displays
        .iter()
        .map(|d| d.x + d.width)
        .fold(f64::NEG_INFINITY, f64::max);
    let maxy = displays
        .iter()
        .map(|d| d.y + d.height)
        .fold(f64::NEG_INFINITY, f64::max);
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().min(650.0), 110.0),
        egui::Sense::hover(),
    );
    let scale = ((rect.width() - 8.0) as f64 / (maxx - minx)).min(100.0 / (maxy - miny));
    for (index, d) in displays.iter().enumerate() {
        let r = egui::Rect::from_min_size(
            rect.min + egui::vec2(((d.x - minx) * scale) as f32, ((d.y - miny) * scale) as f32),
            egui::vec2((d.width * scale) as f32, (d.height * scale) as f32),
        );
        ui.painter().rect_stroke(
            r,
            2.0,
            egui::Stroke::new(2.0_f32, Color32::LIGHT_BLUE),
            egui::StrokeKind::Inside,
        );
        ui.painter().text(
            r.center(),
            egui::Align2::CENTER_CENTER,
            format!("{}: {}", index + 1, d.name),
            egui::FontId::proportional(12.0),
            Color32::WHITE,
        );
    }
}
