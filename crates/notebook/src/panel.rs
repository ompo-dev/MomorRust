//! Painel lateral "Notion": árvore de pastas → notas (markdown) + reuniões
//! (transcrições). Alterna com o chat pelo ícone de sidebar na titlebar.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use crate::store::{Folder, Meeting, Note, NotebookDb};
use editor::{Editor, EditorEvent};
use gpui::{
    Action, App, Context, Entity, EventEmitter, FocusHandle, Focusable, ScrollHandle, Subscription,
    WeakEntity, Window, actions, prelude::*,
};
use language::LanguageRegistry;
use language::language_settings::SoftWrap;
use project::context_server_store::ContextServerStatus;
use project::project_settings::ProjectSettings;
use settings::Settings as _;
use ui::{Divider, Label, ScrollAxes, Scrollbars, Tooltip, WithScrollbar, prelude::*};
use util::ResultExt as _;
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};

actions!(
    notebook,
    [
        /// Abre/foca o painel de notas e reuniões.
        ToggleFocus
    ]
);

const NOTEBOOK_PANEL_WIDTH: f32 = 640.0;

/// Recarrega o painel a partir do banco. Chamado por ferramentas externas que
/// escrevem no notebook (ex.: a IA criando notas/pastas via a tool `notebook`),
/// pra que a mudança apareça na hora sem depender de interação do usuário.
pub fn refresh_panel(cx: &mut App) {
    let Some(view) = cx
        .try_global::<workspace::NotebookSlot>()
        .and_then(|slot| slot.view.clone())
    else {
        return;
    };
    if let Ok(panel) = view.downcast::<NotebookPanel>() {
        panel.update(cx, |panel, cx| panel.reload(cx));
    }
}

/// Marca um item para abrir no próximo render do painel. Usado por tools da IA, que não têm
/// `Window` para criar o editor imediatamente.
pub fn open_on_next_render(kind: &str, id: String, cx: &mut App) {
    workspace::set_notebook_open(true, cx);
    let val = format!("{kind}:{id}");
    let store = db::kvp::KeyValueStore::global(cx);
    db::write_and_log(cx, move || {
        let val = val.clone();
        async move { store.write_kvp("momor_open_item".into(), val).await }
    });
    let Some(view) = cx
        .try_global::<workspace::NotebookSlot>()
        .and_then(|slot| slot.view.clone())
    else {
        return;
    };
    if let Ok(panel) = view.downcast::<NotebookPanel>() {
        panel.update(cx, |panel, cx| {
            panel.restored = false;
            panel.reload(cx);
        });
    }
}

/// Abre um item (nota/reunião/skill/mcp) no painel a partir de fora — ex.: clicar num badge
/// de citação no chat. Garante a sidebar aberta.
pub fn open_item(kind: &str, id: String, window: &mut Window, cx: &mut App) {
    let Some(view) = cx
        .try_global::<workspace::NotebookSlot>()
        .and_then(|slot| slot.view.clone())
    else {
        return;
    };
    let Ok(panel) = view.downcast::<NotebookPanel>() else {
        return;
    };
    workspace::set_notebook_open(true, cx);
    panel.update(cx, |panel, cx| match kind {
        "note" => panel.select_note(id, window, cx),
        "meeting" => panel.select_meeting(id, window, cx),
        "skill" => panel.select_skill(id, window, cx),
        "mcp" => panel.select_mcp(id, window, cx),
        _ => {}
    });
}

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
            workspace.toggle_panel_focus::<NotebookPanel>(window, cx);
        });
    })
    .detach();
}

#[derive(Clone)]
enum Selection {
    None,
    Note(String),
    Meeting(String),
    /// Skill aberta no painel (nome da pasta em ~/.claude/skills).
    Skill(String),
    /// MCP server aberto no painel (id do context server).
    Mcp(String),
}

#[derive(Clone)]
enum McpConfigSource {
    MomorSettings(Arc<str>),
    ClaudeConfig(String),
}

pub struct NotebookPanel {
    focus_handle: FocusHandle,
    _workspace: WeakEntity<Workspace>,
    language_registry: Arc<LanguageRegistry>,
    folders: Vec<Folder>,
    notes: Vec<Note>,
    meetings: Vec<Meeting>,
    /// Pasta ativa: novas notas/pastas são criadas dentro dela.
    selected_folder: Option<String>,
    /// Pastas recolhidas (id). Vazio = todas abertas.
    collapsed_folders: HashSet<String>,
    /// Seções do rodapé recolhidas.
    mcps_collapsed: bool,
    skills_collapsed: bool,
    /// Grupos virtuais recolhidos nas seções de MCPs/Skills.
    collapsed_mcp_groups: HashSet<String>,
    collapsed_skill_groups: HashSet<String>,
    selection: Selection,
    /// Editor de blocos do documento aberto (nota / reunião / skill — estilo Notion).
    note_doc: Option<Entity<crate::note_editor::NoteDoc>>,
    /// Editor inline do JSON do MCP aberto.
    mcp_editor: Option<Entity<Editor>>,
    mcp_editor_source: Option<McpConfigSource>,
    mcp_editor_error: Option<String>,
    _mcp_editor_sub: Option<Subscription>,
    /// Assinatura do evento OpenNote do doc aberto (wikilink/backlink → abre a nota).
    _note_sub: Option<Subscription>,
    _stt_subscription: Subscription,
    /// Largura ajustável da sidebar (árvore).
    sidebar_width: f32,
    width: Option<gpui::Pixels>,
    /// Skills do usuário (~/.claude/skills): nome + descrição. Universal, listadas
    /// no rodapé do painel independente de provedor/agente.
    skills: Vec<(String, String)>,
    /// Já restaurou o item aberto da sessão anterior? (feito no 1º render, que tem window).
    restored: bool,
    /// Último re-scan das skills do disco — re-escaneia a cada ~2s no render pra pegar skills
    /// que o agente instalou (via terminal) sem precisar reiniciar o app.
    last_skill_scan: std::time::Instant,
    /// MCPs do config universal (~/.claude/mcp-configs), re-escaneados junto com as skills.
    mcp_config_names: Vec<String>,
    /// Caixas de pesquisa (o MESMO componente do Rules: `ui_input::InputField`). Criadas no
    /// 1º render (precisam de window).
    tree_search: Option<Entity<ui_input::InputField>>,
    mcp_search: Option<Entity<ui_input::InputField>>,
    skill_search: Option<Entity<ui_input::InputField>>,
    _search_subs: Vec<Subscription>,
    /// Scroll handles das áreas roláveis do sidebar.
    tree_scroll: ScrollHandle,
    mcp_scroll: ScrollHandle,
    skill_scroll: ScrollHandle,
    /// Altura (px) da área MCPS+SKILLS — ajustável pela alça (arrasta como as colunas).
    abilities_height: f32,
    /// Y do mouse no último frame de arrasto da alça (pra calcular o delta).
    abilities_drag: Option<f32>,
}

#[derive(Clone)]
struct DraggedAbilitiesHandle;
impl Render for DraggedAbilitiesHandle {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

#[derive(Clone)]
struct DraggedSidebarHandle;
impl Render for DraggedSidebarHandle {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

impl NotebookPanel {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        language_registry: Arc<LanguageRegistry>,
        cx: &mut Context<Self>,
    ) -> Self {
        // Salva a reunião quando uma sessão de STT termina com transcrição.
        let stt_subscription = cx.subscribe(
            &stt::Stt::global(cx),
            |this: &mut Self, _stt, event: &stt::SttEvent, cx| {
                if let stt::SttEvent::SessionEnded(transcript) = event
                    && !transcript.trim().is_empty()
                {
                    this.save_meeting(transcript.clone(), cx);
                }
            },
        );

        let mut this = Self {
            focus_handle: cx.focus_handle(),
            _workspace: workspace,
            language_registry,
            folders: Vec::new(),
            notes: Vec::new(),
            meetings: Vec::new(),
            selected_folder: None,
            collapsed_folders: HashSet::new(),
            mcps_collapsed: false,
            skills_collapsed: false,
            collapsed_mcp_groups: HashSet::new(),
            collapsed_skill_groups: HashSet::new(),
            selection: Selection::None,
            note_doc: None,
            mcp_editor: None,
            mcp_editor_source: None,
            mcp_editor_error: None,
            _mcp_editor_sub: None,
            _note_sub: None,
            _stt_subscription: stt_subscription,
            sidebar_width: 260.0,
            width: None,
            skills: load_user_skills(),
            restored: false,
            last_skill_scan: std::time::Instant::now(),
            mcp_config_names: load_mcp_config_names(),
            tree_search: None,
            mcp_search: None,
            skill_search: None,
            _search_subs: Vec::new(),
            tree_scroll: ScrollHandle::new(),
            mcp_scroll: ScrollHandle::new(),
            skill_scroll: ScrollHandle::new(),
            abilities_height: 300.0,
            abilities_drag: None,
        };
        this.reload(cx);
        this
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let db = NotebookDb::global(cx);
        self.folders = db.all_folders();
        self.notes = db.all_notes();
        self.meetings = db.all_meetings();
        // ponytail: v1 não ressincroniza a nota ABERTA quando a IA a edita (o editor de
        // blocos reparsear por baixo atropelaria a edição). Reabrir a nota mostra o novo.
        cx.notify();
    }

    fn save_meeting(&mut self, transcript: String, cx: &mut Context<Self>) {
        let db = NotebookDb::global(cx);
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Local::now();
        let title = format!("Reunião {}", now.format("%d/%m %H:%M"));
        let date = now.to_rfc3339();
        cx.spawn(async move |this, cx| {
            db.insert_meeting(id, None, title, date, transcript, String::new())
                .await
                .log_err();
            this.update(cx, |this, cx| this.reload(cx)).ok();
        })
        .detach();
    }

    fn create_folder(&mut self, cx: &mut Context<Self>) {
        let db = NotebookDb::global(cx);
        let id = uuid::Uuid::new_v4().to_string();
        let parent = self.selected_folder.clone();
        cx.spawn(async move |this, cx| {
            db.insert_folder(id, parent, "Nova pasta".into())
                .await
                .log_err();
            this.update(cx, |this, cx| this.reload(cx)).ok();
        })
        .detach();
    }

    fn create_note(&mut self, cx: &mut Context<Self>) {
        let db = NotebookDb::global(cx);
        let id = uuid::Uuid::new_v4().to_string();
        let folder = self.selected_folder.clone();
        cx.spawn(async move |this, cx| {
            db.insert_note(id, folder, "Nova nota".into(), String::new())
                .await
                .log_err();
            this.update(cx, |this, cx| this.reload(cx)).ok();
        })
        .detach();
    }

    fn select_folder(&mut self, folder_id: String, cx: &mut Context<Self>) {
        // Clicar na pasta abre/fecha (como no momor) e a torna a pasta ativa (onde novas
        // notas são criadas).
        if self.collapsed_folders.contains(&folder_id) {
            self.collapsed_folders.remove(&folder_id);
        } else {
            self.collapsed_folders.insert(folder_id.clone());
        }
        self.selected_folder = Some(folder_id);
        cx.notify();
    }

    fn clear_mcp_editor(&mut self) {
        self.mcp_editor = None;
        self.mcp_editor_source = None;
        self.mcp_editor_error = None;
        self._mcp_editor_sub = None;
    }

    fn select_note(&mut self, note_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(note) = self.notes.iter().find(|n| n.id == note_id).cloned() else {
            return;
        };
        self.clear_mcp_editor();
        self.selection = Selection::Note(note_id);
        // Editor de blocos (estilo Notion) — cria e salva por conta própria.
        let languages = self.language_registry.clone();
        let doc = cx.new(|cx| crate::note_editor::NoteDoc::new(note, languages, window, cx));
        // Seguir wikilink / clicar backlink → o doc emite OpenNote, o painel abre a nota.
        self._note_sub = Some(cx.subscribe_in(
            &doc,
            window,
            |this, _doc, ev: &crate::note_editor::NoteDocEvent, window, cx| {
                let crate::note_editor::NoteDocEvent::OpenNote(title) = ev;
                this.open_note_by_title(title.clone(), window, cx);
            },
        ));
        self.note_doc = Some(doc);
        self.persist_open(cx);
        cx.notify();
    }

    /// Abre a nota cujo título casa (usado por wikilinks/backlinks). Ignora se não existe.
    fn open_note_by_title(&mut self, title: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self
            .notes
            .iter()
            .find(|n| n.title == title)
            .map(|n| n.id.clone())
        {
            self.select_note(id, window, cx);
        }
    }

    fn select_meeting(&mut self, meeting_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(meeting) = self.meetings.iter().find(|m| m.id == meeting_id).cloned() else {
            return;
        };
        self.clear_mcp_editor();
        self.selection = Selection::Meeting(meeting_id);
        // Reunião usa o MESMO block editor das notas (título/corpo) + seções extras
        // (Transcrição/Resumo/Uso) renderizadas dentro dele. Ver NoteDoc::new_meeting.
        let languages = self.language_registry.clone();
        let doc =
            cx.new(|cx| crate::note_editor::NoteDoc::new_meeting(meeting, languages, window, cx));
        self.note_doc = Some(doc);
        self.persist_open(cx);
        cx.notify();
    }

    fn skills_dir() -> std::path::PathBuf {
        paths::home_dir().join(".claude").join("skills")
    }

    /// Diretório real de uma skill — procura em ~/.claude/skills E ~/.claude/.agents/skills
    /// (skills do Codex). Default = ~/.claude/skills (onde novas skills são criadas).
    fn skill_dir_for(name: &str) -> std::path::PathBuf {
        let a = Self::skills_dir().join(name);
        if a.exists() {
            return a;
        }
        let b = paths::home_dir()
            .join(".claude")
            .join(".agents")
            .join("skills")
            .join(name);
        if b.exists() { b } else { a }
    }

    /// Abre uma skill no painel usando o MESMO block editor das notas — as instruções
    /// (markdown) viram blocos. Salva no SKILL.md (frontmatter preservado). Ver NoteDoc::new_skill.
    fn select_skill(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((group, _)) = ability_group_parts(&name) {
            self.collapsed_skill_groups.remove(&group);
        }
        self.clear_mcp_editor();
        let path = Self::skill_dir_for(&name).join("SKILL.md");
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let (front, _desc, body) = parse_skill(&content);
        let languages = self.language_registry.clone();
        let doc = cx.new(|cx| {
            crate::note_editor::NoteDoc::new_skill(name.clone(), front, body, languages, window, cx)
        });
        self.note_doc = Some(doc);
        self.selection = Selection::Skill(name);
        self.persist_open(cx);
        cx.notify();
    }

    /// Cria uma skill nova (template) e abre pra editar.
    fn create_skill(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dir = Self::skills_dir();
        // Nome único: nova-skill, nova-skill-2, …
        let mut name = "nova-skill".to_string();
        let mut n = 2;
        while dir.join(&name).exists() {
            name = format!("nova-skill-{n}");
            n += 1;
        }
        std::fs::create_dir_all(dir.join(&name)).log_err();
        let template = format!(
            "---\nname: {name}\ndescription: Descreva quando o agente deve usar esta skill.\n---\n\nInstruções da skill em markdown.\n"
        );
        std::fs::write(dir.join(&name).join("SKILL.md"), template).log_err();
        self.skills = load_user_skills();
        self.select_skill(name, window, cx);
    }

    /// Abre um MCP no painel com editor inline de JSON.
    fn select_mcp(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((group, _)) = ability_group_parts(&name) {
            self.collapsed_mcp_groups.remove(&group);
        }
        self.note_doc = None;
        self._note_sub = None;
        let (json, source) = self.mcp_editor_config(&name, cx);
        let editor = self.new_mcp_editor(json, window, cx);
        self._mcp_editor_sub = Some(cx.subscribe(&editor, |this, editor, ev, cx| {
            if matches!(ev, EditorEvent::Edited { .. }) {
                this.save_mcp_editor(editor, cx);
            }
        }));
        self.mcp_editor = Some(editor);
        self.mcp_editor_source = Some(source);
        self.mcp_editor_error = None;
        self.selection = Selection::Mcp(name);
        self.persist_open(cx);
        cx.notify();
    }

    /// Salva no KV qual item está aberto → reabrir o app volta pra ele. (Ver `restore_open`.)
    fn mcp_editor_config(&self, name: &str, cx: &mut Context<Self>) -> (String, McpConfigSource) {
        if let Some((id, settings)) = find_momor_mcp_settings(name, cx) {
            let content: settings::ContextServerSettingsContent = settings.into();
            return (
                format_one_mcp_json(id.as_ref(), content),
                McpConfigSource::MomorSettings(id),
            );
        }

        if let Some((key, value)) = load_mcp_config_entry(name) {
            return (
                format_one_mcp_json(&key, value),
                McpConfigSource::ClaudeConfig(key),
            );
        }

        if let Some((id, content)) = self.store_mcp_settings(name, cx) {
            return (
                format_one_mcp_json(id.as_ref(), content),
                McpConfigSource::MomorSettings(id),
            );
        }

        let key = mcp_name_candidates(name)
            .into_iter()
            .find(|candidate| !candidate.starts_with("mcp-server-"))
            .unwrap_or_else(|| name.to_string());
        (
            format_one_mcp_json(&key, serde_json::json!({})),
            McpConfigSource::ClaudeConfig(key),
        )
    }

    fn store_mcp_settings(
        &self,
        name: &str,
        cx: &mut Context<Self>,
    ) -> Option<(Arc<str>, settings::ContextServerSettingsContent)> {
        let ws = self._workspace.upgrade()?;
        let store = ws.read(cx).project().read(cx).context_server_store();
        let store = store.read(cx);
        let id = store
            .server_ids()
            .iter()
            .find(|id| mcp_name_matches(&id.0, name))?
            .clone();
        let config = store.configuration_for_server(&id)?;
        let content = match config.as_ref() {
            project::context_server_store::ContextServerConfiguration::Custom {
                command,
                remote,
            } => settings::ContextServerSettingsContent::Stdio {
                enabled: true,
                remote: *remote,
                command: command.clone(),
            },
            project::context_server_store::ContextServerConfiguration::Extension {
                settings,
                remote,
                ..
            } => settings::ContextServerSettingsContent::Extension {
                enabled: true,
                remote: *remote,
                settings: settings.clone(),
            },
            project::context_server_store::ContextServerConfiguration::Http {
                url,
                headers,
                timeout,
            } => settings::ContextServerSettingsContent::Http {
                enabled: true,
                url: url.to_string(),
                headers: headers.clone(),
                timeout: *timeout,
            },
        };
        Some((id.0, content))
    }

    fn new_mcp_editor(
        &self,
        json: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<Editor> {
        let languages = self.language_registry.clone();
        let editor = cx.new(|cx| {
            let mut editor = Editor::auto_height(4, 24, window, cx);
            editor.set_text(json, window, cx);
            editor.set_show_gutter(false, cx);
            editor.set_show_wrap_guides(false, cx);
            editor.set_soft_wrap_mode(SoftWrap::None, cx);
            editor
        });
        let editor_for_language = editor.clone();
        cx.spawn(async move |_this, cx| {
            let language = languages.language_for_name("jsonc").await.ok();
            cx.update(|cx| {
                if let Some(buffer) = editor_for_language
                    .read(cx)
                    .buffer()
                    .read(cx)
                    .as_singleton()
                {
                    buffer.update(cx, |buffer, cx| {
                        buffer.set_language(language, cx);
                        buffer.set_language_registry(languages);
                    });
                }
            });
        })
        .detach();
        editor
    }

    fn save_mcp_editor(&mut self, editor: Entity<Editor>, cx: &mut Context<Self>) {
        let Some(source) = self.mcp_editor_source.clone() else {
            return;
        };
        let text = editor.read(cx).text(cx);
        let result = match source {
            McpConfigSource::MomorSettings(original_id) => {
                parse_one_mcp_settings(&text).map(|(id, settings)| {
                    if let Some(ws) = self._workspace.upgrade() {
                        let fs = ws.read(cx).app_state().fs.clone();
                        let original_id = original_id.clone();
                        let next_id = id.clone();
                        settings::update_settings_file(fs, cx, move |current, _| {
                            if original_id != next_id {
                                current.project.context_servers.remove(&original_id);
                            }
                            current.project.context_servers.insert(next_id, settings);
                        });
                    }
                    self.mcp_editor_source = Some(McpConfigSource::MomorSettings(id.clone()));
                    self.selection = Selection::Mcp(id.to_string());
                    self.persist_open(cx);
                })
            }
            McpConfigSource::ClaudeConfig(original_name) => {
                parse_one_mcp_value(&text).and_then(|(name, value)| {
                    save_mcp_config_entry(&original_name, name.clone(), value)?;
                    self.mcp_config_names = load_mcp_config_names();
                    self.mcp_editor_source = Some(McpConfigSource::ClaudeConfig(name.clone()));
                    self.selection = Selection::Mcp(name);
                    self.persist_open(cx);
                    Ok(())
                })
            }
        };

        self.mcp_editor_error = result.err().map(|err| err.to_string());
        cx.notify();
    }

    fn persist_open(&self, cx: &mut Context<Self>) {
        let val = match &self.selection {
            Selection::Note(id) => format!("note:{id}"),
            Selection::Meeting(id) => format!("meeting:{id}"),
            Selection::Skill(name) => format!("skill:{name}"),
            Selection::Mcp(name) => format!("mcp:{name}"),
            Selection::None => String::new(),
        };
        let store = db::kvp::KeyValueStore::global(cx);
        db::write_and_log(cx, move || async move {
            store.write_kvp("momor_open_item".into(), val).await
        });
    }

    /// Cria uma caixa de pesquisa = `ui_input::InputField` (MESMO componente do Rules: borda,
    /// ícone de lupa, editor full-width). Assina o editor dele pra re-renderizar e filtrar.
    fn make_search(
        &self,
        placeholder: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<ui_input::InputField>, Subscription) {
        let field = cx.new(|cx| {
            ui_input::InputField::new(window, cx, placeholder)
                .label_min_width(px(0.))
                .start_icon(IconName::MagnifyingGlass)
        });
        let editor = field.read(cx).editor().clone();
        let this = cx.weak_entity();
        let sub = editor.subscribe(
            Box::new(move |event, _window, cx| {
                if matches!(event, ui_input::ErasedEditorEvent::BufferEdited) {
                    this.update(cx, |_this, cx| cx.notify()).ok();
                }
            }),
            window,
            cx,
        );
        (field, sub)
    }

    /// Reabre o item da sessão anterior (chamado 1x no 1º render, que tem `window`).
    fn restore_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(Some(val)) = db::kvp::KeyValueStore::global(cx).read_kvp("momor_open_item") else {
            return;
        };
        let Some((kind, rest)) = val.split_once(':') else {
            return;
        };
        let rest = rest.to_string();
        match kind {
            "note" => self.select_note(rest, window, cx),
            "meeting" => self.select_meeting(rest, window, cx),
            "skill" => self.select_skill(rest, window, cx),
            "mcp" => self.select_mcp(rest, window, cx),
            _ => {}
        }
    }

    fn move_item(
        &mut self,
        item: &DraggedNotebookItem,
        target_folder: Option<String>,
        cx: &mut Context<Self>,
    ) {
        // ponytail: não deixa arrastar a pasta pra dentro dela mesma.
        if item.kind == "folder" && target_folder.as_deref() == Some(item.id.as_str()) {
            return;
        }
        let db = NotebookDb::global(cx);
        let id = item.id.clone();
        let kind = item.kind;
        cx.spawn(async move |this, cx| {
            match kind {
                "folder" => db.set_folder_parent(id, target_folder).await.log_err(),
                "note" => db.set_note_folder(id, target_folder).await.log_err(),
                "meeting" => db.set_meeting_folder(id, target_folder).await.log_err(),
                _ => None,
            };
            this.update(cx, |this, cx| this.reload(cx)).ok();
        })
        .detach();
    }

    fn delete_item(&mut self, kind: &'static str, id: String, cx: &mut Context<Self>) {
        // Se o item deletado está selecionado, limpa a seleção.
        let selected = matches!(
            &self.selection,
            Selection::Note(s) | Selection::Meeting(s) | Selection::Skill(s) if *s == id
        );
        if selected {
            self.selection = Selection::None;
            self.note_doc = None;
            self.persist_open(cx);
        }
        // Skill vive em disco (não no DB): apaga a pasta e recarrega a lista.
        if kind == "skill" {
            std::fs::remove_dir_all(Self::skill_dir_for(&id)).log_err();
            self.skills = load_user_skills();
            cx.notify();
            return;
        }
        let db = NotebookDb::global(cx);
        cx.spawn(async move |this, cx| {
            match kind {
                "folder" => db.delete_folder(id).await.log_err(),
                "note" => db.delete_note(id).await.log_err(),
                "meeting" => db.delete_meeting(id).await.log_err(),
                _ => None,
            };
            this.update(cx, |this, cx| this.reload(cx)).ok();
        })
        .detach();
    }

    fn render_sidebar(
        &self,
        has_selection: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected_id = match &self.selection {
            Selection::Note(id) | Selection::Meeting(id) => Some(id.clone()),
            Selection::Skill(_) | Selection::Mcp(_) | Selection::None => None,
        };
        // ponytail: computa as linhas (owned) por DFS antes dos listeners, senão o
        // empréstimo de self/cx colide dentro do .map.
        let mut rows = Vec::new();
        flatten_rows(
            &self.folders,
            &self.notes,
            &self.meetings,
            None,
            0,
            &self.collapsed_folders,
            &mut rows,
        );
        // Filtro da busca da árvore (por título, case-insensitive → lista achatada).
        let tree_query = self
            .tree_search
            .as_ref()
            .map(|e| e.read(cx).text(cx).trim().to_string())
            .unwrap_or_default();
        let tree_q = tree_query.to_lowercase();
        if !tree_q.is_empty() {
            rows.retain(|r| {
                let title = match r {
                    Row::Folder { name, .. } => name,
                    Row::Note { title, .. } | Row::Meeting { title, .. } => title,
                };
                title.to_lowercase().contains(&tree_q)
            });
        }
        let tree_no_results = !tree_query.is_empty() && rows.is_empty();
        let selected_folder = self.selected_folder.clone();
        let selected_bg = cx.theme().colors().element_selected;
        let hover_bg = cx.theme().colors().element_hover;

        v_flex()
            .relative()
            .min_w_0()
            .h_full()
            .items_stretch()
            // Sem nota aberta, a árvore preenche a coluna; com nota, largura fixa+alça.
            .map(|el| {
                if has_selection {
                    el.w(px(self.sidebar_width))
                        .flex_none()
                        .border_r_1()
                        .border_color(cx.theme().colors().border)
                } else {
                    el.flex_1()
                }
            })
            // ponytail: alça de resize da sidebar (só útil quando há conteúdo ao lado).
            .when(has_selection, |el| {
                el.child(gpui::deferred(
                    div()
                        .id("notebook-sidebar-resize")
                        .absolute()
                        .top_0()
                        .right(px(-3.))
                        .w(px(6.))
                        .h_full()
                        .cursor_col_resize()
                        .on_drag(DraggedSidebarHandle, |d, _, _, cx| {
                            cx.stop_propagation();
                            cx.new(|_| d.clone())
                        })
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .occlude(),
                ))
            })
            .child(
                h_flex()
                    .p_2()
                    .gap_1()
                    .justify_between()
                    .child(Label::new("Workspace").size(LabelSize::Small))
                    .child(
                        h_flex()
                            .gap_0p5()
                            .child(
                                IconButton::new("new-folder", IconName::FolderOpenAdd)
                                    .icon_size(IconSize::Small)
                                    .tooltip(Tooltip::text("Nova pasta"))
                                    .on_click(cx.listener(|this, _, _, cx| this.create_folder(cx))),
                            )
                            .child(
                                IconButton::new("new-note", IconName::Plus)
                                    .icon_size(IconSize::Small)
                                    .tooltip(Tooltip::text("Nova nota"))
                                    .on_click(cx.listener(|this, _, _, cx| this.create_note(cx))),
                            ),
                    ),
            )
            .child(Divider::horizontal())
            .when_some(self.tree_search.clone(), |el, ed| {
                el.child(div().w_full().min_w_0().px_2().mt_1().mb_1().child(ed))
            })
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .w_full()
                    .relative()
                    .child(
                        v_flex()
                            .id("notebook-tree")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.tree_scroll)
                            .p_1()
                            .gap_0p5()
                            // ponytail: soltar na área vazia da árvore = mover pra raiz.
                            .on_drop::<DraggedNotebookItem>(
                                cx.listener(|this, item, _, cx| this.move_item(item, None, cx)),
                            )
                            .children(rows.into_iter().map(|row| match row {
                                Row::Folder {
                                    id,
                                    name,
                                    depth,
                                    collapsed,
                                } => {
                                    let sel_id = id.clone();
                                    let del_id = id.clone();
                                    let drop_folder = id.clone();
                                    let is_selected =
                                        selected_folder.as_deref() == Some(id.as_str());
                                    let drag = DraggedNotebookItem {
                                        kind: "folder",
                                        id: id.clone(),
                                        label: name.clone(),
                                    };
                                    let on_drop: DropHandler = Box::new(cx.listener(
                                        move |this, item: &DraggedNotebookItem, _, cx| {
                                            this.move_item(item, Some(drop_folder.clone()), cx)
                                        },
                                    ));
                                    tree_row(
                                        SharedString::from(format!("folder-{id}")),
                                        IconName::Folder,
                                        Color::Muted,
                                        None,
                                        Some(!collapsed),
                                        name,
                                        is_selected,
                                        depth,
                                        selected_bg,
                                        hover_bg,
                                        Some(drag),
                                        Some(on_drop),
                                        cx.listener(move |this, _, _, cx| {
                                            this.select_folder(sel_id.clone(), cx)
                                        }),
                                        Some(Box::new(cx.listener(move |this, _, _, cx| {
                                            this.delete_item("folder", del_id.clone(), cx)
                                        }))),
                                    )
                                    .into_any_element()
                                }
                                Row::Note {
                                    id,
                                    title,
                                    icon,
                                    depth,
                                } => {
                                    let open_id = id.clone();
                                    let del_id = id.clone();
                                    let is_selected = selected_id.as_deref() == Some(id.as_str());
                                    let drag = DraggedNotebookItem {
                                        kind: "note",
                                        id: id.clone(),
                                        label: title.clone(),
                                    };
                                    tree_row(
                                        SharedString::from(format!("note-{id}")),
                                        IconName::FileDoc,
                                        Color::Muted,
                                        icon,
                                        None,
                                        title,
                                        is_selected,
                                        depth,
                                        selected_bg,
                                        hover_bg,
                                        Some(drag),
                                        None,
                                        cx.listener(move |this, _, window, cx| {
                                            this.select_note(open_id.clone(), window, cx)
                                        }),
                                        Some(Box::new(cx.listener(move |this, _, _, cx| {
                                            this.delete_item("note", del_id.clone(), cx)
                                        }))),
                                    )
                                    .into_any_element()
                                }
                                Row::Meeting {
                                    id,
                                    title,
                                    icon,
                                    depth,
                                } => {
                                    let open_id = id.clone();
                                    let del_id = id.clone();
                                    let is_selected = selected_id.as_deref() == Some(id.as_str());
                                    let drag = DraggedNotebookItem {
                                        kind: "meeting",
                                        id: id.clone(),
                                        label: title.clone(),
                                    };
                                    tree_row(
                                        SharedString::from(format!("meeting-{id}")),
                                        IconName::Mic,
                                        Color::Muted,
                                        icon,
                                        None,
                                        title,
                                        is_selected,
                                        depth,
                                        selected_bg,
                                        hover_bg,
                                        Some(drag),
                                        None,
                                        cx.listener(move |this, _, window, cx| {
                                            this.select_meeting(open_id.clone(), window, cx)
                                        }),
                                        Some(Box::new(cx.listener(move |this, _, _, cx| {
                                            this.delete_item("meeting", del_id.clone(), cx)
                                        }))),
                                    )
                                    .into_any_element()
                                }
                            }))
                            .when(tree_no_results, |el| {
                                el.child(search_empty_state(tree_query.clone()))
                            }),
                    )
                    .custom_scrollbars(
                        Scrollbars::new(ScrollAxes::Vertical)
                            .tracked_scroll_handle(&self.tree_scroll),
                        window,
                        cx,
                    ),
            )
            .child(self.render_ability_sections(window, cx))
    }

    /// Rodapé do painel: MCPS e SKILLS — sempre visíveis, universais (não dependem
    /// do provedor/agente). Espelha o sidebar do momor.
    fn render_ability_sections(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected_bg = cx.theme().colors().element_selected;
        let hover_bg = cx.theme().colors().element_hover;
        // MCP servers UNIVERSAIS: os conectados no momor (store, com cor de status) +
        // os do config do ecossistema (~/.claude/mcp-configs), dedupados por nome.
        let mut mcps: Vec<(String, Color)> = self
            ._workspace
            .upgrade()
            .map(|ws| {
                let store = ws.read(cx).project().read(cx).context_server_store();
                let store = store.read(cx);
                // server_ids() = TODOS (inclui os vindos de extensão, ex.: Context7); antes
                // usávamos configured_server_ids() que só pega os do settings.json → dava 0.
                store
                    .server_ids()
                    .iter()
                    .cloned()
                    .map(|id| {
                        let color = match store.status_for_server(&id) {
                            Some(ContextServerStatus::Running) => Color::Success,
                            Some(ContextServerStatus::Error(_)) => Color::Error,
                            _ => Color::Muted,
                        };
                        (id.0.to_string(), color)
                    })
                    .collect()
            })
            .unwrap_or_default();
        // MCPs do config universal que ainda não estão no store (não conectados aqui) → Muted.
        let seen: HashSet<String> = mcps.iter().map(|(n, _)| n.clone()).collect();
        for name in &self.mcp_config_names {
            if !seen.contains(name) && !seen.contains(&format!("mcp-server-{name}")) {
                mcps.push((name.clone(), Color::Muted));
            }
        }
        mcps.sort_by(|a, b| a.0.cmp(&b.0));

        let mcp_count = mcps.len();
        let skill_count = self.skills.len();
        // Filtro das caixas de pesquisa (por substring, case-insensitive).
        let mcp_query = self
            .mcp_search
            .as_ref()
            .map(|e| e.read(cx).text(cx).trim().to_string())
            .unwrap_or_default();
        let mcp_q = mcp_query.to_lowercase();
        let skill_query = self
            .skill_search
            .as_ref()
            .map(|e| e.read(cx).text(cx).trim().to_string())
            .unwrap_or_default();
        let skill_q = skill_query.to_lowercase();

        let mcp_rows: Vec<gpui::AnyElement> = if self.mcps_collapsed {
            Vec::new()
        } else {
            flatten_ability_rows(
                mcps.into_iter()
                    .map(|(name, color)| AbilityListItem {
                        full_name: name.clone(),
                        display_name: name.clone(),
                        dot: color,
                        selected: matches!(&self.selection, Selection::Mcp(s) if *s == name),
                        kind: "mcp",
                        deletable: false,
                    })
                    .collect(),
                &self.collapsed_mcp_groups,
                &mcp_q,
            )
            .into_iter()
            .map(|row| match row {
                AbilityTreeRow::Folder {
                    key,
                    name,
                    depth,
                    collapsed,
                } => {
                    let group_key = key.clone();
                    tree_row(
                        SharedString::from(format!("mcp-group-{group_key}")),
                        IconName::Folder,
                        Color::Muted,
                        None,
                        Some(!collapsed),
                        name,
                        false,
                        depth,
                        selected_bg,
                        hover_bg,
                        None,
                        None,
                        cx.listener(move |this, _, _, cx| {
                            if this.collapsed_mcp_groups.contains(&group_key) {
                                this.collapsed_mcp_groups.remove(&group_key);
                            } else {
                                this.collapsed_mcp_groups.insert(group_key.clone());
                            }
                            cx.notify();
                        }),
                        None,
                    )
                    .into_any_element()
                }
                AbilityTreeRow::Item { item, depth } => {
                    let click = item.full_name.clone();
                    let drag = DraggedNotebookItem {
                        kind: item.kind,
                        id: item.full_name.clone(),
                        label: item.full_name.clone(),
                    };
                    tree_row(
                        SharedString::from(format!("mcp-{}", item.full_name)),
                        IconName::Server,
                        item.dot,
                        None,
                        None,
                        item.display_name,
                        item.selected,
                        depth,
                        selected_bg,
                        hover_bg,
                        Some(drag),
                        None,
                        cx.listener(move |this, _, window, cx| {
                            this.select_mcp(click.clone(), window, cx)
                        }),
                        None,
                    )
                    .into_any_element()
                }
            })
            .collect()
        };
        let skill_rows: Vec<gpui::AnyElement> = if self.skills_collapsed {
            Vec::new()
        } else {
            flatten_ability_rows(
                self.skills
                    .clone()
                    .into_iter()
                    .map(|(name, _)| AbilityListItem {
                        full_name: name.clone(),
                        display_name: name.clone(),
                        dot: Color::Success,
                        selected: matches!(&self.selection, Selection::Skill(s) if *s == name),
                        kind: "skill",
                        deletable: true,
                    })
                    .collect(),
                &self.collapsed_skill_groups,
                &skill_q,
            )
            .into_iter()
            .map(|row| match row {
                AbilityTreeRow::Folder {
                    key,
                    name,
                    depth,
                    collapsed,
                } => {
                    let group_key = key.clone();
                    tree_row(
                        SharedString::from(format!("skill-group-{group_key}")),
                        IconName::Folder,
                        Color::Muted,
                        None,
                        Some(!collapsed),
                        name,
                        false,
                        depth,
                        selected_bg,
                        hover_bg,
                        None,
                        None,
                        cx.listener(move |this, _, _, cx| {
                            if this.collapsed_skill_groups.contains(&group_key) {
                                this.collapsed_skill_groups.remove(&group_key);
                            } else {
                                this.collapsed_skill_groups.insert(group_key.clone());
                            }
                            cx.notify();
                        }),
                        None,
                    )
                    .into_any_element()
                }
                AbilityTreeRow::Item { item, depth } => {
                    let click = item.full_name.clone();
                    let del = item.full_name.clone();
                    let drag = DraggedNotebookItem {
                        kind: item.kind,
                        id: item.full_name.clone(),
                        label: item.full_name.clone(),
                    };
                    tree_row(
                        SharedString::from(format!("skill-{}", item.full_name)),
                        IconName::Book,
                        item.dot,
                        None,
                        None,
                        item.display_name,
                        item.selected,
                        depth,
                        selected_bg,
                        hover_bg,
                        Some(drag),
                        None,
                        cx.listener(move |this, _, window, cx| {
                            this.select_skill(click.clone(), window, cx)
                        }),
                        item.deletable.then(|| {
                            Box::new(cx.listener(move |this, _, _, cx| {
                                this.delete_item("skill", del.clone(), cx)
                            })) as ClickHandler
                        }),
                    )
                    .into_any_element()
                }
            })
            .collect()
        };

        let sec_h = self.abilities_height / 2.0;
        v_flex()
            .w_full()
            .min_w_0()
            .items_stretch()
            .flex_none()
            // Alça de resize (arrasta pra cima/baixo → muda a altura das listas).
            .child(
                div()
                    .id("abilities-resize")
                    .h(px(8.))
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_row_resize()
                    .on_drag(DraggedAbilitiesHandle, |d, _, _, cx| {
                        cx.stop_propagation();
                        cx.new(|_| d.clone())
                    })
                    .child(
                        div()
                            .h(px(3.))
                            .w(px(40.))
                            .rounded_full()
                            .bg(cx.theme().colors().border),
                    ),
            )
            .child(section_header(
                "MCPS",
                mcp_count,
                self.mcps_collapsed,
                IconButton::new("add-mcp", IconName::Plus)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text("Configurar MCP servers (settings)"))
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.stop_propagation();
                        cx.reveal_path(paths::settings_file());
                    }))
                    .into_any_element(),
                cx.listener(|this, _, _, cx| {
                    this.mcps_collapsed = !this.mcps_collapsed;
                    cx.notify();
                }),
            ))
            .when(!self.mcps_collapsed, |el| {
                el.child(self.section_list(
                    self.mcp_search.clone(),
                    mcp_rows,
                    mcp_query,
                    "mcp-scroll",
                    &self.mcp_scroll,
                    sec_h,
                    window,
                    cx,
                ))
            })
            .child(section_header(
                "SKILLS",
                skill_count,
                self.skills_collapsed,
                IconButton::new("add-skill", IconName::Plus)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text("Nova skill"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.create_skill(window, cx);
                    }))
                    .into_any_element(),
                cx.listener(|this, _, _, cx| {
                    this.skills_collapsed = !this.skills_collapsed;
                    cx.notify();
                }),
            ))
            .when(!self.skills_collapsed, |el| {
                el.child(self.section_list(
                    self.skill_search.clone(),
                    skill_rows,
                    skill_query,
                    "skill-scroll",
                    &self.skill_scroll,
                    sec_h,
                    window,
                    cx,
                ))
            })
    }

    /// Container de uma seção: caixa de pesquisa (se houver) + lista com altura máxima e
    /// scroll interno (pra não empurrar o resto do painel quando há muitos itens).
    fn section_list(
        &self,
        search: Option<Entity<ui_input::InputField>>,
        rows: Vec<gpui::AnyElement>,
        query: String,
        scroll_id: &'static str,
        scroll_handle: &ScrollHandle,
        max_height: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let no_results = !query.is_empty() && rows.is_empty();
        v_flex()
            .w_full()
            .min_w_0()
            .items_stretch()
            .flex_none()
            .when_some(search, |el, ed| {
                // InputField = MESMO componente do Rules (borda + lupa + editor full-width).
                el.child(div().w_full().min_w_0().px_2().mb_1().child(ed))
            })
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .max_h(px(max_height.max(80.)))
                    .relative()
                    .child(
                        v_flex()
                            .w_full()
                            .min_w_0()
                            .id(scroll_id)
                            .max_h(px(max_height.max(80.)))
                            .overflow_y_scroll()
                            .track_scroll(scroll_handle)
                            .p_1()
                            .gap_0p5()
                            .child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .children(rows)
                                    .when(no_results, |el| {
                                        el.child(search_empty_state(query.clone()))
                                    }),
                            ),
                    )
                    .custom_scrollbars(
                        Scrollbars::new(ScrollAxes::Vertical)
                            .tracked_scroll_handle(scroll_handle)
                            .id(SharedString::from(format!("{scroll_id}-scrollbar"))),
                        window,
                        cx,
                    ),
            )
            .into_any_element()
    }

    fn render_content(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // ponytail: conteúdo preenche o resto da coluna (o resize do chat é externo).
        // min_h(0) é o que permite o overflow_y_scroll interno funcionar.
        div()
            .flex_1()
            .h_full()
            .min_h(px(0.))
            // Largura mínima: a nota nunca colapsa (evita quebra em 1 caractere).
            .min_w(px(360.))
            .overflow_hidden()
            .child(self.render_content_inner(cx))
    }

    fn render_content_inner(&self, cx: &mut Context<Self>) -> impl IntoElement {
        match &self.selection {
            Selection::Note(_) => {
                if let Some(doc) = &self.note_doc {
                    div()
                        .flex_1()
                        .h_full()
                        .child(doc.clone())
                        .into_any_element()
                } else {
                    empty_state("Selecione uma nota", cx)
                }
            }
            // Reunião usa o mesmo NoteDoc das notas (com seções Transcrição/Resumo/Uso dentro).
            Selection::Meeting(_) => {
                if let Some(doc) = &self.note_doc {
                    div()
                        .flex_1()
                        .h_full()
                        .child(doc.clone())
                        .into_any_element()
                } else {
                    empty_state("Selecione uma reunião", cx)
                }
            }
            // Skill usa o mesmo NoteDoc das notas (instruções = blocos markdown).
            Selection::Skill(_) => {
                if let Some(doc) = &self.note_doc {
                    div()
                        .flex_1()
                        .h_full()
                        .child(doc.clone())
                        .into_any_element()
                } else {
                    empty_state("Selecione uma skill", cx)
                }
            }
            Selection::Mcp(name) => {
                let name = name.clone();
                self.render_mcp_view(name, cx)
            }
            Selection::None => empty_state(
                "Suas notas e reuniões aparecem aqui. Fale numa call para gerar uma transcrição.",
                cx,
            ),
        }
    }

    /// View read-only de um MCP: JSON de config + status; editar abre o settings.json.
    fn render_mcp_view(&self, name: String, cx: &mut Context<Self>) -> gpui::AnyElement {
        let border = cx.theme().colors().border;
        let (status_label, status_color) = self.mcp_status(&name, cx);
        let save_message = self
            .mcp_editor_error
            .as_ref()
            .map(|error| (format!("JSON inválido: {error}"), Color::Error))
            .unwrap_or_else(|| ("Salvo automaticamente".to_string(), Color::Muted));

        v_flex()
            .id("mcp-view")
            .flex_1()
            .h_full()
            .min_h(px(0.))
            .p_6()
            .gap_3()
            .overflow_y_scroll()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Icon::new(IconName::ToolHammer).color(Color::Accent))
                    .child(Label::new(name).size(LabelSize::Large))
                    .child(div().flex_1())
                    .child(
                        Label::new(status_label)
                            .color(status_color)
                            .size(LabelSize::Small),
                    ),
            )
            .child(field_label("Configuração"))
            .when_some(self.mcp_editor.clone(), |el, editor| {
                el.child(boxed(border).child(editor))
            })
            .child(
                Label::new(save_message.0)
                    .color(save_message.1)
                    .size(LabelSize::Small),
            )
            .into_any_element()
    }

    fn mcp_status(&self, name: &str, cx: &mut Context<Self>) -> (&'static str, Color) {
        self._workspace
            .upgrade()
            .and_then(|ws| {
                let store = ws.read(cx).project().read(cx).context_server_store();
                let store = store.read(cx);
                store
                    .server_ids()
                    .iter()
                    .find(|id| mcp_name_matches(&id.0, name))
                    .and_then(|id| store.status_for_server(id))
            })
            .map(|status| match status {
                ContextServerStatus::Running => ("Ativo", Color::Success),
                ContextServerStatus::Error(_) => ("Erro", Color::Error),
                _ => ("Parado", Color::Muted),
            })
            .unwrap_or(("Parado", Color::Muted))
    }
}

/// Payload arrastado (nota/pasta/reunião) pra soltar dentro de uma pasta OU no chat.
#[derive(Clone)]
pub struct DraggedNotebookItem {
    pub kind: &'static str,
    pub id: String,
    pub label: String,
}

/// Payload shared by browser surfaces and the chat composer.
///
/// Keeping this beside the existing notebook drag payload avoids a dependency
/// from the generic chat crate back into Momor's browser implementation.
#[derive(Clone)]
pub struct DraggedBrowserTab {
    pub url: String,
    pub title: String,
}

impl Render for DraggedBrowserTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .px_2()
            .py_0p5()
            .rounded_sm()
            .bg(cx.theme().colors().elevated_surface_background)
            .border_1()
            .border_color(cx.theme().colors().border)
            .gap_1()
            .child(Icon::new(IconName::Public).size(IconSize::XSmall))
            .child(Label::new(self.title.clone()).size(LabelSize::Small))
    }
}

/// Conteúdo textual de um item, pra injetar como contexto ao soltar no chat.
/// Emoji do chip do chat pra um item arrastado: usa o ícone custom da nota/reunião (se for
/// emoji — caminho de imagem não cabe em texto), senão o padrão por tipo.
pub fn drop_icon(item: &DraggedNotebookItem, cx: &App) -> String {
    let default = match item.kind {
        "folder" => "📁",
        "meeting" => "🎙️",
        "skill" => "🧩",
        "mcp" => "🔌",
        _ => "📝",
    };
    let db = NotebookDb::global(cx);
    let custom = match item.kind {
        "note" => db
            .all_notes()
            .into_iter()
            .find(|n| n.id == item.id)
            .and_then(|n| n.icon),
        "meeting" => db
            .all_meetings()
            .into_iter()
            .find(|m| m.id == item.id)
            .and_then(|m| m.icon),
        _ => None,
    };
    custom
        .filter(|ic| !ic.is_empty() && !ic.contains('/') && !ic.contains('\\'))
        .unwrap_or_else(|| default.to_string())
}

/// Ícone CUSTOM (emoji OU caminho de imagem) da nota/reunião pro badge de citação; "" se não
/// tem custom (o badge cai no ícone por tipo). Diferente de `drop_icon` (só emoji + default).
pub fn drop_badge_icon(item: &DraggedNotebookItem, cx: &App) -> String {
    let db = NotebookDb::global(cx);
    let custom = match item.kind {
        "note" => db
            .all_notes()
            .into_iter()
            .find(|n| n.id == item.id)
            .and_then(|n| n.icon),
        "meeting" => db
            .all_meetings()
            .into_iter()
            .find(|m| m.id == item.id)
            .and_then(|m| m.icon),
        _ => None,
    };
    custom.filter(|s| !s.is_empty()).unwrap_or_default()
}

pub fn drop_text(item: &DraggedNotebookItem, cx: &App) -> String {
    let db = NotebookDb::global(cx);
    match item.kind {
        "note" => db
            .all_notes()
            .into_iter()
            .find(|n| n.id == item.id)
            .map(|n| format!("# {}\n\n{}\n", n.title, n.content))
            .unwrap_or_default(),
        "meeting" => db
            .all_meetings()
            .into_iter()
            .find(|m| m.id == item.id)
            .map(|m| {
                let mut out = format!("# {} ({})\n\n", m.title, m.date);
                if !m.summary.trim().is_empty() {
                    out.push_str(&format!("## Resumo\n{}\n\n", m.summary));
                }
                out.push_str(&format!("## Transcrição\n{}\n", m.transcript));
                out
            })
            .unwrap_or_default(),
        "folder" => {
            // Todas as notas + reuniões dessa pasta.
            let notes = db.all_notes();
            let meetings = db.all_meetings();
            let mut out = String::new();
            for n in notes
                .iter()
                .filter(|n| n.folder_id.as_deref() == Some(item.id.as_str()))
            {
                out.push_str(&format!("# {}\n\n{}\n\n", n.title, n.content));
            }
            for m in meetings
                .iter()
                .filter(|m| m.folder_id.as_deref() == Some(item.id.as_str()))
            {
                out.push_str(&format!("# {}\n\n{}\n\n", m.title, m.transcript));
            }
            out
        }
        // Skill: injeta o corpo do SKILL.md (instruções) como contexto pra IA.
        "skill" => {
            let path = NotebookPanel::skill_dir_for(&item.id).join("SKILL.md");
            std::fs::read_to_string(path).unwrap_or_default()
        }
        // MCP: só uma referência ao servidor (a config vive no settings/ecossistema).
        "mcp" => format!("MCP server: {}", item.label),
        _ => String::new(),
    }
}

impl Render for DraggedNotebookItem {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .px_2()
            .py_0p5()
            .rounded_sm()
            .bg(cx.theme().colors().elevated_surface_background)
            .border_1()
            .border_color(cx.theme().colors().border)
            .child(Label::new(self.label.clone()).size(LabelSize::Small))
    }
}

type DropHandler = Box<dyn Fn(&DraggedNotebookItem, &mut Window, &mut App)>;
type ClickHandler = Box<dyn Fn(&gpui::ClickEvent, &mut Window, &mut App)>;

/// Linha achatada da árvore (com profundidade pra indentar).
enum Row {
    Folder {
        id: String,
        name: String,
        depth: usize,
        collapsed: bool,
    },
    Note {
        id: String,
        title: String,
        icon: Option<String>,
        depth: usize,
    },
    Meeting {
        id: String,
        title: String,
        icon: Option<String>,
        depth: usize,
    },
}

#[derive(Clone)]
struct AbilityListItem {
    full_name: String,
    display_name: String,
    dot: Color,
    selected: bool,
    kind: &'static str,
    deletable: bool,
}

enum AbilityTreeRow {
    Folder {
        key: String,
        name: String,
        depth: usize,
        collapsed: bool,
    },
    Item {
        item: AbilityListItem,
        depth: usize,
    },
}

fn ability_group_parts(name: &str) -> Option<(String, String)> {
    for separator in [':', '/'] {
        if let Some((group, rest)) = name.split_once(separator)
            && !group.is_empty()
            && !rest.is_empty()
        {
            return Some((group.to_string(), rest.to_string()));
        }
    }
    if let Some((group, rest)) = name.split_once('-')
        && !group.is_empty()
        && !rest.is_empty()
    {
        return Some((group.to_string(), rest.to_string()));
    }
    None
}

fn flatten_ability_rows(
    items: Vec<AbilityListItem>,
    collapsed: &HashSet<String>,
    query: &str,
) -> Vec<AbilityTreeRow> {
    if !query.is_empty() {
        let mut filtered: Vec<_> = items
            .into_iter()
            .filter(|item| item.full_name.to_lowercase().contains(query))
            .collect();
        filtered.sort_by(|a, b| a.full_name.cmp(&b.full_name));
        return filtered
            .into_iter()
            .map(|mut item| {
                item.display_name = item.full_name.clone();
                AbilityTreeRow::Item { item, depth: 0 }
            })
            .collect();
    }

    let mut grouped: BTreeMap<String, Vec<AbilityListItem>> = BTreeMap::new();
    let mut root_items = Vec::new();

    for mut item in items {
        if let Some((group, child_name)) = ability_group_parts(&item.full_name) {
            item.display_name = child_name;
            grouped.entry(group).or_default().push(item);
        } else {
            root_items.push(item);
        }
    }

    let mut rows = Vec::new();
    for (group, mut children) in grouped {
        children.sort_by(|a, b| a.display_name.cmp(&b.display_name));

        if children.len() < 2 {
            for mut child in children {
                child.display_name = child.full_name.clone();
                root_items.push(child);
            }
            continue;
        }

        let is_collapsed = collapsed.contains(&group);
        rows.push(AbilityTreeRow::Folder {
            key: group.clone(),
            name: group,
            depth: 0,
            collapsed: is_collapsed,
        });

        if !is_collapsed {
            rows.extend(
                children
                    .into_iter()
                    .map(|item| AbilityTreeRow::Item { item, depth: 1 }),
            );
        }
    }

    root_items.sort_by(|a, b| a.full_name.cmp(&b.full_name));
    rows.extend(
        root_items
            .into_iter()
            .map(|item| AbilityTreeRow::Item { item, depth: 0 }),
    );
    rows
}

/// DFS: pastas → subpastas/notas/reuniões indentadas; itens da raiz por último.
/// Pastas em `collapsed` não expõem os filhos.
fn flatten_rows(
    folders: &[Folder],
    notes: &[Note],
    meetings: &[Meeting],
    parent: Option<&str>,
    depth: usize,
    collapsed: &HashSet<String>,
    out: &mut Vec<Row>,
) {
    for folder in folders.iter().filter(|f| f.parent_id.as_deref() == parent) {
        let is_collapsed = collapsed.contains(&folder.id);
        out.push(Row::Folder {
            id: folder.id.clone(),
            name: folder.name.clone(),
            depth,
            collapsed: is_collapsed,
        });
        if is_collapsed {
            continue;
        }
        flatten_rows(
            folders,
            notes,
            meetings,
            Some(&folder.id),
            depth + 1,
            collapsed,
            out,
        );
        for note in notes
            .iter()
            .filter(|n| n.folder_id.as_deref() == Some(folder.id.as_str()))
        {
            out.push(Row::Note {
                id: note.id.clone(),
                title: note.title.clone(),
                icon: note.icon.clone(),
                depth: depth + 1,
            });
        }
        for meeting in meetings
            .iter()
            .filter(|m| m.folder_id.as_deref() == Some(folder.id.as_str()))
        {
            out.push(Row::Meeting {
                id: meeting.id.clone(),
                title: meeting.title.clone(),
                icon: meeting.icon.clone(),
                depth: depth + 1,
            });
        }
    }
    if parent.is_none() {
        for note in notes.iter().filter(|n| n.folder_id.is_none()) {
            out.push(Row::Note {
                id: note.id.clone(),
                title: note.title.clone(),
                icon: note.icon.clone(),
                depth: 0,
            });
        }
        for meeting in meetings.iter().filter(|m| m.folder_id.is_none()) {
            out.push(Row::Meeting {
                id: meeting.id.clone(),
                title: meeting.title.clone(),
                icon: meeting.icon.clone(),
                depth: 0,
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn tree_row(
    id: impl Into<gpui::ElementId>,
    icon: IconName,
    icon_color: Color,
    // Ícone custom (emoji ou caminho de imagem) — substitui o `icon` padrão quando Some.
    custom_icon: Option<String>,
    // Some(expanded) desenha o chevron de pasta (▸/▾); None p/ nota/reunião.
    chevron: Option<bool>,
    label: String,
    is_selected: bool,
    indent: usize,
    selected_bg: gpui::Hsla,
    hover_bg: gpui::Hsla,
    drag: Option<DraggedNotebookItem>,
    on_drop_folder: Option<DropHandler>,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    on_delete: Option<ClickHandler>,
) -> impl IntoElement {
    let id = id.into();
    let group = SharedString::from(format!("tree-row-{id}"));
    let delete_id = ElementId::Name(SharedString::from(format!("{id}-del")));
    let drop_bg = selected_bg;
    h_flex()
        .id(id)
        .group(group.clone())
        .w_full()
        .pr_2()
        .pl(px(8. + indent as f32 * 14.))
        .py_1()
        .gap_1p5()
        .rounded_sm()
        .cursor_pointer()
        .when(is_selected, |el| el.bg(selected_bg))
        .hover(move |el| el.bg(hover_bg))
        // ponytail: cada linha é arrastável; pastas aceitam soltar dentro.
        .when_some(drag, |el, drag| {
            el.on_drag(drag, |d, _, _, cx| cx.new(|_| d.clone()))
        })
        .when_some(on_drop_folder, |el, on_drop| {
            el.drag_over::<DraggedNotebookItem>(move |el, _, _, _| el.bg(drop_bg))
                .on_drop::<DraggedNotebookItem>(move |item, window, cx| on_drop(item, window, cx))
        })
        // Chevron da pasta (▸ fechada / ▾ aberta). Nota/reunião: espaçador p/ alinhar.
        .child(match chevron {
            Some(expanded) => Icon::new(if expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .size(IconSize::XSmall)
            .color(Color::Muted)
            .into_any_element(),
            None => div().w(px(12.)).into_any_element(),
        })
        // Ícone: custom (emoji/imagem) se houver, senão o padrão de nota/reunião/pasta.
        .child(match custom_icon.filter(|s| !s.is_empty()) {
            Some(ic) if ic.contains('/') || ic.contains('\\') => {
                gpui::img(std::path::PathBuf::from(ic))
                    .size(px(16.))
                    .rounded_sm()
                    .object_fit(gpui::ObjectFit::Cover)
                    .into_any_element()
            }
            Some(ic) => div().text_size(px(14.)).child(ic).into_any_element(),
            None => Icon::new(icon)
                .size(IconSize::Small)
                .color(icon_color)
                .into_any_element(),
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(Label::new(label).size(LabelSize::Small).truncate()),
        )
        // ponytail: lixeira aparece no hover da linha
        .when_some(on_delete, |el, on_delete| {
            el.child(
                IconButton::new(delete_id, IconName::Trash)
                    .icon_size(IconSize::XSmall)
                    .icon_color(Color::Muted)
                    .visible_on_hover(group)
                    .on_click(move |ev, window, cx| {
                        cx.stop_propagation();
                        on_delete(ev, window, cx);
                    }),
            )
        })
        .on_click(move |ev, window, cx| on_click(ev, window, cx))
}

/// Cabeçalho colapsável do rodapé (MCPS/SKILLS): chevron + rótulo + contagem + botão `+`.
/// Clicar na linha (fora do `+`) recolhe/expande a seção.
fn section_header(
    label: &'static str,
    count: usize,
    collapsed: bool,
    add_button: gpui::AnyElement,
    on_toggle: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    h_flex()
        .id(label)
        .w_full()
        .min_w_0()
        .px_2()
        .py_1()
        .justify_between()
        .items_center()
        .cursor_pointer()
        .on_click(move |e, window, cx| on_toggle(e, window, cx))
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .child(
                    Icon::new(if collapsed {
                        IconName::ChevronRight
                    } else {
                        IconName::ChevronDown
                    })
                    .size(IconSize::XSmall)
                    .color(Color::Muted),
                )
                .child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
                .child(
                    Label::new(count.to_string())
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
        )
        .child(add_button)
}

/// Rótulo de campo (uppercase, muted).
fn field_label(text: &str) -> gpui::AnyElement {
    Label::new(text.to_uppercase())
        .size(LabelSize::XSmall)
        .color(Color::Muted)
        .into_any_element()
}

/// Caixa com borda arredondada (editores de skill / bloco JSON do MCP).
fn boxed(border: gpui::Hsla) -> gpui::Div {
    div()
        .w_full()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(border)
}

/// Divide o SKILL.md em (linhas do frontmatter, description, corpo).
fn parse_skill(content: &str) -> (Vec<String>, String, String) {
    let trimmed = content.trim_start();
    if let Some(rest) = trimmed.strip_prefix("---")
        && let Some(end) = rest.find("\n---")
    {
        let front: Vec<String> = rest[..end]
            .trim_matches('\n')
            .lines()
            .map(|l| l.to_string())
            .collect();
        // Pula a linha de fechamento inteira (`---...\n`), preservando o corpo intacto.
        let after = &rest[end + 1..];
        let body_start = after.find('\n').map(|i| i + 1).unwrap_or(after.len());
        let body = after[body_start..].trim_start_matches('\n').to_string();
        let desc = front
            .iter()
            .find_map(|l| {
                l.trim_start()
                    .strip_prefix("description:")
                    .map(|v| v.trim().trim_matches(['"', '\'']).to_string())
            })
            .unwrap_or_default();
        return (front, desc, body);
    }
    (Vec::new(), String::new(), content.to_string())
}

/// Skills do usuário — nome + descrição. UNIVERSAL: varre ~/.claude/skills E
/// ~/.claude/.agents/skills (skills do Codex), dedupando por nome. Só conta pastas com SKILL.md.
/// ponytail: scan mínimo próprio; o loader "de verdade" vive no crate `agent`, que o
/// notebook não pode importar (ciclo). Extrair pra um crate comum se surgir 3º uso.
fn load_user_skills() -> Vec<(String, String)> {
    let home = paths::home_dir().join(".claude");
    let roots = [home.join("skills"), home.join(".agents").join("skills")];
    let mut seen = HashSet::new();
    let mut skills: Vec<(String, String)> = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for e in entries.flatten() {
            if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let dir_name = e.file_name().to_string_lossy().to_string();
            if !seen.insert(dir_name.clone()) {
                continue; // já visto (skills/ tem prioridade sobre .agents/skills)
            }
            let Ok(text) = std::fs::read_to_string(e.path().join("SKILL.md")) else {
                continue;
            };
            let desc = text
                .strip_prefix("---")
                .and_then(|rest| rest.find("\n---").map(|end| &rest[..end]))
                .and_then(|front| {
                    front.lines().find_map(|l| {
                        l.trim()
                            .strip_prefix("description:")
                            .map(|v| v.trim().trim_matches(['"', '\'']).to_string())
                    })
                })
                .unwrap_or_default();
            skills.push((dir_name, desc));
        }
    }
    skills.sort_by(|a, b| a.0.cmp(&b.0));
    skills
}

/// Nomes dos MCP servers do config UNIVERSAL (~/.claude/mcp-configs/mcp-servers.json) —
/// pra a sidebar mostrar TODOS os MCPs do ecossistema, não só os configurados no momor.
fn load_mcp_config_names() -> Vec<String> {
    let path = paths::home_dir()
        .join(".claude")
        .join("mcp-configs")
        .join("mcp-servers.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let mut names: Vec<String> = json
        .get("mcpServers")
        .and_then(|v| v.as_object())
        .map(|obj| obj.keys().cloned().collect())
        .unwrap_or_default();
    names.sort();
    names
}

fn mcp_config_path() -> std::path::PathBuf {
    paths::home_dir()
        .join(".claude")
        .join("mcp-configs")
        .join("mcp-servers.json")
}

fn mcp_name_candidates(name: &str) -> Vec<String> {
    let stripped = name.strip_prefix("mcp-server-").unwrap_or(name);
    let prefixed = format!("mcp-server-{stripped}");
    let mut names = vec![name.to_string(), stripped.to_string(), prefixed];
    names.dedup();
    names
}

fn mcp_name_matches(left: &str, right: &str) -> bool {
    mcp_name_candidates(left)
        .into_iter()
        .any(|candidate| candidate == right)
        || mcp_name_candidates(right)
            .into_iter()
            .any(|candidate| candidate == left)
}

fn find_momor_mcp_settings(
    name: &str,
    cx: &App,
) -> Option<(Arc<str>, project::project_settings::ContextServerSettings)> {
    let settings = &ProjectSettings::get_global(cx).context_servers;
    mcp_name_candidates(name).into_iter().find_map(|candidate| {
        settings
            .get(candidate.as_str())
            .cloned()
            .map(|settings| (Arc::<str>::from(candidate), settings))
    })
}

fn load_mcp_config_entry(name: &str) -> Option<(String, serde_json::Value)> {
    let text = std::fs::read_to_string(mcp_config_path()).ok()?;
    let json = serde_json_lenient::from_str::<serde_json::Value>(&text).ok()?;
    let servers = json.get("mcpServers")?.as_object()?;
    mcp_name_candidates(name).into_iter().find_map(|candidate| {
        servers
            .get(&candidate)
            .cloned()
            .map(|value| (candidate, value))
    })
}

fn format_one_mcp_json(name: &str, value: impl serde::Serialize) -> String {
    serde_json::to_string_pretty(&serde_json::json!({ name: value }))
        .unwrap_or_else(|_| format!("{{\n  \"{name}\": {{}}\n}}"))
}

fn parse_one_mcp_value(text: &str) -> anyhow::Result<(String, serde_json::Value)> {
    let value: HashMap<String, serde_json::Value> = serde_json_lenient::from_str(text)?;
    if value.len() != 1 {
        anyhow::bail!("esperado exatamente um MCP");
    }
    Ok(value.into_iter().next().unwrap())
}

fn parse_one_mcp_settings(
    text: &str,
) -> anyhow::Result<(Arc<str>, settings::ContextServerSettingsContent)> {
    let value: HashMap<String, settings::ContextServerSettingsContent> =
        serde_json_lenient::from_str(text)?;
    if value.len() != 1 {
        anyhow::bail!("esperado exatamente um MCP");
    }
    let (name, settings) = value.into_iter().next().unwrap();
    Ok((Arc::<str>::from(name), settings))
}

fn save_mcp_config_entry(
    original_name: &str,
    new_name: String,
    value: serde_json::Value,
) -> anyhow::Result<()> {
    let path = mcp_config_path();
    let mut root = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json_lenient::from_str::<serde_json::Value>(&text).ok())
        .unwrap_or_else(|| serde_json::json!({ "mcpServers": {} }));

    if !root.is_object() {
        root = serde_json::json!({ "mcpServers": {} });
    }
    let root_object = root.as_object_mut().unwrap();
    root_object
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    let servers = root_object
        .get_mut("mcpServers")
        .and_then(|value| value.as_object_mut())
        .ok_or_else(|| anyhow::anyhow!("mcpServers precisa ser um objeto"))?;

    for candidate in mcp_name_candidates(original_name) {
        if candidate != new_name {
            servers.remove(&candidate);
        }
    }
    servers.insert(new_name, value);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format!("{}\n", serde_json::to_string_pretty(&root)?))?;
    Ok(())
}

fn search_empty_state(query: String) -> gpui::AnyElement {
    v_flex()
        .w_full()
        .items_center()
        .justify_center()
        .gap_1()
        .px_3()
        .py_6()
        .child(
            Icon::new(IconName::FolderSearch)
                .size(IconSize::Medium)
                .color(Color::Muted),
        )
        .child(
            div().w_full().min_w_0().text_center().child(
                Label::new(format!("Não encontrado \"{query}\""))
                    .color(Color::Muted)
                    .size(LabelSize::Small)
                    .truncate(),
            ),
        )
        .into_any_element()
}

#[cfg(test)]
mod mcp_config_tests {
    use super::*;

    #[test]
    fn mcp_name_matching_accepts_registry_prefix() {
        assert!(mcp_name_matches("mcp-server-context7", "context7"));
        assert!(mcp_name_matches("context7", "mcp-server-context7"));
        assert!(!mcp_name_matches("context7", "browser-use"));
    }

    #[test]
    fn parse_one_mcp_value_requires_one_entry() {
        assert!(parse_one_mcp_value(r#"{"context7":{"command":"npx"}}"#).is_ok());
        assert!(parse_one_mcp_value(r#"{"a":{},"b":{}}"#).is_err());
    }
}

fn empty_state(text: &str, _cx: &App) -> gpui::AnyElement {
    h_flex()
        .flex_1()
        .h_full()
        .items_center()
        .justify_center()
        .p_8()
        .child(
            Label::new(text.to_string())
                .color(Color::Muted)
                .size(LabelSize::Small),
        )
        .into_any_element()
}

impl Focusable for NotebookPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for NotebookPanel {}

impl Render for NotebookPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Reabre o item da sessão anterior no 1º render (aqui há `window`).
        if !self.restored {
            self.restored = true;
            self.restore_open(window, cx);
        }
        // Caixas de pesquisa (precisam de window → criadas no 1º render).
        if self.mcp_search.is_none() {
            let (t, ts) = self.make_search("Pesquisar notas…", window, cx);
            let (m, ms) = self.make_search("Pesquisar MCPs…", window, cx);
            let (s, ss) = self.make_search("Pesquisar skills…", window, cx);
            self.tree_search = Some(t);
            self.mcp_search = Some(m);
            self.skill_search = Some(s);
            self._search_subs = vec![ts, ms, ss];
        }
        // Re-escaneia as skills do disco a cada ~2s → skills recém-instaladas (pelo agente via
        // terminal) aparecem sozinhas, sem reiniciar. Barato (listar dir + ler frontmatter).
        if self.last_skill_scan.elapsed().as_secs() >= 2 {
            self.last_skill_scan = std::time::Instant::now();
            let fresh = load_user_skills();
            if fresh != self.skills {
                self.skills = fresh;
            }
            let fresh_mcps = load_mcp_config_names();
            if fresh_mcps != self.mcp_config_names {
                self.mcp_config_names = fresh_mcps;
            }
        }
        // ponytail: largura "hug" — sidebar (ajustável) + conteúdo SÓ quando há nota/
        // reunião selecionada. Sem seleção, fica só a sidebar (e o chat ao lado).
        // ponytail: preenche a coluna (que o resize do chat controla). Sem nota
        // selecionada = só a árvore (flex_1). Com nota = árvore (largura ajustável) +
        // conteúdo (flex_1).
        let has_selection = !matches!(self.selection, Selection::None);
        h_flex()
            .key_context("NotebookPanel")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().panel_background)
            // A alça da sidebar (só aparece com conteúdo ao lado) ajusta a largura dela.
            .on_drag_move(cx.listener(
                |this, e: &gpui::DragMoveEvent<DraggedSidebarHandle>, _window, cx| {
                    // Máximo relativo à largura da coluna: a nota nunca fica com menos
                    // de ~380px (senão colapsa e o texto quebra em 1 caractere).
                    let avail = workspace::notebook_width(cx);
                    let max = (avail - 380.0).max(180.0);
                    this.sidebar_width = f32::from(e.event.position.x).clamp(160.0, max);
                    cx.notify();
                },
            ))
            // Alça das seções MCPS/SKILLS: arrastar pra cima aumenta a altura (delta do Y).
            .on_drag_move(cx.listener(
                |this, e: &gpui::DragMoveEvent<DraggedAbilitiesHandle>, _window, cx| {
                    let y = f32::from(e.event.position.y);
                    if let Some(last) = this.abilities_drag {
                        this.abilities_height =
                            (this.abilities_height + (last - y)).clamp(140.0, 720.0);
                    }
                    this.abilities_drag = Some(y);
                    cx.notify();
                },
            ))
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, _| this.abilities_drag = None),
            )
            .child(self.render_sidebar(has_selection, window, cx))
            .when(has_selection, |this| this.child(self.render_content(cx)))
    }
}

impl Panel for NotebookPanel {
    fn persistent_name() -> &'static str {
        "NotebookPanel"
    }

    fn panel_key() -> &'static str {
        "NotebookPanel"
    }

    fn position(&self, _window: &Window, _cx: &App) -> DockPosition {
        DockPosition::Left
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }

    fn set_position(
        &mut self,
        _position: DockPosition,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    fn default_size(&self, _window: &Window, _cx: &App) -> gpui::Pixels {
        self.width.unwrap_or(px(NOTEBOOK_PANEL_WIDTH))
    }

    fn icon(&self, _window: &Window, _cx: &App) -> Option<IconName> {
        Some(IconName::Book)
    }

    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some("Notas e reuniões")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(ToggleFocus)
    }

    fn activation_priority(&self) -> u32 {
        5
    }
}
