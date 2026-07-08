//! Painel lateral "Notion": árvore de pastas → notas (markdown) + reuniões
//! (transcrições). Alterna com o chat pelo ícone de sidebar na titlebar.

use std::sync::Arc;

use crate::store::{Folder, Meeting, Note, NotebookDb};
use editor::{Editor, EditorEvent};
use gpui::{
    Action, App, Context, Entity, EventEmitter, FocusHandle, Focusable, Subscription, WeakEntity,
    Window, actions, prelude::*,
};
use language::LanguageRegistry;
use project::context_server_store::ContextServerStatus;
use ui::{Divider, Label, Tooltip, prelude::*};
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
    selection: Selection,
    /// Editor de blocos da nota aberta (estilo Notion).
    note_doc: Option<Entity<crate::note_editor::NoteDoc>>,
    /// Título editável da reunião aberta (permite renomear).
    meeting_title: Option<Entity<Editor>>,
    _meeting_title_sub: Option<Subscription>,
    _stt_subscription: Subscription,
    /// Largura ajustável da sidebar (árvore).
    sidebar_width: f32,
    width: Option<gpui::Pixels>,
    /// Skills do usuário (~/.claude/skills): nome + descrição. Universal, listadas
    /// no rodapé do painel independente de provedor/agente.
    skills: Vec<(String, String)>,
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
            selection: Selection::None,
            note_doc: None,
            meeting_title: None,
            _meeting_title_sub: None,
            _stt_subscription: stt_subscription,
            sidebar_width: 260.0,
            width: None,
            skills: load_user_skills(),
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
        // Clica de novo pra desmarcar (volta a criar na raiz).
        if self.selected_folder.as_deref() == Some(folder_id.as_str()) {
            self.selected_folder = None;
        } else {
            self.selected_folder = Some(folder_id);
        }
        cx.notify();
    }

    fn select_note(&mut self, note_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(note) = self.notes.iter().find(|n| n.id == note_id).cloned() else {
            return;
        };
        self.selection = Selection::Note(note_id);
        // Editor de blocos (estilo Notion) — cria e salva por conta própria.
        self.meeting_title = None;
        self._meeting_title_sub = None;
        let languages = self.language_registry.clone();
        let doc = cx.new(|cx| crate::note_editor::NoteDoc::new(note, languages, window, cx));
        self.note_doc = Some(doc);
        cx.notify();
    }

    fn select_meeting(&mut self, meeting_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.note_doc = None;
        let title = self
            .meetings
            .iter()
            .find(|m| m.id == meeting_id)
            .map(|m| m.title.clone())
            .unwrap_or_default();
        self.selection = Selection::Meeting(meeting_id);

        // Título editável (renomeia a reunião ao editar).
        let editor = cx.new(|cx| {
            let mut e = Editor::single_line(window, cx);
            e.set_text(title, window, cx);
            e
        });
        let sub = cx.subscribe(&editor, |this, editor, ev, cx| {
            if matches!(ev, EditorEvent::Edited { .. }) {
                let Selection::Meeting(id) = this.selection.clone() else {
                    return;
                };
                let title = editor.read(cx).text(cx);
                let summary = this
                    .meetings
                    .iter()
                    .find(|m| m.id == id)
                    .map(|m| m.summary.clone())
                    .unwrap_or_default();
                let db = NotebookDb::global(cx);
                cx.spawn(async move |this, cx| {
                    db.update_meeting_summary(id, title, summary).await.log_err();
                    this.update(cx, |this, cx| this.reload(cx)).ok();
                })
                .detach();
            }
        });
        self.meeting_title = Some(editor);
        self._meeting_title_sub = Some(sub);
        cx.notify();
    }

    fn move_item(&mut self, item: &DraggedNotebookItem, target_folder: Option<String>, cx: &mut Context<Self>) {
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
            Selection::Note(s) | Selection::Meeting(s) if *s == id
        );
        if selected {
            self.selection = Selection::None;
            self.note_doc = None;
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

    fn render_sidebar(&self, has_selection: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let selected_id = match &self.selection {
            Selection::Note(id) | Selection::Meeting(id) => Some(id.clone()),
            Selection::None => None,
        };
        // ponytail: computa as linhas (owned) por DFS antes dos listeners, senão o
        // empréstimo de self/cx colide dentro do .map.
        let mut rows = Vec::new();
        flatten_rows(&self.folders, &self.notes, &self.meetings, None, 0, &mut rows);
        let selected_folder = self.selected_folder.clone();
        let selected_bg = cx.theme().colors().element_selected;
        let hover_bg = cx.theme().colors().element_hover;

        v_flex()
            .relative()
            .h_full()
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
            .child(
                v_flex()
                    .id("notebook-tree")
                    .flex_1()
                    .overflow_y_scroll()
                    .p_1()
                    .gap_0p5()
                    // ponytail: soltar na área vazia da árvore = mover pra raiz.
                    .on_drop::<DraggedNotebookItem>(cx.listener(|this, item, _, cx| {
                        this.move_item(item, None, cx)
                    }))
                    .children(rows.into_iter().map(|row| match row {
                        Row::Folder { id, name, depth } => {
                            let sel_id = id.clone();
                            let del_id = id.clone();
                            let drop_folder = id.clone();
                            let is_selected = selected_folder.as_deref() == Some(id.as_str());
                            let drag = DraggedNotebookItem {
                                kind: "folder",
                                id: id.clone(),
                                label: name.clone(),
                            };
                            let on_drop: DropHandler =
                                Box::new(cx.listener(move |this, item: &DraggedNotebookItem, _, cx| {
                                    this.move_item(item, Some(drop_folder.clone()), cx)
                                }));
                            tree_row(
                                SharedString::from(format!("folder-{id}")),
                                IconName::Folder,
                                name,
                                is_selected,
                                depth,
                                selected_bg,
                                hover_bg,
                                drag,
                                Some(on_drop),
                                cx.listener(move |this, _, _, cx| {
                                    this.select_folder(sel_id.clone(), cx)
                                }),
                                cx.listener(move |this, _, _, cx| {
                                    this.delete_item("folder", del_id.clone(), cx)
                                }),
                            )
                            .into_any_element()
                        }
                        Row::Note { id, title, depth } => {
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
                                title,
                                is_selected,
                                depth,
                                selected_bg,
                                hover_bg,
                                drag,
                                None,
                                cx.listener(move |this, _, window, cx| {
                                    this.select_note(open_id.clone(), window, cx)
                                }),
                                cx.listener(move |this, _, _, cx| {
                                    this.delete_item("note", del_id.clone(), cx)
                                }),
                            )
                            .into_any_element()
                        }
                        Row::Meeting { id, title, depth } => {
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
                                title,
                                is_selected,
                                depth,
                                selected_bg,
                                hover_bg,
                                drag,
                                None,
                                cx.listener(move |this, _, window, cx| {
                                    this.select_meeting(open_id.clone(), window, cx)
                                }),
                                cx.listener(move |this, _, _, cx| {
                                    this.delete_item("meeting", del_id.clone(), cx)
                                }),
                            )
                            .into_any_element()
                        }
                    })),
            )
            .child(self.render_ability_sections(cx))
    }

    /// Rodapé do painel: MCPS e SKILLS — sempre visíveis, universais (não dependem
    /// do provedor/agente). Espelha o sidebar do momor.
    fn render_ability_sections(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // MCP servers configurados (nome + cor do status).
        let mcps: Vec<(String, Color)> = self
            ._workspace
            .upgrade()
            .map(|ws| {
                let store = ws.read(cx).project().read(cx).context_server_store();
                let store = store.read(cx);
                store
                    .configured_server_ids()
                    .into_iter()
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

        v_flex()
            .flex_none()
            .child(Divider::horizontal())
            .child(ability_header(
                "MCPS",
                mcps.len(),
                IconButton::new("add-mcp", IconName::Plus)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text("Configurar MCP servers (settings)"))
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.reveal_path(paths::settings_file());
                    }))
                    .into_any_element(),
            ))
            .children(
                mcps.into_iter()
                    .map(|(name, color)| ability_row(name, color, cx)),
            )
            .child(ability_header(
                "SKILLS",
                self.skills.len(),
                IconButton::new("add-skill", IconName::Plus)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text("Abrir pasta de skills (~/.claude/skills)"))
                    .on_click(cx.listener(|_, _, _, cx| {
                        let dir = paths::home_dir().join(".claude").join("skills");
                        std::fs::create_dir_all(&dir).ok();
                        cx.reveal_path(&dir);
                    }))
                    .into_any_element(),
            ))
            .children(
                self.skills
                    .clone()
                    .into_iter()
                    .map(|(name, _)| ability_row(name, Color::Success, cx)),
            )
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
            Selection::Meeting(id) => {
                let Some(meeting) = self.meetings.iter().find(|m| &m.id == id).cloned() else {
                    return empty_state("Reunião não encontrada", cx);
                };
                v_flex()
                    .id("meeting-content")
                    .flex_1()
                    .h_full()
                    .p_6()
                    .gap_3()
                    .overflow_y_scroll()
                    // Título editável (renomeia).
                    .child(
                        div()
                            .text_size(px(24.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .when_some(self.meeting_title.clone(), |el, editor| el.child(editor)),
                    )
                    .child(
                        Label::new(format_meeting_date(&meeting.date))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .when(!meeting.summary.is_empty(), |this| {
                        this.child(section_label("Resumo"))
                            .child(div().text_ui(cx).child(meeting.summary.clone()))
                    })
                    .child(section_label("Transcrição"))
                    .child(render_transcript(&meeting.transcript, cx))
                    .into_any_element()
            }
            Selection::None => empty_state(
                "Suas notas e reuniões aparecem aqui. Fale numa call para gerar uma transcrição.",
                cx,
            ),
        }
    }
}

/// Data da reunião em formato simples (dd/mm/aaaa hh:mm) a partir do RFC3339.
fn format_meeting_date(date: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(date)
        .map(|d| d.format("%d/%m/%Y %H:%M").to_string())
        .unwrap_or_else(|_| date.to_string())
}

/// Rótulo de seção (Resumo / Transcrição).
fn section_label(text: &str) -> gpui::AnyElement {
    v_flex()
        .child(Divider::horizontal())
        .child(
            div().pt_1().child(
                Label::new(text.to_uppercase())
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            ),
        )
        .into_any_element()
}

/// Transcrição com falantes distintos: "Você:" (accent) e "Interlocutor:" (verde), cada
/// fala numa linha com o nome numa coluna alinhada. Bem mais fácil de ler que o texto cru.
fn render_transcript(transcript: &str, cx: &App) -> gpui::AnyElement {
    v_flex()
        .gap_1p5()
        .children(transcript.lines().filter(|l| !l.trim().is_empty()).map(|line| {
            match line.split_once(": ") {
                Some((speaker, text)) => {
                    let color = if speaker.trim() == "Você" {
                        Color::Accent
                    } else {
                        Color::Success
                    };
                    h_flex()
                        .gap_2()
                        .items_start()
                        .child(
                            div().flex_none().w(px(90.)).child(
                                Label::new(speaker.trim().to_string())
                                    .size(LabelSize::Small)
                                    .color(color),
                            ),
                        )
                        .child(div().flex_1().text_ui(cx).child(text.to_string()))
                        .into_any_element()
                }
                None => div().text_ui(cx).child(line.to_string()).into_any_element(),
            }
        }))
        .into_any_element()
}

/// Payload arrastado (nota/pasta/reunião) pra soltar dentro de uma pasta OU no chat.
#[derive(Clone)]
pub struct DraggedNotebookItem {
    pub kind: &'static str,
    pub id: String,
    pub label: String,
}

/// Conteúdo textual de um item, pra injetar como contexto ao soltar no chat.
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
            for n in notes.iter().filter(|n| n.folder_id.as_deref() == Some(item.id.as_str())) {
                out.push_str(&format!("# {}\n\n{}\n\n", n.title, n.content));
            }
            for m in meetings.iter().filter(|m| m.folder_id.as_deref() == Some(item.id.as_str())) {
                out.push_str(&format!("# {}\n\n{}\n\n", m.title, m.transcript));
            }
            out
        }
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

/// Linha achatada da árvore (com profundidade pra indentar).
enum Row {
    Folder { id: String, name: String, depth: usize },
    Note { id: String, title: String, depth: usize },
    Meeting { id: String, title: String, depth: usize },
}

/// DFS: pastas → subpastas/notas/reuniões indentadas; itens da raiz por último.
fn flatten_rows(
    folders: &[Folder],
    notes: &[Note],
    meetings: &[Meeting],
    parent: Option<&str>,
    depth: usize,
    out: &mut Vec<Row>,
) {
    for folder in folders
        .iter()
        .filter(|f| f.parent_id.as_deref() == parent)
    {
        out.push(Row::Folder {
            id: folder.id.clone(),
            name: folder.name.clone(),
            depth,
        });
        flatten_rows(folders, notes, meetings, Some(&folder.id), depth + 1, out);
        for note in notes
            .iter()
            .filter(|n| n.folder_id.as_deref() == Some(folder.id.as_str()))
        {
            out.push(Row::Note {
                id: note.id.clone(),
                title: note.title.clone(),
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
                depth: depth + 1,
            });
        }
    }
    if parent.is_none() {
        for note in notes.iter().filter(|n| n.folder_id.is_none()) {
            out.push(Row::Note {
                id: note.id.clone(),
                title: note.title.clone(),
                depth: 0,
            });
        }
        for meeting in meetings.iter().filter(|m| m.folder_id.is_none()) {
            out.push(Row::Meeting {
                id: meeting.id.clone(),
                title: meeting.title.clone(),
                depth: 0,
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn tree_row(
    id: impl Into<gpui::ElementId>,
    icon: IconName,
    label: String,
    is_selected: bool,
    indent: usize,
    selected_bg: gpui::Hsla,
    hover_bg: gpui::Hsla,
    drag: DraggedNotebookItem,
    on_drop_folder: Option<DropHandler>,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    on_delete: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let group = SharedString::from("tree-row");
    let delete_id = ElementId::Name(SharedString::from("del"));
    let drop_bg = selected_bg;
    h_flex()
        .id(id.into())
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
        .on_drag(drag, |d, _, _, cx| cx.new(|_| d.clone()))
        .when_some(on_drop_folder, |el, on_drop| {
            el.drag_over::<DraggedNotebookItem>(move |el, _, _, _| el.bg(drop_bg))
                .on_drop::<DraggedNotebookItem>(move |item, window, cx| {
                    on_drop(item, window, cx)
                })
        })
        .child(Icon::new(icon).size(IconSize::Small).color(Color::Muted))
        .child(Label::new(label).size(LabelSize::Small).truncate())
        .child(div().flex_1())
        // ponytail: lixeira aparece no hover da linha
        .child(
            IconButton::new(delete_id, IconName::Trash)
                .icon_size(IconSize::XSmall)
                .icon_color(Color::Muted)
                .visible_on_hover(group)
                .on_click(move |ev, window, cx| {
                    cx.stop_propagation();
                    on_delete(ev, window, cx);
                }),
        )
        .on_click(move |ev, window, cx| on_click(ev, window, cx))
}

/// Cabeçalho de uma seção do rodapé (MCPS/SKILLS): rótulo + contagem + botão `+`.
fn ability_header(label: &'static str, count: usize, add_button: gpui::AnyElement) -> impl IntoElement {
    h_flex()
        .px_2()
        .py_1()
        .justify_between()
        .items_center()
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
                .child(
                    Label::new(count.to_string())
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
        )
        .child(add_button)
}

/// Linha de um MCP/skill: dot de status colorido + nome.
fn ability_row(name: String, dot: Color, cx: &App) -> impl IntoElement {
    h_flex()
        .h(px(24.))
        .px_2()
        .gap_1p5()
        .items_center()
        .child(div().size(px(6.)).rounded_full().bg(dot.color(cx)))
        .child(Label::new(name).size(LabelSize::Small))
}

/// Skills do usuário (~/.claude/skills/*/SKILL.md) — nome + descrição.
/// ponytail: scan mínimo próprio; o loader "de verdade" vive no crate `agent`, que o
/// notebook não pode importar (ciclo). Extrair pra um crate comum se surgir 3º uso.
fn load_user_skills() -> Vec<(String, String)> {
    let root = paths::home_dir().join(".claude").join("skills");
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut skills: Vec<(String, String)> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| {
            let text = std::fs::read_to_string(e.path().join("SKILL.md")).ok()?;
            let rest = text.strip_prefix("---")?;
            let end = rest.find("\n---")?;
            let (mut name, mut desc) = (None, None);
            for line in rest[..end].lines() {
                let line = line.trim();
                if let Some(v) = line.strip_prefix("name:") {
                    name = Some(v.trim().trim_matches(['"', '\'']).to_string());
                } else if let Some(v) = line.strip_prefix("description:") {
                    desc = Some(v.trim().trim_matches(['"', '\'']).to_string());
                }
            }
            Some((name?, desc.unwrap_or_default()))
        })
        .collect();
    skills.sort_by(|a, b| a.0.cmp(&b.0));
    skills
}

fn empty_state(text: &str, cx: &App) -> gpui::AnyElement {
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            .child(self.render_sidebar(has_selection, cx))
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

    fn set_position(&mut self, _position: DockPosition, _window: &mut Window, _cx: &mut Context<Self>) {
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
