//! Editor de blocos estilo Notion (WYSIWYG) pras notas. Cada bloco é um `Editor`
//! auto-height estilizado pelo tipo (título grande, lista, tarefa com checkbox, etc.) —
//! o markdown NÃO aparece cru. Enter cria bloco; `/` abre um menu custom com seções
//! (Títulos / Blocos básicos / Mídia). Persiste como markdown (round-trip) pra a IA/tool.
//!
//! ponytail: v1 — Enter não divide o texto no cursor, sem drag pra reordenar, sem toolbar
//! de negrito inline, sem Tabela (grid de células editáveis é a próxima feature grande).

use std::path::PathBuf;
use std::time::Duration;

use editor::{Editor, EditorEvent};
use gpui::{
    App, Context, Entity, FocusHandle, Focusable, FontWeight, PathPromptOptions, ScrollHandle,
    Subscription, Task, TextStyleRefinement, Window, actions, img, prelude::*,
};
use std::sync::Arc;

use language::LanguageRegistry;
use language::language_settings::SoftWrap;
use rope::Point;
use settings::Settings as _;
use ui::prelude::*;
use util::ResultExt as _;

use crate::store::{Note, NotebookDb};

/// Payload arrastado pra reordenar blocos.
#[derive(Clone)]
struct DraggedBlock(usize);
impl Render for DraggedBlock {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// Payload da alça de redimensionar imagem (block_id).
#[derive(Clone)]
struct DraggedImageHandle(usize);
impl Render for DraggedImageHandle {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

actions!(
    notebook,
    [
        /// Enter numa nota: cria um bloco novo.
        SplitBlock,
        /// Backspace no início de um bloco vazio: remove.
        MergeBlockBackward,
        /// Enter com o menu `/` aberto: escolhe o item selecionado.
        SlashConfirm,
        /// Esc: fecha o menu `/`.
        SlashDismiss,
        /// Seta pra baixo: próximo item do menu `/`.
        SlashNext,
        /// Seta pra cima: item anterior do menu `/`.
        SlashPrev,
        /// Tab: aninha a linha (indenta um nível).
        Indent,
        /// Shift+Tab: desaninha a linha (um nível).
        Outdent,
    ]
);

const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

const COVERS: &[(&str, u32)] = &[
    ("1", 0x6366f1),
    ("2", 0xec4899),
    ("3", 0xf59e0b),
    ("4", 0x10b981),
    ("5", 0x0ea5e9),
    ("6", 0x8b5cf6),
];

/// Emojis por categoria pro seletor de ícone (grade rolável, estilo Notion).
const EMOJI_CATS: &[(&str, &[&str])] = &[
    (
        "Sorrisos",
        &[
            "😀", "😃", "😄", "😁", "😆", "😅", "🤣", "😂", "🙂", "🙃", "😉", "😊", "😇", "🥰",
            "😍", "🤩", "😘", "😋", "😜", "🤪", "🤨", "🧐", "🤓", "😎", "🥳", "😏", "😌", "😴",
        ],
    ),
    (
        "Gestos",
        &[
            "👍", "👎", "👌", "✌️", "🤞", "🤙", "👋", "🙌", "👏", "🙏", "💪", "🤝", "✍️", "👀",
        ],
    ),
    (
        "Natureza",
        &[
            "🌱", "🌿", "🍀", "🌵", "🌳", "🌲", "🌴", "🌷", "🌸", "🌹", "🌺", "🌻", "🌼", "🐛",
            "🐝", "🦋", "🐞", "🐢", "🐙", "🦄", "🐱", "🐶", "🦊", "🐼",
        ],
    ),
    (
        "Comida",
        &[
            "🍎", "🍊", "🍋", "🍉", "🍇", "🍓", "🍒", "🍑", "🥭", "🍍", "🥥", "🍔", "🍕", "🌮",
            "🍰", "🍩", "🍪", "☕", "🍺", "🍷",
        ],
    ),
    (
        "Objetos",
        &[
            "📝", "📄", "📌", "📎", "📚", "📖", "📅", "⭐", "🔥", "💡", "✅", "❌", "⚠️", "🎯",
            "🚀", "🧠", "💬", "🔔", "🔑", "🔒", "💻", "📱", "🎨", "🎵", "🎲", "🏆",
        ],
    ),
    (
        "Símbolos",
        &[
            "❤️", "🧡", "💛", "💚", "💙", "💜", "🖤", "🤍", "💯", "✨", "🎉", "🎊", "⚡", "💥",
        ],
    ),
];

/// Linguagens do seletor do bloco de código (nomes que a LanguageRegistry entende).
const LANGS: &[&str] = &[
    "Rust",
    "Python",
    "JavaScript",
    "TypeScript",
    "TSX",
    "JSON",
    "HTML",
    "CSS",
    "Bash",
    "Go",
    "C",
    "C++",
    "Java",
    "SQL",
    "Markdown",
    "YAML",
    "TOML",
];

/// Itens do menu `/`: (seção, rótulo, descrição, ícone, tipo).
const MENU: &[(&str, &str, &str, IconName, BlockKind)] = &[
    ("Títulos", "Título 1", "Título de seção grande", IconName::Hash, BlockKind::H1),
    ("Títulos", "Título 2", "Título de seção média", IconName::Hash, BlockKind::H2),
    ("Títulos", "Título 3", "Título de subseção", IconName::Hash, BlockKind::H3),
    ("Blocos básicos", "Texto", "Texto simples", IconName::Menu, BlockKind::Paragraph),
    ("Blocos básicos", "Lista com marcadores", "Lista com marcadores", IconName::ListTree, BlockKind::Bullet),
    ("Blocos básicos", "Lista numerada", "Lista ordenada por números", IconName::ListTree, BlockKind::Numbered),
    ("Blocos básicos", "Tarefa", "Lista com caixas de seleção", IconName::ListTodo, BlockKind::Todo(false)),
    ("Blocos básicos", "Citação", "Destaque uma citação", IconName::Quote, BlockKind::Quote),
    ("Blocos básicos", "Código", "Bloco de código", IconName::Code, BlockKind::Code),
    ("Blocos básicos", "Divisor", "Separa visualmente os blocos", IconName::Dash, BlockKind::Divider),
    ("Mídia", "Imagem", "Escolha uma imagem do computador", IconName::Image, BlockKind::Image),
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    H1,
    H2,
    H3,
    Bullet,
    Numbered,
    Todo(bool),
    Quote,
    Code,
    Divider,
    Image,
}

struct Block {
    id: usize,
    kind: BlockKind,
    editor: Entity<Editor>,
    /// Payload de mídia (ex.: caminho da imagem).
    data: Option<String>,
    /// Nível de indentação (aninhamento de listas). 0 = topo.
    indent: usize,
    /// Largura da imagem em px (None = automática).
    width: Option<f32>,
    /// Linguagem do bloco de código (pro syntax highlight). None = texto puro.
    lang: Option<String>,
    _sub: Subscription,
}

#[derive(Clone)]
struct SlashState {
    block_id: usize,
    query: String,
    selected: usize,
}

pub struct NoteDoc {
    note_id: String,
    languages: Arc<LanguageRegistry>,
    icon: Option<String>,
    cover: Option<String>,
    icon_menu_open: bool,
    cover_menu_open: bool,
    title: Entity<Editor>,
    blocks: Vec<Block>,
    next_id: usize,
    slash: Option<SlashState>,
    /// Bloco de código com o menu de linguagem aberto.
    lang_menu: Option<usize>,
    /// Resize de imagem em andamento: (block_id, última posição x do mouse).
    img_resize: Option<(usize, f32)>,
    slash_scroll: ScrollHandle,
    focus_handle: FocusHandle,
    save_task: Option<Task<()>>,
    _title_sub: Subscription,
}

impl NoteDoc {
    pub fn new(
        note: Note,
        languages: Arc<LanguageRegistry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let title = cx.new(|cx| {
            let mut e = Editor::single_line(window, cx);
            e.set_text(note.title.clone(), window, cx);
            e.set_placeholder_text("Sem título", window, cx);
            e.set_text_style_refinement(TextStyleRefinement {
                font_size: Some(px(30.).into()),
                font_weight: Some(FontWeight::BOLD),
                ..Default::default()
            });
            e
        });
        let title_sub = cx.subscribe(&title, |this, _e, ev, cx| {
            if matches!(ev, EditorEvent::Edited { .. }) {
                this.schedule_save(cx);
            }
        });

        let mut this = Self {
            note_id: note.id,
            languages,
            icon: note.icon.filter(|s| !s.is_empty()),
            cover: note.cover.filter(|s| !s.is_empty()),
            icon_menu_open: false,
            cover_menu_open: false,
            title,
            blocks: Vec::new(),
            next_id: 0,
            slash: None,
            lang_menu: None,
            img_resize: None,
            slash_scroll: ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            save_task: None,
            _title_sub: title_sub,
        };
        for (kind, text, data, indent) in parse_blocks(&note.content) {
            let mut block = this.make_block(kind, &text, window, cx);
            block.indent = indent;
            match kind {
                // Imagem pode trazer a largura codificada com \0 (ver classify).
                BlockKind::Image => {
                    if let Some(d) = &data
                        && let Some((path, w)) = d.split_once('\u{0}')
                    {
                        block.data = Some(path.to_string());
                        block.width = w.parse().ok();
                    } else {
                        block.data = data;
                    }
                }
                // Código: data carrega a linguagem → aplica o highlight.
                BlockKind::Code => {
                    if let Some(lang) = data {
                        this.apply_code_language(block.editor.clone(), lang.clone(), cx);
                        block.lang = Some(lang);
                    }
                }
                _ => block.data = data,
            }
            this.blocks.push(block);
        }
        // Sempre há um parágrafo vazio no fim pra continuar escrevendo.
        let needs_trailing = this.blocks.last().map_or(true, |b| {
            b.kind != BlockKind::Paragraph || !b.editor.read(cx).text(cx).is_empty()
        });
        if needs_trailing {
            let trailing = this.make_block(BlockKind::Paragraph, "", window, cx);
            this.blocks.push(trailing);
        }
        this
    }

    fn make_block(
        &mut self,
        kind: BlockKind,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Block {
        let id = self.next_id;
        self.next_id += 1;
        let text = text.to_string();
        let refinement = block_style(kind, cx);
        let editor = cx.new(|cx| {
            let mut e = Editor::auto_height(1, 40, window, cx);
            e.set_soft_wrap_mode(SoftWrap::EditorWidth, cx);
            e.set_show_gutter(false, cx);
            e.set_show_wrap_guides(false, cx);
            if !text.is_empty() {
                e.set_text(text, window, cx);
            }
            e.set_text_style_refinement(refinement);
            e
        });
        let sub = cx.subscribe(&editor, move |this, editor, ev, cx| {
            if matches!(ev, EditorEvent::Edited { .. }) {
                this.schedule_save(cx);
                this.detect_slash(id, editor, cx);
            }
        });
        Block {
            id,
            kind,
            editor,
            data: None,
            indent: 0,
            width: None,
            lang: None,
            _sub: sub,
        }
    }

    /// Carrega a linguagem (async) e aplica no buffer do editor do bloco → syntax highlight.
    fn apply_code_language(&self, editor: Entity<Editor>, lang: String, cx: &mut Context<Self>) {
        let registry = self.languages.clone();
        cx.spawn(async move |_this, cx| {
            let language = registry.language_for_name(&lang).await.ok();
            cx.update(|cx| {
                if let Some(buffer) = editor.read(cx).buffer().read(cx).as_singleton() {
                    buffer.update(cx, |b, cx| {
                        b.set_language(language, cx);
                        b.set_language_registry(registry.clone());
                    });
                }
            });
        })
        .detach();
    }

    fn focused_index(&self, window: &Window, cx: &App) -> Option<usize> {
        self.blocks
            .iter()
            .position(|b| b.editor.focus_handle(cx).is_focused(window))
    }

    /// Detecta `/query` no fim do bloco pra abrir/fechar o menu.
    fn detect_slash(&mut self, block_id: usize, editor: Entity<Editor>, cx: &mut Context<Self>) {
        let text = editor.read(cx).text(cx);
        let query = text.rfind('/').and_then(|pos| {
            let before_ok = pos == 0 || text[..pos].ends_with([' ', '\n', '\t']);
            let after = &text[pos + 1..];
            (before_ok && !after.chars().any(|c| c.is_whitespace())).then(|| after.to_lowercase())
        });
        self.slash = query.map(|query| SlashState {
            block_id,
            query,
            selected: 0,
        });
        cx.notify();
    }

    fn slash_next(&mut self, _: &SlashNext, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(state) = &mut self.slash {
            let n = filtered_menu(&state.query).len();
            if n > 0 {
                state.selected = (state.selected + 1) % n;
                self.scroll_to_selected(cx);
            }
        }
    }

    fn slash_prev(&mut self, _: &SlashPrev, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(state) = &mut self.slash {
            let n = filtered_menu(&state.query).len();
            if n > 0 {
                state.selected = (state.selected + n - 1) % n;
                self.scroll_to_selected(cx);
            }
        }
    }

    /// Rola o menu `/` pra deixar o item selecionado visível. O índice do filho conta os
    /// cabeçalhos de seção (cada seção nova insere um cabeçalho antes dos itens).
    fn scroll_to_selected(&self, cx: &mut Context<Self>) {
        let Some(state) = &self.slash else { return };
        let items = filtered_menu(&state.query);
        let mut child = 0usize;
        let mut section = "";
        for (i, (sec, ..)) in items.iter().enumerate() {
            if *sec != section {
                section = sec;
                child += 1; // cabeçalho da seção
            }
            if i == state.selected {
                self.slash_scroll.scroll_to_item(child);
                break;
            }
            child += 1;
        }
        cx.notify();
    }

    fn split_block(&mut self, _: &SplitBlock, window: &mut Window, cx: &mut Context<Self>) {
        let Some(idx) = self.focused_index(window, cx) else {
            return;
        };
        let cont = continuation_kind(self.blocks[idx].kind);
        let is_empty = self.blocks[idx].editor.update(cx, |e, cx| e.text(cx).is_empty());

        // Enter num item de lista/citação/código VAZIO sai do modo (vira parágrafo),
        // igual ao Notion — em vez de criar mais um item vazio.
        if is_empty && cont != BlockKind::Paragraph {
            let editor = self.blocks[idx].editor.clone();
            editor.update(cx, |e, _| e.set_text_style_refinement(style_for(BlockKind::Paragraph)));
            self.blocks[idx].kind = BlockKind::Paragraph;
            cx.notify();
            self.schedule_save(cx);
            return;
        }

        // Novo bloco continua o mesmo tipo E o mesmo nível de aninhamento.
        let indent = self.blocks[idx].indent;
        let mut new = self.make_block(cont, "", window, cx);
        new.indent = indent;
        let handle = new.editor.focus_handle(cx);
        self.blocks.insert(idx + 1, new);
        window.focus(&handle, cx);
        cx.notify();
        self.schedule_save(cx);
    }

    fn merge_backward(&mut self, _: &MergeBlockBackward, window: &mut Window, cx: &mut Context<Self>) {
        let Some(idx) = self.focused_index(window, cx) else {
            return;
        };
        let at_start = self.blocks[idx].editor.update(cx, |e, cx| {
            let snapshot = e.snapshot(window, cx);
            let head = e.selections.newest::<Point>(&snapshot.display_snapshot).head();
            head.row == 0 && head.column == 0
        });
        if !at_start {
            self.blocks[idx].editor.update(cx, |e, cx| {
                e.backspace(&editor::actions::Backspace, window, cx)
            });
            return;
        }

        // No 1º caractere: se está aninhado, desce um nível primeiro (1.1 → nível de cima).
        if self.blocks[idx].indent > 0 {
            self.blocks[idx].indent -= 1;
            cx.notify();
            self.schedule_save(cx);
            return;
        }

        // No 1º caractere: bloco não-parágrafo vira parágrafo (fecha o modo lista/citação/código).
        if self.blocks[idx].kind != BlockKind::Paragraph {
            let editor = self.blocks[idx].editor.clone();
            editor.update(cx, |e, _| e.set_text_style_refinement(style_for(BlockKind::Paragraph)));
            self.blocks[idx].kind = BlockKind::Paragraph;
            cx.notify();
            self.schedule_save(cx);
            return;
        }

        // Parágrafo no 1º caractere: funde com o bloco anterior.
        if idx == 0 {
            return;
        }
        // Bloco anterior é mídia (divisor/imagem, sem texto)? Backspace deleta ele —
        // é assim que se remove um divisor/imagem (são "linhas", sem botão de excluir).
        if matches!(self.blocks[idx - 1].kind, BlockKind::Divider | BlockKind::Image) {
            self.blocks.remove(idx - 1);
            cx.notify();
            self.schedule_save(cx);
            return;
        }
        let cur = self.blocks[idx].editor.update(cx, |e, cx| e.text(cx));
        let handle = self.blocks[idx - 1].editor.focus_handle(cx);
        self.blocks[idx - 1].editor.update(cx, |e, cx| {
            let merged = format!("{}{}", e.text(cx), cur);
            e.set_text(merged, window, cx);
        });
        self.blocks.remove(idx);
        window.focus(&handle, cx);
        cx.notify();
        self.schedule_save(cx);
    }

    /// Move um bloco (arrastado) pra logo antes de `target_id`.
    fn move_block(&mut self, from_id: usize, target_id: usize, cx: &mut Context<Self>) {
        if from_id == target_id {
            return;
        }
        let Some(from) = self.blocks.iter().position(|b| b.id == from_id) else {
            return;
        };
        let block = self.blocks.remove(from);
        let target = self
            .blocks
            .iter()
            .position(|b| b.id == target_id)
            .unwrap_or(self.blocks.len());
        self.blocks.insert(target, block);
        cx.notify();
        self.schedule_save(cx);
    }

    /// `+` no gutter: abre o menu de tipo pra a PRÓPRIA linha (transforma ela, mantendo
    /// o texto). Ex.: parágrafo "oi" → escolher Título → vira Título "oi". Não cria linha.
    fn open_type_menu(&mut self, block_id: usize, cx: &mut Context<Self>) {
        self.slash = Some(SlashState {
            block_id,
            query: String::new(),
            selected: 0,
        });
        cx.notify();
    }

    /// Garante um parágrafo vazio no fim (sempre há onde continuar escrevendo).
    fn ensure_trailing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let needs = self.blocks.last().map_or(true, |b| {
            b.kind != BlockKind::Paragraph || !b.editor.read(cx).text(cx).is_empty()
        });
        if needs {
            let new = self.make_block(BlockKind::Paragraph, "", window, cx);
            let handle = new.editor.focus_handle(cx);
            self.blocks.push(new);
            window.focus(&handle, cx);
            cx.notify();
        } else if let Some(last) = self.blocks.last() {
            let handle = last.editor.focus_handle(cx);
            window.focus(&handle, cx);
        }
    }

    /// Tab: indenta a linha (no máximo um nível a mais que a linha de cima).
    fn indent(&mut self, _: &Indent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(idx) = self.focused_index(window, cx) else {
            return;
        };
        let max = if idx > 0 { self.blocks[idx - 1].indent + 1 } else { 0 };
        let new = (self.blocks[idx].indent + 1).min(max);
        if new != self.blocks[idx].indent {
            self.blocks[idx].indent = new;
            cx.notify();
            self.schedule_save(cx);
        }
    }

    /// Shift+Tab: desaninha um nível.
    fn outdent(&mut self, _: &Outdent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(idx) = self.focused_index(window, cx) else {
            return;
        };
        if self.blocks[idx].indent > 0 {
            self.blocks[idx].indent -= 1;
            cx.notify();
            self.schedule_save(cx);
        }
    }

    fn slash_confirm(&mut self, _: &SlashConfirm, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.slash.clone() else {
            return;
        };
        let kind = filtered_menu(&state.query)
            .get(state.selected)
            .map(|item| item.4);
        match kind {
            Some(kind) => self.apply_menu_item(state.block_id, kind, window, cx),
            None => {
                self.slash = None;
                cx.notify();
            }
        }
    }

    fn slash_dismiss(&mut self, _: &SlashDismiss, _window: &mut Window, cx: &mut Context<Self>) {
        self.slash = None;
        cx.notify();
    }

    /// Aplica um item do menu ao bloco: apaga o `/query`, muda o tipo, e (imagem) abre o picker.
    fn apply_menu_item(
        &mut self,
        block_id: usize,
        kind: BlockKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.slash = None;
        if let Some(block) = self.blocks.iter().find(|b| b.id == block_id) {
            let editor = block.editor.clone();
            editor.update(cx, |e, cx| {
                let text = e.text(cx);
                if let Some(pos) = text.rfind('/') {
                    e.set_text(text[..pos].to_string(), window, cx);
                }
                e.set_text_style_refinement(block_style(kind, cx));
            });
        }
        if let Some(block) = self.blocks.iter_mut().find(|b| b.id == block_id) {
            block.kind = kind;
        }
        // Imagem/divisor não têm editor de texto — garante um parágrafo abaixo pra
        // continuar escrevendo (senão a imagem no fim da nota "prende" o cursor).
        if matches!(kind, BlockKind::Image | BlockKind::Divider)
            && let Some(idx) = self.blocks.iter().position(|b| b.id == block_id)
        {
            let need_para = self
                .blocks
                .get(idx + 1)
                .map_or(true, |b| !matches!(b.kind, BlockKind::Paragraph));
            if need_para {
                let new = self.make_block(BlockKind::Paragraph, "", window, cx);
                let handle = new.editor.focus_handle(cx);
                self.blocks.insert(idx + 1, new);
                window.focus(&handle, cx);
            }
        }
        if kind == BlockKind::Image {
            self.pick_image(block_id, cx);
        }
        cx.notify();
        self.schedule_save(cx);
    }

    fn pick_image(&mut self, block_id: usize, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await
                && let Some(path) = paths.into_iter().next()
            {
                let p = path.to_string_lossy().to_string();
                this.update(cx, |this, cx| {
                    if let Some(b) = this.blocks.iter_mut().find(|b| b.id == block_id) {
                        b.data = Some(p);
                    }
                    cx.notify();
                    this.schedule_save(cx);
                })
                .ok();
            }
        })
        .detach();
    }

    fn pick_cover(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await
                && let Some(path) = paths.into_iter().next()
            {
                let p = path.to_string_lossy().to_string();
                this.update(cx, |this, cx| {
                    this.cover = Some(p);
                    this.cover_menu_open = false;
                    this.save_chrome(cx);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn pick_icon_image(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await
                && let Some(path) = paths.into_iter().next()
            {
                let p = path.to_string_lossy().to_string();
                this.update(cx, |this, cx| {
                    this.icon = Some(p);
                    this.icon_menu_open = false;
                    this.save_chrome(cx);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn toggle_todo(&mut self, block_id: usize, cx: &mut Context<Self>) {
        if let Some(block) = self.blocks.iter_mut().find(|b| b.id == block_id)
            && let BlockKind::Todo(done) = block.kind
        {
            block.kind = BlockKind::Todo(!done);
            cx.notify();
            self.schedule_save(cx);
        }
    }

    /// Escolhe a linguagem de um bloco de código (aplica o highlight).
    fn set_lang(&mut self, block_id: usize, lang: String, cx: &mut Context<Self>) {
        self.lang_menu = None;
        if let Some(block) = self.blocks.iter().find(|b| b.id == block_id) {
            self.apply_code_language(block.editor.clone(), lang.clone(), cx);
        }
        if let Some(block) = self.blocks.iter_mut().find(|b| b.id == block_id) {
            block.lang = Some(lang);
        }
        cx.notify();
        self.schedule_save(cx);
    }

    fn render_lang_menu(&self, block_id: usize, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("lang-menu")
            .max_h(px(260.))
            .overflow_y_scroll()
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.lang_menu = None;
                cx.notify();
            }))
            .w(px(160.))
            .p_1()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().elevated_surface_background)
            .shadow_lg()
            .children(LANGS.iter().map(|lang| {
                let lang = lang.to_string();
                div()
                    .id(SharedString::from(format!("langopt-{lang}")))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|s| s.bg(cx.theme().colors().element_hover))
                    .child(Label::new(lang.clone()).size(LabelSize::Small))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_lang(block_id, lang.clone(), cx)
                    }))
            }))
    }


    fn markdown(&self, cx: &App) -> String {
        self.blocks
            .iter()
            .map(|b| {
                let text = b.editor.read(cx).text(cx);
                let line = match b.kind {
                    BlockKind::H1 => format!("# {text}"),
                    BlockKind::H2 => format!("## {text}"),
                    BlockKind::H3 => format!("### {text}"),
                    BlockKind::Bullet => format!("- {text}"),
                    BlockKind::Numbered => format!("1. {text}"),
                    BlockKind::Todo(false) => format!("- [ ] {text}"),
                    BlockKind::Todo(true) => format!("- [x] {text}"),
                    BlockKind::Quote => format!("> {text}"),
                    BlockKind::Code => {
                        return format!("```{}\n{text}\n```", b.lang.clone().unwrap_or_default());
                    }
                    BlockKind::Divider => return "---".to_string(),
                    BlockKind::Image => {
                        let path = b.data.clone().unwrap_or_default();
                        return match b.width {
                            Some(w) => format!("![]({path} \"{}\")", w as i32),
                            None => format!("![]({path})"),
                        };
                    }
                    BlockKind::Paragraph => text,
                };
                // Aninhamento persiste como 2 espaços por nível.
                format!("{}{line}", "  ".repeat(b.indent))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        let id = self.note_id.clone();
        let content = self.markdown(cx);
        let title = {
            let t = self.title.read(cx).text(cx).trim().to_string();
            if t.is_empty() { "Nova nota".to_string() } else { t }
        };
        let timer = cx.background_executor().timer(SAVE_DEBOUNCE);
        self.save_task = Some(cx.spawn(async move |_this, cx| {
            timer.await;
            let db = cx.update(|cx| NotebookDb::global(cx));
            db.update_note(id, title, content).await.log_err();
            cx.update(|cx| crate::panel::refresh_panel(cx));
        }));
    }

    fn save_chrome(&mut self, cx: &mut Context<Self>) {
        let id = self.note_id.clone();
        let icon = self.icon.clone();
        let cover = self.cover.clone();
        cx.spawn(async move |_this, cx| {
            let db = cx.update(|cx| NotebookDb::global(cx));
            db.update_note_chrome(id, icon, cover).await.log_err();
            cx.update(|cx| crate::panel::refresh_panel(cx));
        })
        .detach();
    }
}

fn filtered_menu(query: &str) -> Vec<&'static (&'static str, &'static str, &'static str, IconName, BlockKind)> {
    MENU.iter()
        .filter(|(_, label, _, _, _)| query.is_empty() || label.to_lowercase().contains(query))
        .collect()
}

/// Tipo do bloco criado ao dar Enter: listas/citação/código continuam; resto vira parágrafo.
fn continuation_kind(kind: BlockKind) -> BlockKind {
    match kind {
        BlockKind::Bullet => BlockKind::Bullet,
        BlockKind::Numbered => BlockKind::Numbered,
        BlockKind::Todo(_) => BlockKind::Todo(false),
        BlockKind::Quote => BlockKind::Quote,
        BlockKind::Code => BlockKind::Code,
        _ => BlockKind::Paragraph,
    }
}

/// Renderiza o ícone da nota: caminho (contém / ou \) vira imagem; senão, emoji.
fn icon_element(icon: &str) -> AnyElement {
    if icon.contains('/') || icon.contains('\\') {
        img(PathBuf::from(icon.to_string()))
            .size(px(64.))
            .rounded_lg()
            .object_fit(gpui::ObjectFit::Cover)
            .into_any_element()
    } else {
        div().text_size(px(56.)).child(icon.to_string()).into_any_element()
    }
}

fn style_for(kind: BlockKind) -> TextStyleRefinement {
    let (size, weight) = match kind {
        BlockKind::H1 => (28.0, FontWeight::BOLD),
        BlockKind::H2 => (22.0, FontWeight::BOLD),
        BlockKind::H3 => (18.0, FontWeight::SEMIBOLD),
        _ => (16.0, FontWeight::NORMAL),
    };
    TextStyleRefinement {
        font_size: Some(px(size).into()),
        font_weight: Some(weight),
        ..Default::default()
    }
}

/// Igual ao `style_for`, mas usa a fonte monoespaçada do editor no bloco de código.
fn block_style(kind: BlockKind, cx: &App) -> TextStyleRefinement {
    let mut r = style_for(kind);
    if kind == BlockKind::Code {
        r.font_family = Some(
            theme_settings::ThemeSettings::get_global(cx)
                .buffer_font
                .family
                .clone(),
        );
    }
    r
}

fn parse_blocks(content: &str) -> Vec<(BlockKind, String, Option<String>, usize)> {
    let mut out = Vec::new();
    let mut lines = content.lines().peekable();
    while let Some(line) = lines.next() {
        if let Some(fence_lang) = line.trim_start().strip_prefix("```") {
            let lang = fence_lang.trim().to_string();
            let mut code = Vec::new();
            while let Some(l) = lines.peek() {
                if l.trim_start().starts_with("```") {
                    lines.next();
                    break;
                }
                code.push(lines.next().unwrap().to_string());
            }
            // data carrega a linguagem do bloco de código (ver new()).
            out.push((
                BlockKind::Code,
                code.join("\n"),
                (!lang.is_empty()).then_some(lang),
                0,
            ));
            continue;
        }
        // Indentação = 2 espaços por nível; classifica a linha já sem os espaços.
        let indent = line.chars().take_while(|c| *c == ' ').count() / 2;
        let (kind, text, data) = classify(line.trim_start_matches(' '));
        out.push((kind, text, data, indent));
    }
    if out.is_empty() {
        out.push((BlockKind::Paragraph, String::new(), None, 0));
    }
    out
}

fn classify(line: &str) -> (BlockKind, String, Option<String>) {
    if line.trim() == "---" {
        return (BlockKind::Divider, String::new(), None);
    }
    if let Some(inner) = line.strip_prefix("![](").and_then(|r| r.strip_suffix(')')) {
        // `path` ou `path "300"` (largura no título). Codifica width com \0 pra não
        // alterar a assinatura de classify — o new() separa de volta.
        let data = match inner.rsplit_once(" \"") {
            Some((p, w)) => format!("{p}\u{0}{}", w.trim_end_matches('"')),
            None => inner.to_string(),
        };
        return (BlockKind::Image, String::new(), Some(data));
    }
    if let Some(r) = line.strip_prefix("### ") {
        return (BlockKind::H3, r.to_string(), None);
    }
    if let Some(r) = line.strip_prefix("## ") {
        return (BlockKind::H2, r.to_string(), None);
    }
    if let Some(r) = line.strip_prefix("# ") {
        return (BlockKind::H1, r.to_string(), None);
    }
    if let Some(r) = line.strip_prefix("- [ ] ") {
        return (BlockKind::Todo(false), r.to_string(), None);
    }
    if let Some(r) = line
        .strip_prefix("- [x] ")
        .or_else(|| line.strip_prefix("- [X] "))
    {
        return (BlockKind::Todo(true), r.to_string(), None);
    }
    if let Some(r) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
        return (BlockKind::Bullet, r.to_string(), None);
    }
    if let Some(r) = line.strip_prefix("> ") {
        return (BlockKind::Quote, r.to_string(), None);
    }
    if let Some(pos) = line.find(". ")
        && pos > 0
        && line[..pos].chars().all(|c| c.is_ascii_digit())
    {
        return (BlockKind::Numbered, line[pos + 2..].to_string(), None);
    }
    (BlockKind::Paragraph, line.to_string(), None)
}

impl Focusable for NoteDoc {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for NoteDoc {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Numeração hierárquica das listas ("1", "1.1", "1.2", "2"…) por nível de indentação.
        let mut labels: Vec<String> = Vec::with_capacity(self.blocks.len());
        let mut counters: Vec<usize> = Vec::new();
        for block in &self.blocks {
            if block.kind == BlockKind::Numbered {
                let d = block.indent;
                if counters.len() <= d {
                    counters.resize(d + 1, 0);
                }
                counters.truncate(d + 1);
                counters[d] += 1;
                labels.push(
                    counters
                        .iter()
                        .map(|c| c.to_string())
                        .collect::<Vec<_>>()
                        .join("."),
                );
            } else {
                counters.clear();
                labels.push(String::new());
            }
        }

        let mut rows: Vec<AnyElement> = Vec::new();
        for (i, block) in self.blocks.iter().enumerate() {
            let block_el = self
                .render_block(block, &labels[i], cx)
                .into_any_element();
            if self.slash.as_ref().is_some_and(|s| s.block_id == block.id) {
                // Menu como OVERLAY absoluto abaixo do bloco: não entra no fluxo, então
                // não empurra o conteúdo nem faz a nota rolar (o que sumia com a capa).
                rows.push(
                    div()
                        .relative()
                        .child(block_el)
                        .child(gpui::deferred(
                            div().absolute().top_full().left_0().child(
                                // anchored + snap_to_window: se não couber embaixo, encosta na
                                // borda da janela em vez de sair da tela.
                                gpui::anchored()
                                    .snap_to_window_with_margin(px(8.))
                                    .child(self.render_slash_menu(block.id, cx)),
                            ),
                        ))
                        .into_any_element(),
                );
            } else {
                rows.push(block_el);
            }
        }

        // ponytail: capa + cabeçalho FIXOS (fora do scroll). Antes tudo ficava dentro do
        // overflow_y_scroll e o auto-scroll dos editores empurrava a capa pra fora / sumia.
        v_flex()
            .id("note-doc")
            .track_focus(&self.focus_handle)
            .size_full()
            .on_action(cx.listener(Self::split_block))
            .on_action(cx.listener(Self::merge_backward))
            .on_action(cx.listener(Self::slash_confirm))
            .on_action(cx.listener(Self::slash_dismiss))
            .on_action(cx.listener(Self::slash_next))
            .on_action(cx.listener(Self::slash_prev))
            .on_action(cx.listener(Self::indent))
            .on_action(cx.listener(Self::outdent))
            // Resize de imagem: arrastar a alça atualiza a largura pelo delta do mouse.
            .on_drag_move(cx.listener(
                |this, e: &gpui::DragMoveEvent<DraggedImageHandle>, _window, cx| {
                    let id = e.drag(cx).0;
                    let x = f32::from(e.event.position.x);
                    if let Some((rid, last)) = this.img_resize
                        && rid == id
                        && let Some(b) = this.blocks.iter_mut().find(|b| b.id == id)
                    {
                        let base = b.width.unwrap_or(400.0);
                        b.width = Some((base + (x - last)).clamp(80.0, 1400.0));
                    }
                    this.img_resize = Some((id, x));
                    cx.notify();
                    this.schedule_save(cx);
                },
            ))
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, _| this.img_resize = None),
            )
            // Capa e cabeçalho ficam fixos no topo.
            .child(self.render_cover(cx))
            .child(div().flex_none().w_full().px_12().pt_2().child(self.render_header(cx)))
            // Só os blocos rolam.
            .child(
                v_flex()
                    .id("note-scroll")
                    .flex_1()
                    .min_h(px(0.))
                    .w_full()
                    .overflow_y_scroll()
                    .px_12()
                    .pb_16()
                    .child(v_flex().mt_2().gap_1().ml(px(-48.)).children(rows))
                    .child(
                        div()
                            .id("note-tail")
                            .w_full()
                            .min_h(px(160.))
                            .cursor_text()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.ensure_trailing(window, cx)
                            })),
                    ),
            )
    }
}

impl NoteDoc {
    fn render_slash_menu(&self, block_id: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.slash.as_ref().map(|s| s.query.clone()).unwrap_or_default();
        let selected_idx = self.slash.as_ref().map_or(0, |s| s.selected);
        let items = filtered_menu(&query);

        let mut children: Vec<AnyElement> = Vec::new();
        let mut section = "";
        for (i, (sec, label, desc, icon, kind)) in items.iter().enumerate() {
            if *sec != section {
                section = sec;
                children.push(
                    div()
                        .px_2()
                        .pt_2()
                        .pb_1()
                        .child(
                            Label::new(sec.to_uppercase())
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        )
                        .into_any_element(),
                );
            }
            let kind = *kind;
            let selected = i == selected_idx;
            children.push(
                h_flex()
                    .id(SharedString::from(format!("slash-{i}")))
                    .w_full()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .items_center()
                    .cursor_pointer()
                    .when(selected, |el| el.bg(cx.theme().colors().element_selected))
                    .hover(|s| s.bg(cx.theme().colors().element_hover))
                    .child(
                        div()
                            .flex_none()
                            .size(px(28.))
                            .rounded_md()
                            .border_1()
                            .border_color(cx.theme().colors().border)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Icon::new(*icon).size(IconSize::Small).color(Color::Muted)),
                    )
                    .child(
                        v_flex()
                            .child(Label::new(*label).size(LabelSize::Small))
                            .child(
                                Label::new(*desc)
                                    .size(LabelSize::XSmall)
                                    .color(Color::Muted),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.apply_menu_item(block_id, kind, window, cx)
                    }))
                    .into_any_element(),
            );
        }
        if children.is_empty() {
            children.push(
                div()
                    .p_2()
                    .child(Label::new("Nenhum bloco").size(LabelSize::Small).color(Color::Muted))
                    .into_any_element(),
            );
        }

        v_flex()
            .id("slash-menu")
            .track_scroll(&self.slash_scroll)
            .w(px(320.))
            .max_h(px(300.))
            .overflow_y_scroll()
            // Scroll fica no menu, não vaza pra página.
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            // Clicar fora do menu fecha ele.
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.slash = None;
                cx.notify();
            }))
            .px_1()
            .py_2()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().elevated_surface_background)
            .shadow_lg()
            .children(children)
    }

    fn render_cover(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div().w_full().when_some(self.cover.clone(), |el, cover| {
            // Cover pode ser uma cor predefinida ("1".."6") ou um caminho de imagem.
            let color = COVERS.iter().find(|(c, _)| *c == cover).map(|(_, rgb)| *rgb);
            el.h(px(220.))
                .relative()
                .overflow_hidden()
                .map(|el| match color {
                    Some(rgb) => el.bg(gpui::rgb(rgb)),
                    None => el.child(
                        img(PathBuf::from(cover.clone()))
                            .absolute()
                            .inset_0()
                            .size_full()
                            .object_fit(gpui::ObjectFit::Cover),
                    ),
                })
                .child(
                    h_flex()
                        .absolute()
                        .right_3()
                        .bottom_3()
                        .gap_1()
                        .child(Button::new("cover-change", "Trocar").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.cover_menu_open = !this.cover_menu_open;
                                cx.notify();
                            },
                        )))
                        .child(Button::new("cover-remove", "Remover").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.cover = None;
                                this.cover_menu_open = false;
                                this.save_chrome(cx);
                                cx.notify();
                            },
                        ))),
                )
                .when(self.cover_menu_open, |el| {
                    el.child(
                        h_flex()
                            .absolute()
                            .right_3()
                            .bottom_12()
                            .gap_1()
                            .items_center()
                            .p_1()
                            .rounded_md()
                            .bg(cx.theme().colors().elevated_surface_background)
                            .children(COVERS.iter().map(|(id, rgb)| {
                                let id = id.to_string();
                                div()
                                    .id(SharedString::from(format!("cover-{id}")))
                                    .size(px(24.))
                                    .rounded_sm()
                                    .bg(gpui::rgb(*rgb))
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.cover = Some(id.clone());
                                        this.cover_menu_open = false;
                                        this.save_chrome(cx);
                                        cx.notify();
                                    }))
                            }))
                            .child(Button::new("cover-image", "Imagem").on_click(cx.listener(
                                |this, _, _, cx| this.pick_cover(cx),
                            ))),
                    )
                })
        })
    }

    /// Header estilo Notion: ícone (emoji OU imagem) sobre a borda inferior da capa,
    /// com o título AO LADO. Abaixo, os botões de adicionar ícone/capa.
    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let has_cover = self.cover.is_some();
        v_flex()
            .relative()
            .gap_1()
            .child(
                h_flex()
                    .items_end()
                    .gap_3()
                    // Ícone straddleia a borda inferior da capa (metade dentro, metade fora).
                    .when(has_cover, |el| el.mt(px(-40.)))
                    .when(!has_cover, |el| el.pt_8())
                    .when_some(self.icon.clone(), |el, icon| {
                        el.child(
                            div()
                                .id("note-icon")
                                .flex_none()
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.icon_menu_open = !this.icon_menu_open;
                                    cx.notify();
                                }))
                                .child(icon_element(&icon)),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .pb_1()
                            .text_size(px(30.))
                            .child(self.title.clone()),
                    ),
            )
            // Menu de emoji como OVERLAY absoluto — não empurra o layout (a capa some se empurrar).
            .when(self.icon_menu_open, |el| {
                el.child(gpui::deferred(
                    div()
                        .absolute()
                        .top(px(72.))
                        .left_0()
                        .child(self.render_emoji_grid(cx)),
                ))
            })
            .child(
                h_flex()
                    .gap_2()
                    .when(self.icon.is_none(), |el| {
                        el.child(Button::new("add-icon", "Adicionar ícone").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.icon_menu_open = !this.icon_menu_open;
                                cx.notify();
                            }),
                        ))
                    })
                    .when(self.cover.is_none(), |el| {
                        el.child(Button::new("add-cover", "Adicionar capa").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.cover = Some("1".to_string());
                                this.save_chrome(cx);
                                cx.notify();
                            },
                        )))
                    }),
            )
    }

    fn render_emoji_grid(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("emoji-grid")
            .w(px(320.))
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().elevated_surface_background)
            .shadow_lg()
            // Barra superior: upload de imagem / remover.
            .child(
                h_flex()
                    .justify_between()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        Button::new("icon-upload", "Upload imagem")
                            .on_click(cx.listener(|this, _, _, cx| this.pick_icon_image(cx))),
                    )
                    .child(Button::new("icon-remove", "Remover").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.icon = None;
                            this.icon_menu_open = false;
                            this.save_chrome(cx);
                            cx.notify();
                        },
                    ))),
            )
            // Grade rolável por categoria.
            .child(
                v_flex()
                    .id("emoji-scroll")
                    .max_h(px(300.))
                    .overflow_y_scroll()
                    .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                    .p_1()
                    .children(EMOJI_CATS.iter().map(|(cat, emojis)| {
                        v_flex()
                            .child(
                                div().px_1().pt_1().pb_0p5().child(
                                    Label::new(cat.to_uppercase())
                                        .size(LabelSize::XSmall)
                                        .color(Color::Muted),
                                ),
                            )
                            .child(h_flex().flex_wrap().gap_0p5().children(emojis.iter().map(
                                |emoji| {
                                    let emoji = emoji.to_string();
                                    div()
                                        .id(SharedString::from(format!("emoji-{emoji}")))
                                        .p_1()
                                        .text_size(px(20.))
                                        .rounded_sm()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(cx.theme().colors().element_hover))
                                        .child(emoji.clone())
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.icon = Some(emoji.clone());
                                            this.icon_menu_open = false;
                                            this.save_chrome(cx);
                                            cx.notify();
                                        }))
                                },
                            )))
                    })),
            )
    }

    /// Gutter estilo Notion. EM FLUXO (reserva largura fixa) — assim a área dele faz parte
    /// do hitbox do bloco e não some quando o mouse vai clicar. Conteúdo só aparece no hover.
    fn render_gutter(&self, id: usize, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .flex_none()
            .w(px(48.))
            .gap_0p5()
            .justify_end()
            .pr_1()
            .pt(px(1.))
            .invisible()
            .group_hover("blk", |s| s.visible())
            .child(
                IconButton::new(SharedString::from(format!("add-{id}")), IconName::Plus)
                    .icon_size(IconSize::Small)
                    .tooltip(ui::Tooltip::text("Transformar esta linha"))
                    .on_click(cx.listener(move |this, _, _, cx| this.open_type_menu(id, cx))),
            )
            .child(
                div()
                    .id(SharedString::from(format!("drag-{id}")))
                    .cursor_grab()
                    .p_1()
                    .rounded_sm()
                    .hover(|s| s.bg(cx.theme().colors().element_hover))
                    .on_drag(DraggedBlock(id), |d, _, _, cx| cx.new(|_| d.clone()))
                    .child(Icon::new(IconName::Menu).size(IconSize::Small).color(Color::Muted)),
            )
    }

    fn render_block(
        &self,
        block: &Block,
        number_label: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = block.id;
        let indent_px = block.indent as f32 * 24.0;
        let is_slash = self.slash.as_ref().is_some_and(|s| s.block_id == id);

        // Conteúdo do bloco (varia por tipo).
        let content: AnyElement = match block.kind {
            BlockKind::Divider => div()
                .w_full()
                .py_2()
                .child(div().w_full().h(px(1.)).bg(cx.theme().colors().border))
                .into_any_element(),
            BlockKind::Image => {
                if let Some(path) = block.data.clone() {
                    // max_w (não w): o object_fit Contain do gpui deixa a imagem menor que a
                    // caixa quando usa w fixo, e a alça descolava. Com max_w a caixa envolve a
                    // imagem, então a alça fica GRUDADA na borda dela.
                    let w = block.width.unwrap_or(400.0);
                    div()
                        .relative()
                        .flex_none()
                        .group("imgblk")
                        .child(img(PathBuf::from(path)).max_w(px(w)).rounded_md())
                        .child(
                            div()
                                .id(SharedString::from(format!("imgresize-{id}")))
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .right(px(-6.))
                                .w(px(12.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_col_resize()
                                .invisible()
                                .group_hover("imgblk", |s| s.visible())
                                .child(
                                    div()
                                        .w(px(4.))
                                        .h(px(40.))
                                        .rounded_full()
                                        .bg(cx.theme().colors().icon_accent),
                                )
                                .on_drag(DraggedImageHandle(id), |d, _, _, cx| {
                                    cx.stop_propagation();
                                    cx.new(|_| d.clone())
                                }),
                        )
                        .into_any_element()
                } else {
                    div()
                        .id(SharedString::from(format!("img-empty-{id}")))
                        .w_full()
                        .h(px(120.))
                        .rounded_md()
                        .border_1()
                        .border_dashed()
                        .border_color(cx.theme().colors().border)
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .child(Label::new("Clique para escolher uma imagem").color(Color::Muted))
                        .on_click(cx.listener(move |this, _, _, cx| this.pick_image(id, cx)))
                        .into_any_element()
                }
            }
            _ => {
                let ctx = if is_slash {
                    "SlashMenu"
                } else if block.kind == BlockKind::Code {
                    "NoteCode"
                } else {
                    "NoteBlock"
                };
                let body = div().flex_1().min_w(px(0.)).child(block.editor.clone());
                let row = h_flex().key_context(ctx).w_full().gap_2().items_start();
                match block.kind {
                    BlockKind::Bullet => row
                        .child(div().pt(px(4.)).child(Label::new("•").color(Color::Muted)))
                        .child(body)
                        .into_any_element(),
                    BlockKind::Numbered => row
                        .child(
                            div()
                                .pt(px(2.))
                                .child(Label::new(format!("{number_label}.")).color(Color::Muted)),
                        )
                        .child(body)
                        .into_any_element(),
                    BlockKind::Todo(done) => row
                        .child(
                            div()
                                .id(SharedString::from(format!("todo-{id}")))
                                .mt(px(3.))
                                .size(px(16.))
                                .rounded_sm()
                                .border_1()
                                .border_color(cx.theme().colors().border)
                                .cursor_pointer()
                                .when(done, |d| {
                                    d.bg(cx.theme().colors().icon_accent).child(
                                        Icon::new(IconName::Check)
                                            .size(IconSize::XSmall)
                                            .color(Color::Default),
                                    )
                                })
                                .on_click(cx.listener(move |this, _, _, cx| this.toggle_todo(id, cx))),
                        )
                        .child(body)
                        .into_any_element(),
                    BlockKind::Quote => row
                        .child(
                            div()
                                .w_full()
                                .border_l_2()
                                .border_color(cx.theme().colors().border)
                                .pl_3()
                                .child(body),
                        )
                        .into_any_element(),
                    BlockKind::Code => {
                        let lang_label = block.lang.clone().unwrap_or_else(|| "texto".into());
                        let menu_open = self.lang_menu == Some(id);
                        row.child(
                            div()
                                .relative()
                                .w_full()
                                .rounded_md()
                                .bg(cx.theme().colors().editor_background)
                                .child(
                                    // Cabeçalho do bloco: pill com a linguagem (abre o menu).
                                    h_flex().justify_end().px_2().pt_1().child(
                                        div()
                                            .id(SharedString::from(format!("lang-{id}")))
                                            .px_1p5()
                                            .rounded_sm()
                                            .cursor_pointer()
                                            .hover(|s| s.bg(cx.theme().colors().element_hover))
                                            .child(
                                                Label::new(lang_label)
                                                    .size(LabelSize::XSmall)
                                                    .color(Color::Muted),
                                            )
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.lang_menu =
                                                    (this.lang_menu != Some(id)).then_some(id);
                                                cx.notify();
                                            })),
                                    ),
                                )
                                .child(div().px_2().pb_2().child(body))
                                .when(menu_open, |el| {
                                    el.child(gpui::deferred(
                                        div()
                                            .absolute()
                                            .top_8()
                                            .right_2()
                                            .child(self.render_lang_menu(id, cx)),
                                    ))
                                }),
                        )
                        .into_any_element()
                    }
                    _ => row.child(body).into_any_element(),
                }
            }
        };

        // Envelope comum: gutter (hover, em fluxo) + alvo de drop pra reordenar.
        h_flex()
            .group("blk")
            .w_full()
            .items_start()
            .drag_over::<DraggedBlock>(|el, _, _, cx| {
                el.border_t_2().border_color(cx.theme().colors().border_focused)
            })
            .on_drop(cx.listener(move |this, dragged: &DraggedBlock, _, cx| {
                this.move_block(dragged.0, id, cx)
            }))
            .child(self.render_gutter(id, cx))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .pl(px(indent_px))
                    .child(content),
            )
    }
}
