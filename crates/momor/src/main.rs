//! Momor — chat-only app on Zed's real stack (gpui + ui + theme + icons).
//! One window, one panel: the chat. Nothing else exists.

mod input;

use std::sync::Arc;

use anyhow::Context as _;
use futures::AsyncReadExt as _;
use gpui::{
    App, Bounds, Context, Entity, FocusHandle, Focusable, KeyBinding, ScrollHandle,
    TitlebarOptions, Window, WindowBounds, WindowOptions, actions, prelude::*, px, size,
};
use gpui_platform::application;
use http_client::{AsyncBody, HttpClient, Method, Request};
use reqwest_client::ReqwestClient;
use theme::{ActiveTheme, LoadThemes};
use ui::prelude::*;
use ui::{IconButton, LoadingLabel};

use crate::input::TextInput;

actions!(momor, [SendPrompt, Quit]);

/// Fixed fonts/sizes in place of the `settings` crate (which no longer exists).
struct MomorThemeSettings {
    ui_font: gpui::Font,
    buffer_font: gpui::Font,
}

impl theme::ThemeSettingsProvider for MomorThemeSettings {
    fn ui_font<'a>(&'a self, _cx: &'a App) -> &'a gpui::Font {
        &self.ui_font
    }

    fn buffer_font<'a>(&'a self, _cx: &'a App) -> &'a gpui::Font {
        &self.buffer_font
    }

    fn ui_font_size(&self, _cx: &App) -> gpui::Pixels {
        px(14.)
    }

    fn buffer_font_size(&self, _cx: &App) -> gpui::Pixels {
        px(13.)
    }

    fn ui_density(&self, _cx: &App) -> theme::UiDensity {
        theme::UiDensity::default()
    }
}

// ponytail: modelo fixo; troque a const (ou exporte MOMOR_MODEL) quando precisar de outro.
const MODEL: &str = "claude-sonnet-4-5";
const PLACEHOLDER: &str = "Message Claude Agent — @ to include context, / for commands";

#[derive(Clone, Copy, PartialEq)]
enum Role {
    User,
    Assistant,
}

struct Message {
    role: Role,
    text: SharedString,
}

struct ChatPanel {
    input: Entity<TextInput>,
    messages: Vec<Message>,
    thinking: bool,
    http: Arc<dyn HttpClient>,
    scroll: ScrollHandle,
}

impl ChatPanel {
    fn new(cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new(cx, PLACEHOLDER));
        let http: Arc<dyn HttpClient> = Arc::new(
            ReqwestClient::user_agent("momor/0.1").unwrap_or_else(|_| ReqwestClient::new()),
        );
        Self {
            input,
            messages: Vec::new(),
            thinking: false,
            http,
            scroll: ScrollHandle::new(),
        }
    }

    fn on_send(&mut self, _: &SendPrompt, _window: &mut Window, cx: &mut Context<Self>) {
        self.send(cx);
    }

    fn send(&mut self, cx: &mut Context<Self>) {
        if self.thinking {
            return;
        }
        let text = self.input.read(cx).content.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.input.update(cx, |input, cx| {
            input.reset();
            cx.notify();
        });
        self.messages.push(Message {
            role: Role::User,
            text: text.into(),
        });
        self.thinking = true;
        self.scroll.scroll_to_bottom();
        cx.notify();

        let history: Vec<serde_json::Value> = self
            .messages
            .iter()
            .map(|message| {
                serde_json::json!({
                    "role": match message.role {
                        Role::User => "user",
                        Role::Assistant => "assistant",
                    },
                    "content": message.text.to_string(),
                })
            })
            .collect();
        let http = self.http.clone();

        cx.spawn(async move |this, cx| {
            let reply = request_claude(http, history)
                .await
                .unwrap_or_else(|error| format!("⚠ {error:#}"));
            this.update(cx, |this, cx| {
                this.thinking = false;
                this.messages.push(Message {
                    role: Role::Assistant,
                    text: reply.into(),
                });
                this.scroll.scroll_to_bottom();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn dropdown(label: &'static str) -> impl IntoElement {
        // ponytail: seletor decorativo (como no print); vire PopoverMenu quando houver opções reais.
        h_flex()
            .gap_0p5()
            .child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
            .child(
                Icon::new(IconName::ChevronDown)
                    .size(IconSize::XSmall)
                    .color(Color::Muted),
            )
    }

    fn render_message(&self, index: usize, message: &Message, cx: &Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        match message.role {
            Role::User => div()
                .id(("msg", index))
                .rounded_md()
                .border_1()
                .border_color(colors.border)
                .bg(colors.editor_background)
                .px_2p5()
                .py_2()
                .child(message.text.clone())
                .into_any_element(),
            Role::Assistant => div()
                .id(("msg", index))
                .px_1()
                .child(message.text.clone())
                .into_any_element(),
        }
    }
}

impl Focusable for ChatPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle.clone()
    }
}

impl Render for ChatPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors().clone();

        let header = h_flex()
            .h(px(36.))
            .px_2()
            .flex_none()
            .justify_between()
            .border_b_1()
            .border_color(colors.border)
            .bg(colors.title_bar_background)
            .child(
                h_flex()
                    .gap_1p5()
                    .child(
                        Icon::new(IconName::Sparkle)
                            .size(IconSize::Small)
                            .color(Color::Accent),
                    )
                    .child(Label::new("@momor — Claude Agent").size(LabelSize::Small)),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(IconButton::new("new-thread", IconName::Plus))
                    .child(IconButton::new("history", IconName::HistoryRerun))
                    .child(IconButton::new("more", IconName::Ellipsis)),
            );

        let thread: AnyElement = if self.messages.is_empty() && !self.thinking {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_2()
                .child(
                    Icon::new(IconName::Sparkle)
                        .size(IconSize::XLarge)
                        .color(Color::Muted),
                )
                .child(
                    Label::new("Converse com o Claude — Enter envia")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .into_any_element()
        } else {
            div()
                .id("thread")
                .flex_1()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .child(
                    v_flex()
                        .gap_3()
                        .p_3()
                        .children(
                            self.messages
                                .iter()
                                .enumerate()
                                .map(|(index, message)| self.render_message(index, message, cx)),
                        )
                        .when(self.thinking, |this| {
                            this.child(LoadingLabel::new("Claude está pensando"))
                        }),
                )
                .into_any_element()
        };

        let composer = v_flex()
            .m_2()
            .flex_none()
            .rounded_lg()
            .border_1()
            .border_color(colors.border)
            .bg(colors.editor_background)
            .child(
                h_flex()
                    .px_2p5()
                    .py_1()
                    .justify_between()
                    .border_b_1()
                    .border_color(colors.border_variant)
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Icon::new(IconName::ChevronRight)
                                    .size(IconSize::XSmall)
                                    .color(Color::Muted),
                            )
                            .child(Label::new("Plan").size(LabelSize::Small).color(Color::Muted)),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Label::new("All Done")
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            )
                            .child(
                                Icon::new(IconName::Close)
                                    .size(IconSize::XSmall)
                                    .color(Color::Muted),
                            ),
                    ),
            )
            .child(h_flex().px_2p5().pt_2().pb_1().child(self.input.clone()))
            .child(
                h_flex()
                    .px_1p5()
                    .py_1()
                    .justify_between()
                    .child(
                        h_flex()
                            .gap_0p5()
                            .child(IconButton::new("attach", IconName::Plus))
                            .child(IconButton::new("settings", IconName::Settings)),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .child(Self::dropdown("Bypass Permissions"))
                            .child(Self::dropdown("Opus"))
                            .child(Self::dropdown("Max"))
                            .child(Self::dropdown("Default"))
                            .child(
                                IconButton::new("send", IconName::Send)
                                    .icon_color(Color::Accent)
                                    .on_click(cx.listener(|this, _, _, cx| this.send(cx))),
                            ),
                    ),
            );

        v_flex()
            .size_full()
            .bg(colors.panel_background)
            .text_color(colors.text)
            .font_family("IBM Plex Sans")
            .text_size(px(14.))
            .on_action(cx.listener(Self::on_send))
            .child(header)
            .child(thread)
            .child(composer)
    }
}

async fn request_claude(
    http: Arc<dyn HttpClient>,
    messages: Vec<serde_json::Value>,
) -> anyhow::Result<String> {
    let key = std::env::var("ANTHROPIC_API_KEY")
        .context("defina ANTHROPIC_API_KEY para respostas reais (modo offline)")?;
    let body = serde_json::json!({
        "model": MODEL,
        "max_tokens": 2048,
        "messages": messages,
    });
    let request = Request::builder()
        .method(Method::POST)
        .uri("https://api.anthropic.com/v1/messages")
        .header("x-api-key", key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .body(AsyncBody::from(serde_json::to_string(&body)?))?;

    let mut response = http.send(request).await?;
    let mut raw = Vec::new();
    response.body_mut().read_to_end(&mut raw).await?;
    let value: serde_json::Value = serde_json::from_slice(&raw)
        .with_context(|| format!("resposta inválida da API ({})", response.status()))?;

    if !response.status().is_success() {
        anyhow::bail!(
            "API {}: {}",
            response.status(),
            value["error"]["message"].as_str().unwrap_or("erro desconhecido")
        );
    }
    Ok(value["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string())
}

fn main() {
    application().with_assets(assets::Assets).run(|cx: &mut App| {
        assets::Assets.load_fonts(cx).ok();
        theme::init(LoadThemes::JustBase, cx);
        theme::set_theme_settings_provider(
            Box::new(MomorThemeSettings {
                ui_font: gpui::font("IBM Plex Sans"),
                buffer_font: gpui::font("Lilex"),
            }),
            cx,
        );

        cx.bind_keys([
            KeyBinding::new("backspace", input::Backspace, Some("TextInput")),
            KeyBinding::new("delete", input::Delete, Some("TextInput")),
            KeyBinding::new("left", input::Left, Some("TextInput")),
            KeyBinding::new("right", input::Right, Some("TextInput")),
            KeyBinding::new("shift-left", input::SelectLeft, Some("TextInput")),
            KeyBinding::new("shift-right", input::SelectRight, Some("TextInput")),
            KeyBinding::new("home", input::Home, Some("TextInput")),
            KeyBinding::new("end", input::End, Some("TextInput")),
            KeyBinding::new("ctrl-a", input::SelectAll, Some("TextInput")),
            KeyBinding::new("ctrl-v", input::Paste, Some("TextInput")),
            KeyBinding::new("ctrl-c", input::Copy, Some("TextInput")),
            KeyBinding::new("ctrl-x", input::Cut, Some("TextInput")),
            KeyBinding::new("enter", SendPrompt, Some("TextInput")),
            KeyBinding::new("ctrl-q", Quit, None),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());

        let bounds = Bounds::centered(None, size(px(520.), px(760.)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("momor".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                |_, cx| cx.new(ChatPanel::new),
            )
            .unwrap();

        window
            .update(cx, |panel, window, cx| {
                window.focus(&panel.focus_handle(cx), cx);
                cx.activate(true);
            })
            .unwrap();
    });
}
