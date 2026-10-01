//! Desktop presentation. Transfer state and workers live in the application model.
use super::*;
use gpui::{ClipboardItem, ExternalPaths, Svg, rgb, svg};

impl Desktop {
    fn icon(&self, name: &'static str, pixels: f32) -> Svg {
        svg()
            .path(name)
            .size(px(pixels))
            .text_color(self.color(0x95a5b8))
            .flex_shrink_0()
    }
    pub(super) fn switch(&mut self, view: View, action: Option<Action>, cx: &mut Context<Self>) {
        self.view = view;
        if let Some(action) = action
            && self.job.is_none()
            && self.retiring.is_none()
            && self.action != action
        {
            self.action = action;
            self.preview_revision = None;
            self.summary = None;
            self.policy = ConflictPolicy::Preserve;
            self.preview_snapshot = None;
        }
        if matches!(view, View::Settings | View::Trusted) {
            self.refresh_peers();
        }
        cx.notify();
    }
    fn sidebar(&self, compact: bool, cx: &mut Context<Self>) -> Div {
        let mut links = div().flex().flex_col().gap_2();
        for (id, label, icon, view, action) in [
            ("home", "Home", "home", View::Workflow, None),
            ("transfers", "Transfers", "transfer", View::Transfers, None),
            ("sync", "Sync", "sync", View::Workflow, Some(Action::Sync)),
            ("trusted", "Trusted Devices", "shield", View::Trusted, None),
        ] {
            let selected = self.view == view
                && if id == "sync" {
                    self.action.syncing()
                } else if id == "home" {
                    !self.action.syncing()
                } else {
                    true
                };
            links = links.child(
                self.button(id, "")
                    .bg(self.color(0x1c2735))
                    .border_color(self.color(0x1c2735))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(self.icon(icon, 20.))
                    .when(!compact, |row| row.child(label))
                    .when(selected, |row| {
                        row.bg(self.color(0x213b39))
                            .text_color(self.color(0x88e5cf))
                            .border_color(self.color(0x213b39))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.switch(view, action, cx))),
            );
        }
        div()
            .w(px(if compact { 68. } else { 212. }))
            .h_full()
            .flex_shrink_0()
            .bg(self.color(0x1c2735))
            .border_r_1()
            .border_color(self.color(0x243141))
            .p_3()
            .flex()
            .flex_col()
            .gap_6()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .py_3()
                    .child(img(self.logo.clone()).size(px(34.)))
                    .when(!compact, |row| {
                        row.child(
                            div()
                                .text_2xl()
                                .font_weight(gpui::FontWeight::BOLD)
                                .child("XFER"),
                        )
                    }),
            )
            .child(links)
            .child(div().flex_1())
            .child(
                self.button("settings", "")
                    .when(self.view == View::Settings, |row| {
                        row.border_color(self.color(0x63e6c9))
                    })
                    .bg(self.color(0x1c2735))
                    .border_color(self.color(0x1c2735))
                    .flex()
                    .gap_3()
                    .items_center()
                    .child(self.icon("settings", 20.))
                    .when(!compact, |row| row.child("Settings"))
                    .on_click(cx.listener(|this, _, _, cx| this.switch(View::Settings, None, cx))),
            )
    }
    fn header(&self, compact: bool, cx: &App) -> Div {
        let title = match self.view {
            View::Settings => "Settings",
            View::Trusted => "Trusted devices",
            View::Transfers => "Your transfers",
            View::Workflow => match self.action {
                Action::Copy => "Send files securely",
                Action::Receive => "Receive files securely",
                _ => "Keep your folders in sync",
            },
        };
        let description = match self.view {
            View::Workflow => "Fast. Private. Direct. Encrypted file transfers and folder sync.",
            View::Transfers => "Progress, results, and details for this window.",
            View::Trusted => "Compare security codes before trusting a new computer.",
            View::Settings => "Your preferences and identity are shared with the CLI.",
        };
        let endpoint = format!(
            "{}:{}",
            self.inputs[1].read(cx).value(),
            self.inputs[2].read(cx).value()
        );
        let trusted = self.known.iter().any(|(known, _)| known == &endpoint);
        let badges = div()
            .flex()
            .gap_5()
            .items_center()
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(self.icon("shield", 28.).text_color(self.color(0x88e5cf)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(if self.secure {
                                "End-to-end encrypted"
                            } else {
                                "Encryption off"
                            })
                            .child(div().text_xs().text_color(self.color(0x95a5b8)).child(
                                if self.secure {
                                    "X25519 · ChaCha20-Poly1305"
                                } else {
                                    "Use only on trusted networks"
                                },
                            )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(self.icon("check", 24.).text_color(self.color(0x88e5cf)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(if trusted {
                                "Remembered identity"
                            } else {
                                "Verify on connection"
                            })
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(self.color(0x95a5b8))
                                    .child("Compare the security code"),
                            ),
                    ),
            );
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(if compact { 26. } else { 30. }))
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(self.color(0x95a5b8))
                            .child(description),
                    ),
            )
            .when(!compact, |row| row.child(badges.text_xs()))
    }
    fn tabs(&self, cx: &mut Context<Self>) -> Div {
        let mut tabs = div()
            .flex()
            .gap_1()
            .p_1()
            .rounded_lg()
            .bg(self.color(0x1c2735));
        for (id, title, icon, action) in [
            ("send-tab", "Send", "send", Action::Copy),
            ("receive-tab", "Receive", "receive", Action::Receive),
            ("sync-tab", "Sync", "sync", Action::Sync),
            ("two-way-tab", "Two-way", "sync", Action::TwoWay),
        ] {
            tabs = tabs.child(
                self.button(id, "")
                    .flex_1()
                    .flex()
                    .justify_center()
                    .items_center()
                    .gap_2()
                    .child(
                        self.icon(icon, 16.)
                            .when(self.action == action, |icon| icon.text_color(rgb(0xffffff))),
                    )
                    .child(title)
                    .when(self.action == action, |row| {
                        row.bg(self.color(0x63e6c9))
                            .text_color(rgb(0xffffff))
                            .border_color(self.color(0x63e6c9))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.switch(View::Workflow, Some(action), cx)
                    })),
            );
        }
        tabs
    }
    fn content_picker(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let receiving = self.action == Action::Receive;
        let path = self.inputs[0].read(cx).value();
        let chosen = !path.is_empty();
        let busy = self.job.is_some() || self.retiring.is_some();
        div().id("drop-zone").p_5().min_h(px(252.)).rounded_lg().border_1().border_color(self.color(0x4b9f8f))
            .bg(self.color(0x151e29)).flex().flex_col().items_center().justify_center().gap_3()
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                if this.job.is_some() || this.retiring.is_some() { return; }
                if paths.paths().len() != 1 {
                    this.error = Some("Drop one file or folder. To send several files, choose their containing folder.".into());
                } else if let Some(path) = paths.paths().first() {
                    this.select_path(path.clone(), cx);
                    this.error = None;
                }
                cx.notify();
            }))
            .child(self.icon("folder", 44.))
            .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child(if receiving { "Choose where files arrive" } else if chosen { "Content selected" } else { "Drop a file or folder here" }))
            .child(div().text_sm().text_color(self.color(0x95a5b8)).child(if receiving { "Files are saved inside your destination folder." } else { "Or select content from your computer." }))
            .child(div().flex().gap_2().flex_wrap().justify_center()
                .when(self.action == Action::Copy, |row| row.child(self.button("select-files", "Select File…").bg(self.color(0x63e6c9)).text_color(rgb(0xffffff))
                    .when(busy, |row| row.opacity(0.5)).on_click(cx.listener(|this, _, _, cx| this.choose(false, cx)))))
                .child(self.button("select-folder", "Select Folder…") .when(busy, |row| row.opacity(0.5)).on_click(cx.listener(|this, _, _, cx| this.choose(true, cx)))))
            .child(div().w_full().child(self.field(if receiving { "Destination folder" } else { "Selected path" }, 0)))
            .child(div().text_xs().text_color(self.color(0x95a5b8)).child("Direct (LAN) · IPv4 / IPv6 · No cloud storage"))
    }
    fn devices(&self, wide: bool, cx: &mut Context<Self>) -> Div {
        let mut cards = div().flex().flex_col().gap_2();
        if self.peers.is_empty() {
            cards =
                cards.child(
                    div()
                        .h(px(128.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(self.color(0x95a5b8))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .items_center()
                                .child(self.icon("computer", 32.))
                                .child("Looking for nearby receivers…")
                                .child(div().text_xs().child(
                                    "Start receiving on another computer, or add its address.",
                                )),
                        ),
                );
        } else if wide && self.peers.len() <= 3 {
            cards = cards.flex_row();
            for (index, peer) in self.peers.iter().enumerate() {
                let peer = peer.clone();
                let endpoint = peer.address.to_string();
                let trusted = self.known.iter().any(|(known, _)| known == &endpoint);
                let selected = self.inputs[1].read(cx).value() == peer.address.ip().to_string()
                    && self.inputs[2].read(cx).value() == peer.address.port().to_string();
                cards =
                    cards.child(
                        self.button(("device-card", index), "")
                            .flex_1()
                            .min_w_0()
                            .h(px(142.))
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap_2()
                            .child(self.icon("computer", 28.))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(peer.name.clone()),
                            )
                            .child(div().text_xs().text_color(self.color(0x88e5cf)).child(
                                if trusted {
                                    "● Remembered"
                                } else {
                                    "● Verify code"
                                },
                            ))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(self.color(0x95a5b8))
                                    .child(endpoint),
                            )
                            .when(selected, |row| {
                                row.border_color(self.color(0x63e6c9))
                                    .bg(self.color(0x213b39))
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.job.is_some() || this.retiring.is_some() {
                                    return;
                                }
                                this.inputs[1].update(cx, |input, cx| {
                                    input.set(peer.address.ip().to_string(), cx)
                                });
                                this.inputs[2].update(cx, |input, cx| {
                                    input.set(peer.address.port().to_string(), cx)
                                });
                                cx.notify();
                            })),
                    );
            }
        } else {
            cards = cards.child(
                uniform_list(
                    "nearby-devices",
                    self.peers.len(),
                    cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                        range
                            .map(|index| {
                                let peer = this.peers[index].clone();
                                let endpoint = peer.address.to_string();
                                let trusted =
                                    this.known.iter().any(|(known, _)| known == &endpoint);
                                let selected = this.inputs[1].read(cx).value()
                                    == peer.address.ip().to_string()
                                    && this.inputs[2].read(cx).value()
                                        == peer.address.port().to_string();
                                this.button(("device", index), "")
                                    .h(px(88.))
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .gap_4()
                                    .child(this.icon("computer", 32.))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                                    .child(peer.name.clone()),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(this.color(0x95a5b8))
                                                    .child(endpoint),
                                            )
                                            .child(div().text_xs().child(if peer.secure {
                                                "● Online · Encryption on"
                                            } else {
                                                "● Online · Encryption off"
                                            })),
                                    )
                                    .child(
                                        div().text_xs().text_color(this.color(0x88e5cf)).child(
                                            if trusted { "Remembered" } else { "Verify code" },
                                        ),
                                    )
                                    .when(selected, |row| {
                                        row.border_color(this.color(0x63e6c9))
                                            .bg(this.color(0x213b39))
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if this.job.is_some() || this.retiring.is_some() {
                                            return;
                                        }
                                        this.inputs[1].update(cx, |input, cx| {
                                            input.set(peer.address.ip().to_string(), cx)
                                        });
                                        this.inputs[2].update(cx, |input, cx| {
                                            input.set(peer.address.port().to_string(), cx)
                                        });
                                        cx.notify();
                                    }))
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .h(px((self.peers.len().min(3) * 88) as f32)),
            );
        }
        self.card("Nearby Devices")
            .child(cards)
            .child(
                div()
                    .flex()
                    .gap_3()
                    .items_end()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(self.field("Address or hostname", 1)),
                    )
                    .child(div().w(px(90.)).child(self.field("Port", 2))),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(self.color(0x95a5b8))
                    .child("Choose a nearby device or enter an address manually."),
            )
    }
    fn connection_cards(&self, wide: bool, cx: &mut Context<Self>) -> Div {
        let receiving = self.action == Action::Receive && self.job.is_some();
        let mut receiver = self
            .card(if receiving {
                "Ready to receive"
            } else {
                "Receive on this computer"
            })
            .child(
                div()
                    .text_sm()
                    .text_color(self.color(0x95a5b8))
                    .child(if receiving {
                        "Keep this window open. Compare security codes when prompted."
                    } else {
                        "Choose a destination, then start your receiver."
                    }),
            );
        if receiving {
            if let Some(addresses) = &self.receiver_addresses {
                let copy = addresses.clone();
                receiver = receiver
                    .child(div().text_sm().child(addresses.clone()))
                    .child(self.button("copy-address", "Copy addresses").on_click(
                        move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                        },
                    ));
            }
            receiver = receiver.child(
                self.button("stop-receiver", "Turn off")
                    .on_click(cx.listener(|this, _, window, cx| this.cancel(&Cancel, window, cx))),
            );
        } else {
            receiver = receiver.child(self.button("receive-here", "Set up receiving").on_click(
                cx.listener(|this, _, _, cx| {
                    this.switch(View::Workflow, Some(Action::Receive), cx)
                }),
            ));
        }
        let token = self
            .card("Share a temporary token")
            .child(div().text_sm().text_color(self.color(0x95a5b8)).child(
                "Use the same token on both computers. It stays in memory and is never saved.",
            ))
            .child(
                self.button("generate-token", "Generate Token")
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.job.is_some() || this.retiring.is_some() {
                            return;
                        }
                        match workflow::temporary_token() {
                            Ok(token) => {
                                this.inputs[3].update(cx, |input, cx| input.set(token, cx));
                                this.advanced = true;
                            }
                            Err(error) => this.error = Some(error.to_string()),
                        }
                        cx.notify();
                    })),
            );
        div()
            .flex()
            .gap_3()
            .when(!wide, |row| row.flex_col())
            .child(receiver.flex_1().min_w_0())
            .child(token.flex_1().min_w_0())
    }
    fn form(&self, wide: bool, cx: &mut Context<Self>) -> Div {
        let receiving = self.action == Action::Receive;
        let mut form = div().flex().flex_col().gap_4();
        let mut columns = div().flex().gap_4().when(!wide, |row| row.flex_col());
        columns = columns.child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_3()
                .child(self.tabs(cx))
                .child(self.content_picker(cx)),
        );
        let mut right = div().flex_1().min_w_0().flex().flex_col().gap_3();
        if !receiving {
            right = right.child(self.devices(wide, cx));
        }
        right = right.child(self.connection_cards(wide, cx));
        form = form.child(columns.child(right));
        form = form.child(
            self.button(
                "advanced",
                if self.advanced {
                    "Hide transfer options"
                } else {
                    "Transfer options"
                },
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.advanced = !this.advanced;
                cx.notify();
            })),
        );
        if self.advanced {
            let mut advanced = self.card("Transfer options").child(
                div()
                    .flex()
                    .gap_3()
                    .child(div().flex_1().min_w_0().child(self.field("Port", 2)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(self.field("Shared token (optional)", 3)),
                    ),
            );
            advanced = advanced.child(self.field(
                if receiving {
                    "Bind address"
                } else {
                    "Exclude globs (separate with ;)"
                },
                if receiving { 5 } else { 4 },
            ));
            let mut options = div().flex().flex_wrap().gap_2().child(self.checkbox(
                "Encryption",
                self.secure,
                Toggle::Secure,
                cx,
            ));
            if self.action.syncing() {
                options = options.child(self.checkbox(
                    "Respect .gitignore",
                    self.gitignore,
                    Toggle::Gitignore,
                    cx,
                ));
            }
            if self.action == Action::Copy {
                options = options.child(self.checkbox(
                    "Follow safe symlinks",
                    self.follow_links,
                    Toggle::FollowLinks,
                    cx,
                ));
            }
            if receiving {
                options = options
                    .child(self.checkbox("Allow sync", self.allow_sync, Toggle::SyncAccess, cx))
                    .child(self.checkbox("Overwrite copies", self.overwrite, Toggle::Overwrite, cx))
                    .child(self.checkbox(
                        "Nearby discovery",
                        self.discoverable,
                        Toggle::Discovery,
                        cx,
                    ));
                advanced = advanced.child(
                    div()
                        .text_sm()
                        .text_color(self.color(0x95a5b8))
                        .child("Sync updates this folder directly. Copies are saved inside it."),
                );
            }
            form = form.child(advanced.child(options));
        }
        if !self.secure {
            form = form.child(self.card("Encryption is off").child(
                "Both computers must use this mode. Enable encryption for private transfers.",
            ));
        }
        form
    }
    fn settings(&self, cx: &mut Context<Self>) -> Div {
        let mut body = div().flex().flex_col().gap_4();
        let mut peers = self.card("Remembered computers");
        if self.known.is_empty() {
            peers = peers.child(div().text_color(self.color(0x95a5b8)).child("No remembered peers yet. Approve a security code during your first transfer to remember a computer."));
        } else {
            peers = peers
                .child(
                    uniform_list(
                        "known-peers",
                        self.known.len(),
                        cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                            range
                                .map(|i| {
                                    let (endpoint, fingerprint) = this.known[i].clone();
                                    div()
                                        .h(px(64.))
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_3()
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .flex()
                                                .flex_col()
                                                .gap_1()
                                                .child(endpoint.clone())
                                                .child(
                                                    div()
                                                        .text_sm()
                                                        .text_color(this.color(0x95a5b8))
                                                        .child(fingerprint),
                                                ),
                                        )
                                        .child(
                                            this.button(i, "Forget")
                                                .text_color(this.color(0xffbacb))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.peer_update(Some(endpoint.clone()), cx)
                                                })),
                                        )
                                })
                                .collect::<Vec<_>>()
                        }),
                    )
                    .h(px((self.known.len().min(4) * 64) as f32)),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(self.color(0x95a5b8))
                        .child("Forgetting a computer requires comparing its security code again."),
                )
                .child(
                    self.button(
                        "clear-peers",
                        if self.confirm_clear {
                            "Confirm forget all computers"
                        } else {
                            "Forget all computers"
                        },
                    )
                    .text_color(self.color(0xffbacb))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.confirm_clear {
                            this.confirm_clear = false;
                            this.peer_update(None, cx);
                        } else {
                            this.confirm_clear = true;
                            cx.notify();
                        }
                    })),
                );
        }
        body = body.child(peers);
        if self.view == View::Trusted {
            return body;
        }
        body = body.child(
            self.card("About XFER")
                .child(format!("Version {}", xfer::VERSION))
                .child(
                    div()
                        .text_sm()
                        .text_color(self.color(0x95a5b8))
                        .child(format!(
                            "Configuration: {}",
                            self.config
                                .as_ref()
                                .map_or("~/.xfer".into(), |p| p.display().to_string())
                        )),
                )
                .child(
                    self.button("releases", "Download desktop releases")
                        .on_click(|_, _, cx| cx.open_url(RELEASES)),
                ),
        );
        body
    }
    fn feedback(&self, cx: &mut Context<Self>) -> Div {
        let running = self.job.is_some() || self.retiring.is_some();
        let mut body = self.card(if running {
            "Active Transfer"
        } else if self.summary.is_some() {
            "Transfer result"
        } else {
            "Ready when you are"
        });
        if let Some((prompt, _)) = &self.trust {
            body = body.child(
                div()
                    .p_4()
                    .rounded_lg()
                    .bg(self.color(0x3b2e18))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(if prompt.changed {
                        "WARNING: saved peer identity changed"
                    } else {
                        "Compare this code on both computers"
                    })
                    .child(div().text_2xl().child(prompt.sas.clone()))
                    .child(prompt.endpoint.clone())
                    .child(prompt.fingerprint.clone())
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                self.button("trust", "Codes match — trust peer")
                                    .on_click(cx.listener(|this, _, _, cx| this.answer(true, cx))),
                            )
                            .child(
                                self.button("reject", "Reject")
                                    .on_click(cx.listener(|this, _, _, cx| this.answer(false, cx))),
                            ),
                    ),
            );
        } else if let Some((sas, fingerprint)) = &self.sas {
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(format!("Security code: {sas}"))
                    .child(fingerprint.clone()),
            );
        }
        if let Some(progress) = &self.progress {
            let fraction = if progress.total == 0 {
                0.
            } else {
                (progress.transferred as f32 / progress.total as f32).clamp(0., 1.)
            };
            body = body
                .child(format!("{} · {}", progress.phase, progress.current_path))
                .child(
                    div().h(px(8.)).rounded_md().bg(self.color(0x1c2735)).child(
                        div()
                            .h_full()
                            .w(relative(fraction))
                            .bg(self.color(0x63e6c9)),
                    ),
                )
                .child(format!(
                    "{} / {} · {} / {} files",
                    human_bytes(progress.transferred),
                    human_bytes(progress.total),
                    progress.files_done,
                    progress.files_total
                ));
        }
        if let Some(summary) = &self.summary {
            body = body.child(format!(
                "{} · {} files · {}",
                if summary.preview {
                    "Preview"
                } else {
                    "Complete"
                },
                summary.file_count,
                summary.destination.display()
            ));
            if let Some(stats) = summary.sync_stats {
                body = body.child(format!(
                    "{} changes · {} unchanged · {} to send · {} reused",
                    stats.changed_files,
                    stats.unchanged_files,
                    human_bytes(stats.sent_bytes),
                    human_bytes(stats.reused_bytes)
                ));
            }
            if !summary.conflicts.is_empty() {
                body = body
                    .child(format!("{} conflicts preserved", summary.conflicts.len()))
                    .child(
                        uniform_list(
                            "conflicts",
                            summary.conflicts.len(),
                            cx.processor(|this, range: std::ops::Range<usize>, _, _| {
                                range
                                    .map(|i| {
                                        div().h(px(28.)).child(
                                            this.summary.as_ref().unwrap().conflicts[i].clone(),
                                        )
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .h(px(110.)),
                    );
            }
            if self.action == Action::TwoWay && summary.preview {
                let mut choices = div().flex().gap_2().flex_wrap();
                for (id, label, policy) in [
                    ("preserve", "Preserve both", ConflictPolicy::Preserve),
                    ("local", "Prefer local", ConflictPolicy::PreferLocal),
                    ("remote", "Prefer remote", ConflictPolicy::PreferRemote),
                ] {
                    choices = choices.child(self.button(id, label).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.policy = policy;
                            this.preview_revision = None;
                            this.start(true, cx);
                        },
                    )));
                }
                body = body.child(choices);
            }
        }
        let primary = if self.retiring.is_some() {
            "Stopping…"
        } else if running {
            "Cancel"
        } else if self.action == Action::Receive {
            "Start receiving"
        } else if self.action.syncing() && self.preview_revision == Some(self.revision) {
            "Apply reviewed sync"
        } else if self.action.syncing() {
            "Preview sync"
        } else {
            "Send"
        };
        body = body.child(
            div()
                .flex()
                .gap_2()
                .child(
                    self.button("primary", primary)
                        .bg(self.color(0x63e6c9))
                        .text_color(rgb(0xffffff))
                        .on_click(cx.listener(|this, _, window, cx| {
                            if this.job.is_some() {
                                this.cancel(&Cancel, window, cx);
                            } else {
                                this.view = View::Workflow;
                                this.start(false, cx);
                            }
                        })),
                )
                .when(self.action.syncing(), |row| {
                    row.child(
                        self.button("preview", "Refresh preview")
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.action.syncing() {
                                    this.start(true, cx);
                                }
                            })),
                    )
                })
                .child(self.button("details", "Details").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.details = !this.details;
                        cx.notify();
                    },
                ))),
        );
        if running && self.progress.is_none() {
            body = body.child(if self.retiring.is_some() {
                "Releasing the connection before another transfer can start…"
            } else {
                "Preparing, connecting, or waiting for a sender…"
            });
        }
        if self.details {
            body = body
                .child(
                    self.rates
                        .description()
                        .unwrap_or_else(|| "Waiting for payload progress".into()),
                )
                .child(
                    uniform_list(
                        "logs",
                        self.logs.len(),
                        cx.processor(|this, range: std::ops::Range<usize>, _, _| {
                            range
                                .map(|i| div().h(px(30.)).text_sm().child(this.logs[i].clone()))
                                .collect::<Vec<_>>()
                        }),
                    )
                    .h(px(180.)),
                )
                .child(
                    self.button("peer-release", "Align versions / download releases")
                        .on_click(|_, _, cx| cx.open_url(RELEASES)),
                );
        }
        body
    }
    pub(super) fn render_ui(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        self.theme = theme::Theme::new(window.appearance());
        let width = window.viewport_size().width;
        let compact = width < px(1000.);
        let wide = width >= px(1080.);
        let mut body = div()
            .w_full()
            .flex()
            .flex_col()
            .gap_5()
            .child(self.header(compact, cx));
        match self.view {
            View::Settings | View::Trusted => {
                body = body.child(self.settings(cx));
                if self.trust.is_some() {
                    body = body.child(self.feedback(cx));
                }
            }
            View::Transfers => body = body.child(self.feedback(cx)),
            View::Workflow => body = body.child(self.form(wide, cx)).child(self.feedback(cx)),
        }
        if let Some(error) = &self.error {
            body = body.child(
                div()
                    .p_3()
                    .rounded_lg()
                    .bg(self.color(0x422631))
                    .text_color(self.color(0xffbacb))
                    .child(error.clone()),
            );
        }
        div()
            .id("desktop")
            .key_context("Desktop")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::menu_settings))
            .on_action(cx.listener(Self::menu_about))
            .on_action(cx.listener(Self::menu_file))
            .on_action(cx.listener(Self::menu_folder))
            .on_action(cx.listener(Self::menu_close))
            .on_action(|_: &menus::Minimize, window, _| window.minimize_window())
            .on_action(|_: &menus::Zoom, window, _| window.zoom_window())
            .on_action(|_: &menus::Fullscreen, window, _| window.toggle_fullscreen())
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::next_field))
            .on_action(cx.listener(Self::previous_field))
            .size_full()
            .bg(self.color(0x0e141c))
            .text_color(self.color(0xe6edf5))
            .font_family(if cfg!(target_os = "macos") {
                ".SystemUIFont"
            } else {
                "sans-serif"
            })
            .text_size(px(14.))
            .flex()
            .child(self.sidebar(width < px(720.), cx))
            .child(
                div()
                    .id("content")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scroll()
                    .p_5()
                    .child(body),
            )
    }
}
