mod add_llm_provider_modal;
pub mod configure_context_server_modal;
mod configure_context_server_tools_modal;
mod manage_profiles_modal;
mod tool_picker;

use std::{ops::Range, rc::Rc, sync::Arc};

use agent::ContextServerRegistry;
use anyhow::Result;
use cloud_api_types::Plan;
use collections::HashMap;
use context_server::ContextServerId;
use editor::{Editor, MultiBufferOffset, SelectionEffects, scroll::Autoscroll};
use extension::ExtensionManifest;
use extension_host::ExtensionStore;
use fs::Fs;
use gpui::{
    Action, Anchor, AnyView, App, AsyncWindowContext, Entity, EventEmitter, FocusHandle, Focusable,
    ScrollHandle, Subscription, Task, TaskExt, WeakEntity,
};
use itertools::Itertools;
use language::LanguageRegistry;
use language_model::{
    IconOrSvg, LanguageModelProvider, LanguageModelProviderId, LanguageModelRegistry,
    MOMOR_CLOUD_PROVIDER_ID,
};
use language_models::AllLanguageModelSettings;
use notifications::status_toast::StatusToast;
use project::{
    agent_server_store::{AgentId, AgentServerStore, ExternalAgentSource},
    context_server_store::{ContextServerConfiguration, ContextServerStatus, ContextServerStore},
};
use settings::{Settings, SettingsStore, update_settings_file};
use ui::{
    AiSettingItem, AiSettingItemSource, AiSettingItemStatus, ButtonStyle, Chip, ContextMenu,
    ContextMenuEntry, Disclosure, Divider, DividerColor, ElevationIndex, LabelSize, PopoverMenu,
    Switch, TintColor, Tooltip, WithScrollbar, prelude::*,
};
use util::ResultExt as _;
use workspace::{Workspace, create_and_open_local_file};
use momor_actions::{ExtensionCategoryFilter, OpenBrowser};

pub(crate) use configure_context_server_modal::ConfigureContextServerModal;
pub(crate) use configure_context_server_tools_modal::ConfigureContextServerToolsModal;
pub(crate) use manage_profiles_modal::ManageProfilesModal;

use crate::{
    Agent,
    agent_configuration::add_llm_provider_modal::{AddLlmProviderModal, LlmCompatibleProvider},
    agent_connection_store::{AgentConnectionStatus, AgentConnectionStore},
};

pub struct AgentConfiguration {
    fs: Arc<dyn Fs>,
    language_registry: Arc<LanguageRegistry>,
    agent_server_store: Entity<AgentServerStore>,
    agent_connection_store: Entity<AgentConnectionStore>,
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    configuration_views_by_provider: HashMap<LanguageModelProviderId, AnyView>,
    context_server_store: Entity<ContextServerStore>,
    expanded_provider_configurations: HashMap<LanguageModelProviderId, bool>,
    context_server_registry: Entity<ContextServerRegistry>,
    _subscriptions: Vec<Subscription>,
    scroll_handle: ScrollHandle,
    // ponytail: STT â€” LISTA de campos de chave por provedor (chaves reserva p/ failover)
    stt_inputs: Vec<(stt::SttProviderKind, Vec<Entity<ui_input::InputField>>)>,
    stt_expanded: HashMap<&'static str, bool>,
}

impl AgentConfiguration {
    pub fn new(
        fs: Arc<dyn Fs>,
        agent_server_store: Entity<AgentServerStore>,
        agent_connection_store: Entity<AgentConnectionStore>,
        context_server_store: Entity<ContextServerStore>,
        context_server_registry: Entity<ContextServerRegistry>,
        language_registry: Arc<LanguageRegistry>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();

        let subscriptions = vec![
            cx.subscribe_in(
                &LanguageModelRegistry::global(cx),
                window,
                |this, _, event: &language_model::Event, window, cx| match event {
                    language_model::Event::AddedProvider(provider_id) => {
                        let provider = LanguageModelRegistry::read_global(cx).provider(provider_id);
                        if let Some(provider) = provider {
                            this.add_provider_configuration_view(&provider, window, cx);
                        }
                    }
                    language_model::Event::RemovedProvider(provider_id) => {
                        this.remove_provider_configuration_view(provider_id);
                    }
                    _ => {}
                },
            ),
            cx.subscribe(&agent_server_store, |_, _, _, cx| cx.notify()),
            cx.observe(&agent_connection_store, |_, _, cx| cx.notify()),
            cx.subscribe(&context_server_store, |_, _, _, cx| cx.notify()),
        ];

        let stt_inputs = stt::SttProviderKind::ALL
            .into_iter()
            .map(|kind| {
                let keys = stt::provider_keys(kind);
                let mut fields = Vec::new();
                if keys.is_empty() {
                    fields.push(cx.new(|cx| ui_input::InputField::new(window, cx, "API key")));
                } else {
                    for k in &keys {
                        let k = k.clone();
                        fields.push(cx.new(|cx| {
                            let input = ui_input::InputField::new(window, cx, "API key");
                            input.set_text(&k, window, cx);
                            input
                        }));
                    }
                }
                (kind, fields)
            })
            .collect();

        let mut this = Self {
            fs,
            language_registry,
            workspace,
            focus_handle,
            configuration_views_by_provider: HashMap::default(),
            agent_server_store,
            agent_connection_store,
            context_server_store,
            expanded_provider_configurations: HashMap::default(),
            context_server_registry,
            _subscriptions: subscriptions,
            scroll_handle: ScrollHandle::new(),
            stt_inputs,
            stt_expanded: HashMap::default(),
        };

        this.build_provider_configuration_views(window, cx);
        this
    }

    fn build_provider_configuration_views(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let providers = LanguageModelRegistry::read_global(cx).visible_providers();
        for provider in providers {
            self.add_provider_configuration_view(&provider, window, cx);
        }
    }

    fn remove_provider_configuration_view(&mut self, provider_id: &LanguageModelProviderId) {
        self.configuration_views_by_provider.remove(provider_id);
        self.expanded_provider_configurations.remove(provider_id);
    }

    fn add_provider_configuration_view(
        &mut self,
        provider: &Arc<dyn LanguageModelProvider>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let configuration_view = provider.configuration_view(
            language_model::ConfigurationViewTargetAgent::MomorAgent,
            window,
            cx,
        );
        self.configuration_views_by_provider
            .insert(provider.id(), configuration_view);
    }
}

impl Focusable for AgentConfiguration {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

pub enum AssistantConfigurationEvent {
    NewThread(Arc<dyn LanguageModelProvider>),
}

impl EventEmitter<AssistantConfigurationEvent> for AgentConfiguration {}

enum AgentIcon {
    Name(IconName),
    Path(SharedString),
}

impl AgentConfiguration {
    // ponytail: STT â€” MESMO componente visual dos LLM Providers: linhas com disclosure
    // expansÃ­vel, âœ“ verde no ativo, chave dentro ao expandir.
    fn render_stt_section(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = stt::selected_provider();
        let rows: Vec<_> = self.stt_inputs.clone();
        v_flex()
            .border_t_1()
            .border_color(cx.theme().colors().border)
            .child(self.render_section_title(
                "Speech-to-Text (STT)",
                "OuÃ§a o microfone e o Ã¡udio do PC; a transcriÃ§Ã£o vira contexto pra IA.",
                div().into_any_element(),
            ))
            .children(
                rows.into_iter()
                    .map(|(kind, inputs)| self.render_stt_provider_block(kind, inputs, selected, cx)),
            )
    }

    fn render_stt_provider_block(
        &mut self,
        kind: stt::SttProviderKind,
        inputs: Vec<Entity<ui_input::InputField>>,
        selected: stt::SttProviderKind,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let id = kind.id();
        let row_id = SharedString::from(format!("stt-provider-disclosure-{id}"));
        let is_expanded = self.stt_expanded.get(id).copied().unwrap_or(false);
        let has_key = stt::provider_key(kind).is_some();
        let is_selected = kind == selected;

        v_flex()
            .min_w_0()
            .w_full()
            .when(is_expanded, |this| this.mb_2())
            .child(
                div()
                    .px_2()
                    .child(Divider::horizontal().color(DividerColor::BorderFaded)),
            )
            .child(
                h_flex()
                    .map(|this| {
                        if is_expanded {
                            this.mt_2().mb_1()
                        } else {
                            this.my_2()
                        }
                    })
                    .w_full()
                    .child(
                        h_flex()
                            .id(row_id.clone())
                            .px_2()
                            .py_0p5()
                            .w_full()
                            .justify_between()
                            .rounded_sm()
                            .hover(|hover| hover.bg(cx.theme().colors().element_hover))
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_1p5()
                                    .child(
                                        Icon::new(IconName::Mic)
                                            .size(IconSize::Small)
                                            .color(Color::Muted),
                                    )
                                    .child(Label::new(kind.name()))
                                    .when(!kind.is_streaming(), |el| {
                                        el.child(
                                            Label::new("em breve")
                                                .size(LabelSize::XSmall)
                                                .color(Color::Muted),
                                        )
                                    })
                                    .when(has_key && is_selected && !is_expanded, |el| {
                                        el.child(Icon::new(IconName::Check).color(Color::Success))
                                    }),
                            )
                            .child(
                                Disclosure::new(row_id, is_expanded)
                                    .opened_icon(IconName::ChevronUp)
                                    .closed_icon(IconName::ChevronDown),
                            )
                            .on_click(cx.listener(move |this, _, _, _| {
                                let e = this.stt_expanded.entry(id).or_insert(false);
                                *e = !*e;
                            })),
                    ),
            )
            .when(is_expanded, |parent| {
                let n = inputs.len();
                parent.child(
                    v_flex()
                        .min_w_0()
                        .w_full()
                        .px_2()
                        .gap_2()
                        // Um campo por chave. A 1Âª Ã© a principal; as demais sÃ£o reserva
                        // (o failover em runtime tenta na ordem). Ã— remove a chave.
                        .children(inputs.iter().enumerate().map(|(i, field)| {
                            h_flex()
                                .w_full()
                                .gap_1()
                                .items_center()
                                .child(div().flex_1().min_w_0().child(field.clone()))
                                .when(n > 1, |el| {
                                    el.child(
                                        IconButton::new(
                                            SharedString::from(format!("stt-rmkey-{id}-{i}")),
                                            IconName::Trash,
                                        )
                                        .icon_size(IconSize::Small)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.remove_stt_key(kind, i, cx);
                                        })),
                                    )
                                })
                        }))
                        .child(
                            Button::new(
                                SharedString::from(format!("stt-addkey-{id}")),
                                "+ Adicionar chave reserva",
                            )
                            .style(ButtonStyle::Subtle)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.add_stt_key(kind, window, cx);
                            })),
                        )
                        .child(
                            h_flex()
                                .gap_1()
                                .justify_end()
                                .child(
                                    Button::new(
                                        SharedString::from(format!("stt-select-{id}")),
                                        if is_selected { "Ativo" } else { "Usar este" },
                                    )
                                    .style(if is_selected {
                                        ButtonStyle::Tinted(TintColor::Accent)
                                    } else {
                                        ButtonStyle::Subtle
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        stt::set_selected_provider(kind, cx);
                                        this.notify_stt_saved("Provedor STT selecionado.", cx);
                                    })),
                                )
                                .child(
                                    Button::new(
                                        SharedString::from(format!("stt-save-{id}")),
                                        "Salvar chaves",
                                    )
                                    .style(ButtonStyle::Filled)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.save_stt_keys(kind, cx);
                                    })),
                                ),
                        ),
                )
            })
    }

    /// Adiciona um campo de chave reserva vazio ao provedor.
    fn add_stt_key(
        &mut self,
        kind: stt::SttProviderKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let field = cx.new(|cx| ui_input::InputField::new(window, cx, "API key"));
        if let Some((_, fields)) = self.stt_inputs.iter_mut().find(|(k, _)| *k == kind) {
            fields.push(field);
        }
        cx.notify();
    }

    /// Remove o campo de chave `index` (mantÃ©m pelo menos 1) e persiste.
    fn remove_stt_key(
        &mut self,
        kind: stt::SttProviderKind,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        if let Some((_, fields)) = self.stt_inputs.iter_mut().find(|(k, _)| *k == kind)
            && fields.len() > 1
            && index < fields.len()
        {
            fields.remove(index);
        }
        self.save_stt_keys(kind, cx);
        cx.notify();
    }

    /// Junta todas as chaves nÃ£o-vazias do provedor (vÃ­rgula) e salva no KVP.
    fn save_stt_keys(&mut self, kind: stt::SttProviderKind, cx: &mut Context<Self>) {
        let joined = self
            .stt_inputs
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, fields)| {
                fields
                    .iter()
                    .map(|f| f.read(cx).text(cx).trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        stt::set_provider_key(kind, joined, cx);
        self.notify_stt_saved("Chaves salvas.", cx);
    }

    fn notify_stt_saved(&self, msg: &'static str, cx: &mut Context<Self>) {
        if let Some(workspace) = self.workspace.upgrade() {
            workspace.update(cx, |workspace, cx| {
                let status = StatusToast::new(msg, cx, |this, _| this);
                workspace.toggle_status_toast(status, cx);
            });
        }
    }

    fn render_section_title(
        &mut self,
        title: impl Into<SharedString>,
        description: impl Into<SharedString>,
        menu: AnyElement,
    ) -> impl IntoElement {
        h_flex()
            .p_4()
            .pb_0()
            .mb_2p5()
            .items_start()
            .justify_between()
            .child(
                v_flex()
                    .w_full()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .pr_1()
                            .w_full()
                            .gap_2()
                            .justify_between()
                            .flex_wrap()
                            .child(Headline::new(title.into()))
                            .child(menu),
                    )
                    .child(Label::new(description.into()).color(Color::Muted)),
            )
    }

    fn render_provider_configuration_block(
        &mut self,
        provider: &Arc<dyn LanguageModelProvider>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let provider_id = provider.id().0;
        let provider_name = provider.name().0;
        let provider_id_string = SharedString::from(format!("provider-disclosure-{provider_id}"));

        let configuration_view = self
            .configuration_views_by_provider
            .get(&provider.id())
            .cloned();

        let is_expanded = self
            .expanded_provider_configurations
            .get(&provider.id())
            .copied()
            .unwrap_or(false);

        let is_momor_provider = provider.id() == MOMOR_CLOUD_PROVIDER_ID;
        let current_plan = if is_momor_provider {
            self.workspace
                .upgrade()
                .and_then(|workspace| workspace.read(cx).user_store().read(cx).plan())
        } else {
            None
        };

        let is_signed_in = self
            .workspace
            .read_with(cx, |workspace, _| {
                !workspace.client().status().borrow().is_signed_out()
            })
            .unwrap_or(false);

        v_flex()
            .min_w_0()
            .w_full()
            .when(is_expanded, |this| this.mb_2())
            .child(
                div()
                    .px_2()
                    .child(Divider::horizontal().color(DividerColor::BorderFaded)),
            )
            .child(
                h_flex()
                    .map(|this| {
                        if is_expanded {
                            this.mt_2().mb_1()
                        } else {
                            this.my_2()
                        }
                    })
                    .w_full()
                    .justify_between()
                    .child(
                        h_flex()
                            .id(provider_id_string.clone())
                            .px_2()
                            .py_0p5()
                            .w_full()
                            .justify_between()
                            .rounded_sm()
                            .hover(|hover| hover.bg(cx.theme().colors().element_hover))
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_1p5()
                                    .child(
                                        match provider.icon() {
                                            IconOrSvg::Svg(path) => Icon::from_external_svg(path),
                                            IconOrSvg::Icon(name) => Icon::new(name),
                                        }
                                        .size(IconSize::Small)
                                        .color(Color::Muted),
                                    )
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .gap_1()
                                            .child(Label::new(provider_name.clone()))
                                            .map(|this| {
                                                if is_momor_provider && is_signed_in {
                                                    this.child(
                                                        self.render_momor_plan_info(current_plan, cx),
                                                    )
                                                } else {
                                                    this.when(
                                                        provider.is_authenticated(cx)
                                                            && !is_expanded,
                                                        |parent| {
                                                            parent.child(
                                                                Icon::new(IconName::Check)
                                                                    .color(Color::Success),
                                                            )
                                                        },
                                                    )
                                                }
                                            }),
                                    ),
                            )
                            .child(
                                Disclosure::new(provider_id_string, is_expanded)
                                    .opened_icon(IconName::ChevronUp)
                                    .closed_icon(IconName::ChevronDown),
                            )
                            .on_click(cx.listener({
                                let provider_id = provider.id();
                                move |this, _event, _window, _cx| {
                                    let is_expanded = this
                                        .expanded_provider_configurations
                                        .entry(provider_id.clone())
                                        .or_insert(false);

                                    *is_expanded = !*is_expanded;
                                }
                            })),
                    ),
            )
            .child(
                v_flex()
                    .min_w_0()
                    .w_full()
                    .px_2()
                    .gap_1()
                    .when(is_expanded, |parent| match configuration_view {
                        Some(configuration_view) => parent.child(configuration_view),
                        None => parent.child(Label::new(format!(
                            "No configuration view for {provider_name}",
                        ))),
                    })
                    .when(is_expanded && provider.is_authenticated(cx), |parent| {
                        parent.child(
                            Button::new(
                                SharedString::from(format!("new-thread-{provider_id}")),
                                "Start New Thread",
                            )
                            .full_width()
                            .style(ButtonStyle::Outlined)
                            .layer(ElevationIndex::ModalSurface)
                            .start_icon(
                                Icon::new(IconName::Thread)
                                    .size(IconSize::Small)
                                    .color(Color::Muted),
                            )
                            .label_size(LabelSize::Small)
                            .on_click(cx.listener({
                                let provider = provider.clone();
                                move |_this, _event, _window, cx| {
                                    cx.emit(AssistantConfigurationEvent::NewThread(
                                        provider.clone(),
                                    ))
                                }
                            })),
                        )
                    })
                    .when(
                        is_expanded && is_removable_provider(&provider.id(), cx),
                        |this| {
                            this.child(
                                Button::new(
                                    SharedString::from(format!("delete-provider-{provider_id}")),
                                    "Remove Provider",
                                )
                                .full_width()
                                .style(ButtonStyle::Outlined)
                                .start_icon(
                                    Icon::new(IconName::Trash)
                                        .size(IconSize::Small)
                                        .color(Color::Muted),
                                )
                                .label_size(LabelSize::Small)
                                .on_click(cx.listener({
                                    let provider = provider.clone();
                                    move |this, _event, window, cx| {
                                        this.delete_provider(provider.clone(), window, cx);
                                    }
                                })),
                            )
                        },
                    ),
            )
    }

    fn delete_provider(
        &mut self,
        provider: Arc<dyn LanguageModelProvider>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let fs = self.fs.clone();
        let provider_id = provider.id();

        cx.spawn_in(window, async move |_, cx| {
            cx.update(|_window, cx| {
                update_settings_file(fs.clone(), cx, {
                    let provider_id = provider_id.clone();
                    move |settings, _| {
                        if let Some(ref mut openai_compatible) = settings
                            .language_models
                            .as_mut()
                            .and_then(|lm| lm.openai_compatible.as_mut())
                        {
                            let key_to_remove: Arc<str> = Arc::from(provider_id.0.as_ref());
                            openai_compatible.remove(&key_to_remove);
                        }
                    }
                });
            })
            .log_err();

            cx.update(|_window, cx| {
                LanguageModelRegistry::global(cx).update(cx, {
                    let provider_id = provider_id.clone();
                    move |registry, cx| {
                        registry.unregister_provider(provider_id, cx);
                    }
                })
            })
            .log_err();

            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn render_provider_configuration_section(
        &mut self,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let providers = LanguageModelRegistry::read_global(cx).visible_providers();

        let popover_menu = PopoverMenu::new("add-provider-popover")
            .trigger(
                Button::new("add-provider", "Add Provider")
                    .style(ButtonStyle::Outlined)
                    .start_icon(
                        Icon::new(IconName::Plus)
                            .size(IconSize::Small)
                            .color(Color::Muted),
                    )
                    .label_size(LabelSize::Small),
            )
            .menu({
                let workspace = self.workspace.clone();
                move |window, cx| {
                    Some(ContextMenu::build(window, cx, |menu, _window, _cx| {
                        menu.header("Compatible APIs").entry("OpenAI", None, {
                            let workspace = workspace.clone();
                            move |window, cx| {
                                workspace
                                    .update(cx, |workspace, cx| {
                                        AddLlmProviderModal::toggle(
                                            LlmCompatibleProvider::OpenAi,
                                            workspace,
                                            window,
                                            cx,
                                        );
                                    })
                                    .log_err();
                            }
                        })
                    }))
                }
            })
            .anchor(gpui::Anchor::TopRight)
            .offset(gpui::Point {
                x: px(0.0),
                y: px(2.0),
            });

        v_flex()
            .min_w_0()
            .w_full()
            .child(self.render_section_title(
                "LLM Providers",
                "Add at least one provider to use AI-powered features with Momor's native agent.",
                popover_menu.into_any_element(),
            ))
            .child(
                div()
                    .w_full()
                    .pl(DynamicSpacing::Base08.rems(cx))
                    .pr(DynamicSpacing::Base20.rems(cx))
                    .children(
                        providers.into_iter().map(|provider| {
                            self.render_provider_configuration_block(&provider, cx)
                        }),
                    ),
            )
    }

    fn render_momor_plan_info(&self, plan: Option<Plan>, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(plan) = plan {
            let free_chip_bg = cx
                .theme()
                .colors()
                .editor_background
                .opacity(0.5)
                .blend(cx.theme().colors().text_accent.opacity(0.05));

            let pro_chip_bg = cx
                .theme()
                .colors()
                .editor_background
                .opacity(0.5)
                .blend(cx.theme().colors().text_accent.opacity(0.2));

            let (plan_name, label_color, bg_color) = match plan {
                Plan::MomorFree => ("Free", Color::Default, free_chip_bg),
                Plan::MomorProTrial => ("Pro Trial", Color::Accent, pro_chip_bg),
                Plan::MomorPro => ("Pro", Color::Accent, pro_chip_bg),
                Plan::MomorBusiness => ("Business", Color::Accent, pro_chip_bg),
                Plan::MomorStudent => ("Student", Color::Accent, pro_chip_bg),
            };

            Chip::new(plan_name.to_string())
                .bg_color(bg_color)
                .label_color(label_color)
                .into_any_element()
        } else {
            div().into_any_element()
        }
    }

    fn render_agent_servers_section(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let agent_server_store = self.agent_server_store.read(cx);

        let agents = agent_server_store
            .external_agents()
            .cloned()
            .collect::<Vec<_>>();

        let agents: Vec<_> = agents
            .into_iter()
            .map(|name| {
                let icon = if let Some(icon_path) = agent_server_store.agent_icon(&name) {
                    AgentIcon::Path(icon_path)
                } else {
                    AgentIcon::Name(IconName::Sparkle)
                };
                let display_name = agent_server_store
                    .agent_display_name(&name)
                    .unwrap_or_else(|| name.0.clone());
                let source = agent_server_store.agent_source(&name).unwrap_or_default();
                (name, icon, display_name, source)
            })
            .sorted_unstable_by_key(|(_, _, display_name, _)| display_name.to_lowercase())
            .collect();

        let add_agent_popover = PopoverMenu::new("add-agent-server-popover")
            .trigger(
                Button::new("add-agent", "Add Agent")
                    .style(ButtonStyle::Outlined)
                    .start_icon(
                        Icon::new(IconName::Plus)
                            .size(IconSize::Small)
                            .color(Color::Muted),
                    )
                    .label_size(LabelSize::Small),
            )
            .menu({
                move |window, cx| {
                    Some(ContextMenu::build(window, cx, |menu, _window, _cx| {
                        menu.entry("Install from Registry", None, {
                            |window, cx| {
                                window.dispatch_action(Box::new(momor_actions::AcpRegistry), cx)
                            }
                        })
                        .entry("Add Custom Agent", None, {
                            move |window, cx| {
                                if let Some(workspace) = Workspace::for_window(window, cx) {
                                    let workspace = workspace.downgrade();
                                    window
                                        .spawn(cx, async |cx| {
                                            open_new_agent_servers_entry_in_settings_editor(
                                                workspace, cx,
                                            )
                                            .await
                                        })
                                        .detach_and_log_err(cx);
                                }
                            }
                        })
                        .separator()
                        .header("Learn More")
                        .item(
                            ContextMenuEntry::new("ACP Docs")
                                .icon(IconName::ArrowUpRight)
                                .icon_color(Color::Muted)
                                .icon_position(IconPosition::End)
                                .handler({
                                    move |window, cx| {
                                        window.dispatch_action(
                                            Box::new(OpenBrowser {
                                                url: "https://agentclientprotocol.com/".into(),
                                            }),
                                            cx,
                                        );
                                    }
                                }),
                        )
                    }))
                }
            })
            .anchor(gpui::Anchor::TopRight)
            .offset(gpui::Point {
                x: px(0.0),
                y: px(2.0),
            });

        v_flex()
            .min_w_0()
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .child(
                v_flex()
                    .child(self.render_section_title(
                        "External Agents",
                        "All agents connected through the Agent Client Protocol.",
                        add_agent_popover.into_any_element(),
                    ))
                    .child(
                        v_flex()
                            .p_4()
                            .pt_0()
                            .gap_2()
                            .children(Itertools::intersperse_with(
                                agents
                                    .into_iter()
                                    .map(|(name, icon, display_name, source)| {
                                        self.render_agent_server(
                                            icon,
                                            name,
                                            display_name,
                                            source,
                                            cx,
                                        )
                                        .into_any_element()
                                    }),
                                || {
                                    Divider::horizontal()
                                        .color(DividerColor::BorderFaded)
                                        .into_any_element()
                                },
                            )),
                    ),
            )
    }

    fn render_agent_server(
        &self,
        icon: AgentIcon,
        id: impl Into<SharedString>,
        display_name: impl Into<SharedString>,
        source: ExternalAgentSource,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = id.into();
        let display_name = display_name.into();

        let icon = match icon {
            AgentIcon::Name(icon_name) => Icon::new(icon_name)
                .size(IconSize::Small)
                .color(Color::Muted),
            AgentIcon::Path(icon_path) => Icon::from_external_svg(icon_path)
                .size(IconSize::Small)
                .color(Color::Muted),
        };

        let source_kind = match source {
            ExternalAgentSource::Extension => AiSettingItemSource::Extension,
            ExternalAgentSource::Registry => AiSettingItemSource::Registry,
            ExternalAgentSource::Custom => AiSettingItemSource::Custom,
        };

        let agent_server_name = AgentId(id.clone());
        let agent = Agent::Custom {
            id: agent_server_name.clone(),
        };

        let (connection_status, running_version) = {
            let connection_store = self.agent_connection_store.read(cx);
            (
                connection_store.connection_status(&agent, cx),
                connection_store.agent_version(&agent, cx),
            )
        };

        let restart_button = matches!(
            connection_status,
            AgentConnectionStatus::Connected | AgentConnectionStatus::Connecting
        )
        .then(|| {
            IconButton::new(
                SharedString::from(format!("restart-{}", id)),
                IconName::RotateCw,
            )
            .disabled(connection_status == AgentConnectionStatus::Connecting)
            .icon_color(Color::Muted)
            .icon_size(IconSize::Small)
            .tooltip(Tooltip::text("Restart Agent Connection"))
            .on_click(cx.listener({
                let agent = agent.clone();
                move |this, _, _window, cx| {
                    let server: Rc<dyn agent_servers::AgentServer> =
                        Rc::new(agent_servers::CustomAgentServer::new(agent.id()));
                    this.agent_connection_store.update(cx, |store, cx| {
                        store.restart_connection(agent.clone(), server, cx);
                    });
                }
            }))
        });

        let uninstall_button = match source {
            ExternalAgentSource::Extension => Some(
                IconButton::new(
                    SharedString::from(format!("uninstall-{}", id)),
                    IconName::Trash,
                )
                .icon_color(Color::Muted)
                .icon_size(IconSize::Small)
                .tooltip(Tooltip::text("Uninstall Agent Extension"))
                .on_click(cx.listener(move |this, _, _window, cx| {
                    let agent_name = agent_server_name.clone();

                    if let Some(ext_id) = this.agent_server_store.update(cx, |store, _cx| {
                        store.get_extension_id_for_agent(&agent_name)
                    }) {
                        ExtensionStore::global(cx)
                            .update(cx, |store, cx| store.uninstall_extension(ext_id, cx))
                            .detach_and_log_err(cx);
                    }
                })),
            ),
            ExternalAgentSource::Registry => {
                let fs = self.fs.clone();
                Some(
                    IconButton::new(
                        SharedString::from(format!("uninstall-{}", id)),
                        IconName::Trash,
                    )
                    .icon_color(Color::Muted)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text("Remove Registry Agent"))
                    .on_click(cx.listener(move |_, _, _window, cx| {
                        let agent_name = agent_server_name.clone();
                        update_settings_file(fs.clone(), cx, move |settings, _| {
                            let Some(agent_servers) = settings.agent_servers.as_mut() else {
                                return;
                            };
                            if let Some(entry) = agent_servers.get(agent_name.0.as_ref())
                                && matches!(
                                    entry,
                                    settings::CustomAgentServerSettings::Registry { .. }
                                )
                            {
                                agent_servers.remove(agent_name.0.as_ref());
                            }
                        });
                    })),
                )
            }
            ExternalAgentSource::Custom => {
                let fs = self.fs.clone();
                Some(
                    IconButton::new(
                        SharedString::from(format!("uninstall-{}", id)),
                        IconName::Trash,
                    )
                    .icon_color(Color::Muted)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text("Remove Custom Agent"))
                    .on_click(cx.listener(move |_, _, _window, cx| {
                        let agent_name = agent_server_name.clone();
                        update_settings_file(fs.clone(), cx, move |settings, _| {
                            let Some(agent_servers) = settings.agent_servers.as_mut() else {
                                return;
                            };
                            if let Some(entry) = agent_servers.get(agent_name.0.as_ref())
                                && matches!(
                                    entry,
                                    settings::CustomAgentServerSettings::Custom { .. }
                                )
                            {
                                agent_servers.remove(agent_name.0.as_ref());
                            }
                        });
                    })),
                )
            }
        };

        let status = match connection_status {
            AgentConnectionStatus::Disconnected => AiSettingItemStatus::Stopped,
            AgentConnectionStatus::Connecting => AiSettingItemStatus::Starting,
            AgentConnectionStatus::Connected => AiSettingItemStatus::Running,
        };

        AiSettingItem::new(id, display_name, status, source_kind)
            .icon(icon)
            .when_some(running_version, |this, version| this.detail_label(version))
            .when_some(restart_button, |this, button| this.action(button))
            .when_some(uninstall_button, |this, button| this.action(button))
    }
}

impl Render for AgentConfiguration {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("assistant-configuration")
            .key_context("AgentConfiguration")
            .track_focus(&self.focus_handle(cx))
            .relative()
            .size_full()
            .pb_8()
            .bg(cx.theme().colors().panel_background)
            .child(
                div()
                    .size_full()
                    .child(
                        v_flex()
                            .id("assistant-configuration-content")
                            .track_scroll(&self.scroll_handle)
                            .size_full()
                            .min_w_0()
                            .overflow_y_scroll()
                            // ponytail: seção de MCP Servers removida — os MCPs vivem na sidebar
                            .child(self.render_agent_servers_section(cx))
                            .child(self.render_stt_section(cx))
                            .child(self.render_provider_configuration_section(cx)),
                    )
                    .vertical_scrollbar_for(&self.scroll_handle, window, cx),
            )
    }
}

fn extension_only_provides_context_server(manifest: &ExtensionManifest) -> bool {
    manifest.context_servers.len() == 1
        && manifest.themes.is_empty()
        && manifest.icon_themes.is_empty()
        && manifest.languages.is_empty()
        && manifest.grammars.is_empty()
        && manifest.language_servers.is_empty()
        && manifest.slash_commands.is_empty()
        && manifest.snippets.is_none()
        && manifest.debug_locators.is_empty()
}

pub(crate) fn resolve_extension_for_context_server(
    id: &ContextServerId,
    cx: &App,
) -> Option<(Arc<str>, Arc<ExtensionManifest>)> {
    ExtensionStore::global(cx)
        .read(cx)
        .installed_extensions()
        .iter()
        .find(|(_, entry)| entry.manifest.context_servers.contains_key(&id.0))
        .map(|(id, entry)| (id.clone(), entry.manifest.clone()))
}

// This notification appears when trying to delete
// an MCP server extension that not only provides
// the server, but other things, too, like language servers and more.
fn show_unable_to_uninstall_extension_with_context_server(
    workspace: &mut Workspace,
    id: ContextServerId,
    cx: &mut App,
) {
    let workspace_handle = workspace.weak_handle();
    let context_server_id = id.clone();

    let status_toast = StatusToast::new(
        format!(
            "The {} extension provides more than just the MCP server. Proceed to uninstall anyway?",
            id.0
        ),
        cx,
        move |this, _cx| {
            let workspace_handle = workspace_handle.clone();

            this.icon(
                Icon::new(IconName::Warning)
                    .size(IconSize::Small)
                    .color(Color::Warning),
            )
            .dismiss_button(true)
            .action("Uninstall", move |_, _cx| {
                if let Some((extension_id, _)) =
                    resolve_extension_for_context_server(&context_server_id, _cx)
                {
                    ExtensionStore::global(_cx).update(_cx, |store, cx| {
                        store
                            .uninstall_extension(extension_id, cx)
                            .detach_and_log_err(cx);
                    });

                    workspace_handle
                        .update(_cx, |workspace, cx| {
                            let fs = workspace.app_state().fs.clone();
                            cx.spawn({
                                let context_server_id = context_server_id.clone();
                                async move |_workspace_handle, cx| {
                                    cx.update(|cx| {
                                        update_settings_file(fs, cx, move |settings, _| {
                                            settings
                                                .project
                                                .context_servers
                                                .remove(&context_server_id.0);
                                        });
                                    });
                                    anyhow::Ok(())
                                }
                            })
                            .detach_and_log_err(cx);
                        })
                        .log_err();
                }
            })
        },
    );

    workspace.toggle_status_toast(status_toast, cx);
}

async fn open_new_agent_servers_entry_in_settings_editor(
    workspace: WeakEntity<Workspace>,
    cx: &mut AsyncWindowContext,
) -> Result<()> {
    let settings_editor = workspace
        .update_in(cx, |_, window, cx| {
            create_and_open_local_file(paths::settings_file(), window, cx, || {
                settings::initial_user_settings_content().as_ref().into()
            })
        })?
        .await?
        .downcast::<Editor>()
        .unwrap();

    settings_editor
        .downgrade()
        .update_in(cx, |item, window, cx| {
            let text = item.buffer().read(cx).snapshot(cx).text();

            let settings = cx.global::<SettingsStore>();

            let mut unique_server_name = None;
            let Some(edits) = settings
                .edits_for_update(&text, |settings| {
                    let server_name: Option<String> = (0..u8::MAX)
                        .map(|i| {
                            if i == 0 {
                                "your_agent".to_string()
                            } else {
                                format!("your_agent_{}", i)
                            }
                        })
                        .find(|name| {
                            !settings
                                .agent_servers
                                .as_ref()
                                .is_some_and(|agent_servers| {
                                    agent_servers.contains_key(name.as_str())
                                })
                        });
                    if let Some(server_name) = server_name {
                        unique_server_name = Some(SharedString::from(server_name.clone()));
                        settings.agent_servers.get_or_insert_default().insert(
                            server_name,
                            settings::CustomAgentServerSettings::Custom {
                                path: "path_to_executable".into(),
                                args: vec![],
                                env: HashMap::default(),
                                default_mode: None,
                                default_model: None,
                                favorite_models: vec![],
                                default_config_options: Default::default(),
                                favorite_config_option_values: Default::default(),
                            },
                        );
                    }
                })
                .log_err()
            else {
                return;
            };

            if edits.is_empty() {
                return;
            }

            let ranges = edits
                .iter()
                .map(|(range, _)| range.clone())
                .collect::<Vec<_>>();

            item.edit(
                edits.into_iter().map(|(range, s)| {
                    (
                        MultiBufferOffset(range.start)..MultiBufferOffset(range.end),
                        s,
                    )
                }),
                cx,
            );
            if let Some((unique_server_name, buffer)) =
                unique_server_name.zip(item.buffer().read(cx).as_singleton())
            {
                let snapshot = buffer.read(cx).snapshot();
                if let Some(range) =
                    find_text_in_buffer(&unique_server_name, ranges[0].start, &snapshot)
                {
                    item.change_selections(
                        SelectionEffects::scroll(Autoscroll::newest()),
                        window,
                        cx,
                        |selections| {
                            selections.select_ranges(vec![
                                MultiBufferOffset(range.start)..MultiBufferOffset(range.end),
                            ]);
                        },
                    );
                }
            }
        })
}

fn find_text_in_buffer(
    text: &str,
    start: usize,
    snapshot: &language::BufferSnapshot,
) -> Option<Range<usize>> {
    let chars = text.chars().collect::<Vec<char>>();

    let mut offset = start;
    let mut char_offset = 0;
    for c in snapshot.chars_at(start) {
        if char_offset >= chars.len() {
            break;
        }
        offset += 1;

        if c == chars[char_offset] {
            char_offset += 1;
        } else {
            char_offset = 0;
        }
    }

    if char_offset == chars.len() {
        Some(offset.saturating_sub(chars.len())..offset)
    } else {
        None
    }
}

// OpenAI-compatible providers are user-configured and can be removed,
// whereas built-in providers (like Anthropic, OpenAI, Google, etc.) can't.
//
// If in the future we have more "API-compatible-type" of providers,
// they should be included here as removable providers.
fn is_removable_provider(provider_id: &LanguageModelProviderId, cx: &App) -> bool {
    AllLanguageModelSettings::get_global(cx)
        .openai_compatible
        .contains_key(provider_id.0.as_ref())
}
