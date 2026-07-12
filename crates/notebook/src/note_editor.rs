//! Editor de blocos estilo Notion (WYSIWYG) pras notas. Cada bloco é um `Editor`
//! auto-height estilizado pelo tipo (título grande, lista, tarefa com checkbox, etc.) —
//! o markdown NÃO aparece cru. Enter cria bloco; `/` abre um menu custom com seções
//! (Títulos / Blocos básicos / Mídia). Persiste como markdown (round-trip) pra a IA/tool.
//!
//! ponytail: v1 — Enter não divide o texto no cursor, sem drag pra reordenar, sem toolbar
//! de negrito inline, sem Tabela (grid de células editáveis é a próxima feature grande).

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::Duration;

use editor::{
    Anchor, Editor, EditorEvent, FoldPlaceholder, MultiBufferOffset,
    display_map::{Crease, CreaseMetadata, FoldId},
};
use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    ListAlignment, ListState, PathPromptOptions, ScrollHandle, Subscription, Task,
    TextStyleRefinement, WeakEntity, Window, actions, img, prelude::*,
};
use std::sync::Arc;

use language::LanguageRegistry;
use language::language_settings::SoftWrap;
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use rope::Point;
use settings::Settings as _;
use ui::prelude::*;
use ui::{ScrollAxes, Scrollbars, WithScrollbar};
use util::ResultExt as _;

use crate::store::{Note, NotebookDb};

#[cfg(test)]
static RENDERED_BLOCKS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static CREATED_MARKDOWNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static SERIALIZED_NOTES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Cache de imagens de blocos ricos (```plot), por hash(lang+fonte). Rasterizar SVG é
/// caro → só re-renderiza quando a fonte muda. Thread-local (o render é single-thread).
thread_local! {
    static RICH_CACHE: RefCell<HashMap<u64, Option<Arc<gpui::RenderImage>>>> =
        RefCell::new(HashMap::new());
}

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
const BLOCK_HEIGHT_HINT: f32 = 28.;
const FOOTER_HEIGHT_HINT: f32 = 200.;

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
    // Langs "ricas": renderizam pra imagem em vez de só mostrar o código (ver rich_block).
    "plot",
    "math",
];

/// Itens do menu `/`: (seção, rótulo, descrição, ícone, tipo).
/// Tabela markdown inicial (2 colunas, 1 linha vazia + separador GFM).
const TABLE_STARTER: &str = "| Coluna 1 | Coluna 2 |\n| --- | --- |\n|  |  |";

const MENU: &[(&str, &str, &str, IconName, BlockKind)] = &[
    (
        "Títulos",
        "Título 1",
        "Título de seção grande",
        IconName::Hash,
        BlockKind::H1,
    ),
    (
        "Títulos",
        "Título 2",
        "Título de seção média",
        IconName::Hash,
        BlockKind::H2,
    ),
    (
        "Títulos",
        "Título 3",
        "Título de subseção",
        IconName::Hash,
        BlockKind::H3,
    ),
    (
        "Blocos básicos",
        "Texto",
        "Texto simples",
        IconName::Menu,
        BlockKind::Paragraph,
    ),
    (
        "Blocos básicos",
        "Lista com marcadores",
        "Lista com marcadores",
        IconName::ListTree,
        BlockKind::Bullet,
    ),
    (
        "Blocos básicos",
        "Lista numerada",
        "Lista ordenada por números",
        IconName::ListTree,
        BlockKind::Numbered,
    ),
    (
        "Blocos básicos",
        "Tarefa",
        "Lista com caixas de seleção",
        IconName::ListTodo,
        BlockKind::Todo(false),
    ),
    (
        "Blocos básicos",
        "Citação",
        "Destaque uma citação",
        IconName::Quote,
        BlockKind::Quote,
    ),
    (
        "Blocos básicos",
        "Código",
        "Bloco de código",
        IconName::Code,
        BlockKind::Code,
    ),
    (
        "Blocos básicos",
        "Divisor",
        "Separa visualmente os blocos",
        IconName::Dash,
        BlockKind::Divider,
    ),
    (
        "Blocos básicos",
        "Tabela",
        "Tabela de dados (linhas e colunas)",
        IconName::DatabaseZap,
        BlockKind::Table,
    ),
    (
        "Mídia",
        "Imagem",
        "Escolha uma imagem do computador",
        IconName::Image,
        BlockKind::Image,
    ),
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
    /// Tabela markdown (`| a | b |`): edita cru multi-linha, renderiza como tabela.
    Table,
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
    /// Criado apenas quando o bloco entra no viewport e precisa do preview WYSIWYG.
    md: RefCell<Option<Entity<Markdown>>>,
    /// Só para `Table`: grade de editores por célula (linha × coluna). A tabela é sempre
    /// uma grade editável — nunca alterna pra markdown cru. `block.editor` fica sem uso.
    cells: Vec<Vec<Entity<Editor>>>,
    _sub: Subscription,
    /// Subscriptions dos editores de célula (mantêm o save em dia).
    _cell_subs: Vec<Subscription>,
}

#[derive(Clone)]
struct SlashState {
    block_id: usize,
    query: String,
    selected: usize,
}

/// Alvo do documento: nota comum, ou reunião (que além do corpo em blocos carrega
/// transcrição/resumo/uso renderizados como seções após os blocos).
pub enum DocKind {
    Note,
    Meeting {
        date: String,
        transcript: String,
        summary: String,
        uso: String,
    },
    /// Skill (~/.claude/skills/<note_id>/SKILL.md): corpo = instruções em markdown,
    /// `front` = linhas do frontmatter preservadas (name/description/etc.).
    Skill {
        front: Vec<String>,
    },
}

/// Aba ativa na visão de reunião.
#[derive(Clone, Copy, PartialEq)]
enum MeetingTab {
    Notas,
    Transcricao,
    Resumo,
    Uso,
}

/// Eventos que o NoteDoc emite pro painel (que controla qual nota está aberta).
pub enum NoteDocEvent {
    /// Abrir a nota com este título (wikilink seguido / clique num backlink).
    OpenNote(String),
}

pub struct NoteDoc {
    note_id: String,
    kind: DocKind,
    /// Aba ativa (só relevante p/ reunião).
    meeting_tab: MeetingTab,
    languages: Arc<LanguageRegistry>,
    icon: Option<String>,
    cover: Option<String>,
    icon_menu_open: bool,
    cover_menu_open: bool,
    title: Entity<Editor>,
    blocks: Vec<Block>,
    number_labels: Vec<String>,
    next_id: usize,
    slash: Option<SlashState>,
    /// Autocomplete de `[[wikilink]]` aberto (reusa SlashState: block_id/query/selected).
    link: Option<SlashState>,
    /// Conversão markdown pendente (atalho `# `, `- `, ```lang…) detectada num edit —
    /// aplicada no próximo render, que tem `window` (o subscription de edit não tem).
    pending_convert: Option<(usize, BlockKind, String, Option<String>)>,
    /// Bloco de código com o menu de linguagem aberto.
    lang_menu: Option<usize>,
    /// Resize de imagem em andamento: (block_id, última posição x do mouse).
    img_resize: Option<(usize, f32)>,
    slash_scroll: ScrollHandle,
    /// Lista virtualizada do corpo; mantém editores fora da tela sem renderizá-los.
    body_list: ListState,
    /// Scroll do corpo da nota (pra desenhar a scrollbar lateral).
    /// Usado pelas abas não editáveis de reunião.
    body_scroll: ScrollHandle,
    focus_handle: FocusHandle,
    save_task: Option<Task<()>>,
    /// Notas que apontam pra esta (via `[[título]]`), computadas ao abrir. (id, título)
    backlinks: Vec<(String, String)>,
    /// Título → ícone custom (emoji/imagem), pra o badge de wikilink mostrar o ícone certo.
    link_icons: HashMap<String, String>,
    _title_sub: Subscription,
}

impl EventEmitter<NoteDocEvent> for NoteDoc {}

impl NoteDoc {
    pub fn new(
        note: Note,
        languages: Arc<LanguageRegistry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::from_parts(
            note.id,
            note.title,
            note.content,
            note.icon,
            note.cover,
            DocKind::Note,
            languages,
            window,
            cx,
        )
    }

    /// Reunião com o mesmo block editor das notas + seções Transcrição/Resumo/Uso.
    pub fn new_meeting(
        meeting: crate::store::Meeting,
        languages: Arc<LanguageRegistry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::from_parts(
            meeting.id,
            meeting.title,
            meeting.content,
            meeting.icon,
            meeting.cover,
            DocKind::Meeting {
                date: meeting.date,
                transcript: meeting.transcript,
                summary: meeting.summary,
                uso: meeting.uso,
            },
            languages,
            window,
            cx,
        )
    }

    /// Skill: o corpo (instruções markdown) usa o mesmo block editor das notas.
    /// Salva reescrevendo o SKILL.md e preservando o frontmatter (`front`).
    pub fn new_skill(
        name: String,
        front: Vec<String>,
        body: String,
        languages: Arc<LanguageRegistry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::from_parts(
            name.clone(),
            name,
            body,
            None,
            None,
            DocKind::Skill { front },
            languages,
            window,
            cx,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn from_parts(
        id: String,
        title_text: String,
        content: String,
        icon: Option<String>,
        cover: Option<String>,
        kind: DocKind,
        languages: Arc<LanguageRegistry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Backlinks: notas cujo corpo contém `[[<este título>]]`. Só p/ notas comuns.
        let backlinks = if matches!(kind, DocKind::Note) {
            compute_backlinks(&id, &title_text, cx)
        } else {
            Vec::new()
        };
        let link_icons = compute_link_icons(cx);
        let title = cx.new(|cx| {
            let mut e = Editor::single_line(window, cx);
            e.set_text(title_text, window, cx);
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
            note_id: id,
            kind,
            meeting_tab: MeetingTab::Notas,
            languages,
            icon: icon.filter(|s| !s.is_empty()),
            cover: cover.filter(|s| !s.is_empty()),
            icon_menu_open: false,
            cover_menu_open: false,
            title,
            blocks: Vec::new(),
            number_labels: Vec::new(),
            next_id: 0,
            slash: None,
            link: None,
            pending_convert: None,
            lang_menu: None,
            img_resize: None,
            slash_scroll: ScrollHandle::new(),
            body_list: ListState::new(0, ListAlignment::Top, px(512.)),
            body_scroll: ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            save_task: None,
            backlinks,
            link_icons,
            _title_sub: title_sub,
        };
        for (kind, text, data, indent) in parse_blocks(&content) {
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
        this.refresh_number_labels();
        this.body_list.splice_focusable_with_size_hints(
            0..0,
            this.blocks
                .iter()
                .map(|block| {
                    (
                        Some(block.editor.focus_handle(cx)),
                        Some(gpui::size(px(0.), px(BLOCK_HEIGHT_HINT))),
                    )
                })
                .chain(std::iter::once((
                    None,
                    Some(gpui::size(px(0.), px(FOOTER_HEIGHT_HINT))),
                ))),
        );
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
                e.set_text(text.clone(), window, cx);
            }
            e.set_text_style_refinement(refinement);
            e
        });
        // ponytail: NÃO aplicamos mais a linguagem Markdown (tree-sitter) por bloco. Cada
        // bloco tinha um parser tree-sitter próprio → uma skill/nota grande com centenas de
        // linhas = centenas de parsers re-realçando a cada scroll = TRAVA tudo. O realce cru
        // dos `**`/`` ` `` só aparecia no bloco focado; o WYSIWYG (MarkdownElement fora de foco)
        // não usa isso. Ganho enorme de performance abrindo notas grandes.
        let sub = cx.subscribe(&editor, move |this, editor, ev, cx| {
            if matches!(ev, EditorEvent::Edited { .. }) {
                this.schedule_save(cx);
                this.detect_slash(id, editor.clone(), cx);
                this.detect_wikilink(id, editor.clone(), cx);
                // Mantém o render markdown do bloco em sincronia com o texto.
                this.refresh_md(id, cx);
                this.detect_markdown(id, editor, cx);
                this.remeasure_block(id);
            }
        });
        // `[[wikilinks]]` viram badges inline (crease) já ao criar/carregar o bloco.
        if kind_renders_md(kind) {
            self.fold_wikilinks_in_editor(&editor, window, cx);
        }
        let (cells, cell_subs) = if kind == BlockKind::Table {
            self.build_table_cells(id, &text, window, cx)
        } else {
            (Vec::new(), Vec::new())
        };
        Block {
            id,
            kind,
            editor,
            data: None,
            indent: 0,
            width: None,
            lang: None,
            md: RefCell::new(None),
            cells,
            _sub: sub,
            _cell_subs: cell_subs,
        }
    }

    /// Cria um editor de célula de tabela (1 linha, cresce até 3) + subscription que salva.
    fn make_cell_editor(
        &self,
        block_id: usize,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<Editor>, Subscription) {
        let text = text.to_string();
        let editor = cx.new(|cx| {
            let mut e = Editor::auto_height(1, 3, window, cx);
            e.set_soft_wrap_mode(SoftWrap::EditorWidth, cx);
            e.set_show_gutter(false, cx);
            e.set_show_wrap_guides(false, cx);
            if !text.is_empty() {
                e.set_text(text.clone(), window, cx);
            }
            e
        });
        let sub = cx.subscribe(&editor, move |this, _ed, ev, cx| {
            if matches!(ev, EditorEvent::Edited { .. }) {
                this.schedule_save(cx);
                this.remeasure_block(block_id);
            }
        });
        (editor, sub)
    }

    /// Faz o parse do markdown de tabela (`| a | b |`) numa grade de editores de célula.
    fn build_table_cells(
        &self,
        block_id: usize,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Vec<Vec<Entity<Editor>>>, Vec<Subscription>) {
        let mut cells = Vec::new();
        let mut subs = Vec::new();
        for row in parse_table(text) {
            let mut erow = Vec::new();
            for c in row {
                let (ed, sub) = self.make_cell_editor(block_id, &c, window, cx);
                erow.push(ed);
                subs.push(sub);
            }
            cells.push(erow);
        }
        (cells, subs)
    }

    /// Reconstrói a grade de células de um bloco a partir do texto (usado ao converter um
    /// bloco em tabela). Substitui `cells`/`_cell_subs` do bloco.
    fn rebuild_table_cells(
        &mut self,
        block_id: usize,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (cells, subs) = self.build_table_cells(block_id, text, window, cx);
        if let Some(b) = self.blocks.iter_mut().find(|b| b.id == block_id) {
            b.cells = cells;
            b._cell_subs = subs;
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

    fn refresh_number_labels(&mut self) {
        self.number_labels.clear();
        self.number_labels.reserve(self.blocks.len());
        let mut counters = Vec::<usize>::new();
        for block in &self.blocks {
            if block.kind == BlockKind::Numbered {
                let depth = block.indent;
                if counters.len() <= depth {
                    counters.resize(depth + 1, 0);
                }
                counters.truncate(depth + 1);
                counters[depth] += 1;
                self.number_labels.push(
                    counters
                        .iter()
                        .map(|count| count.to_string())
                        .collect::<Vec<_>>()
                        .join("."),
                );
            } else {
                counters.clear();
                self.number_labels.push(String::new());
            }
        }
    }

    fn remeasure_block(&self, block_id: usize) {
        if let Some(index) = self.blocks.iter().position(|block| block.id == block_id) {
            self.body_list.remeasure_items(index..index + 1);
        }
    }

    fn block_inserted(&mut self, index: usize, focus_handle: FocusHandle) {
        self.body_list.splice_focusable_with_size_hints(
            index..index,
            [(
                Some(focus_handle),
                Some(gpui::size(px(0.), px(BLOCK_HEIGHT_HINT))),
            )],
        );
        self.body_list.scroll_to_reveal_item(index);
        self.refresh_number_labels();
    }

    fn block_removed(&mut self, index: usize) {
        self.body_list.splice(index..index + 1, 0);
        self.refresh_number_labels();
    }

    fn blocks_reordered(&mut self, range: std::ops::Range<usize>, cx: &App) {
        let handles = self.blocks[range.clone()]
            .iter()
            .map(|block| {
                (
                    Some(block.editor.focus_handle(cx)),
                    Some(gpui::size(px(0.), px(BLOCK_HEIGHT_HINT))),
                )
            })
            .collect::<Vec<_>>();
        self.body_list
            .splice_focusable_with_size_hints(range, handles);
        self.refresh_number_labels();
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

    /// Auto-detecção markdown: quando um parágrafo passa a começar com um marcador
    /// (`# `, `## `, `- `, `> `, `N. `, `- [ ] `, `---`, `![]()`) ou uma fence colada
    /// inteira (```lang\n…\n```), agenda a conversão do bloco. Aplicada no render (que tem
    /// `window`; o subscription de edit não tem) via `pending_convert`.
    fn detect_markdown(&mut self, block_id: usize, editor: Entity<Editor>, cx: &mut Context<Self>) {
        let Some(block) = self.blocks.iter().find(|b| b.id == block_id) else {
            return;
        };
        if block.kind != BlockKind::Paragraph {
            return;
        }
        let text = editor.read(cx).text(cx);

        // Fence FECHADA (abre e fecha com ```), colada em 1 linha OU multi-linha → bloco de
        // código. `lang` = 1º token; `body` = o resto até o ``` de fechamento. (Fence só
        // aberta, sem fechar, é o Enter que converte — ver split_block.)
        if let Some(rest) = text.trim_start().strip_prefix("```")
            && let Some(close) = rest.rfind("```")
        {
            let inner = &rest[..close];
            let (lang, body) = match inner.find(char::is_whitespace) {
                Some(i) => (inner[..i].trim().to_string(), inner[i..].trim().to_string()),
                None => (inner.trim().to_string(), String::new()),
            };
            self.pending_convert = Some((
                block_id,
                BlockKind::Code,
                body,
                (!lang.is_empty()).then_some(lang),
            ));
            cx.notify();
            return;
        }

        let (kind, stripped, data) = classify(&text);
        if kind != BlockKind::Paragraph {
            self.pending_convert = Some((block_id, kind, stripped, data));
            cx.notify();
        }
    }

    /// Aplica uma conversão de bloco: troca kind + estilo + texto (sem o marcador), e a
    /// linguagem quando é código. Precisa de `window` (chamada do render / do Enter).
    fn convert_block(
        &mut self,
        block_id: usize,
        kind: BlockKind,
        text: String,
        data: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(idx) = self.blocks.iter().position(|b| b.id == block_id) else {
            return;
        };
        let editor = self.blocks[idx].editor.clone();
        editor.update(cx, |e, cx| {
            e.set_text(text, window, cx);
            e.set_text_style_refinement(block_style(kind, cx));
            e.move_to_end(&editor::actions::MoveToEnd, window, cx);
        });
        self.blocks[idx].kind = kind;
        if kind == BlockKind::Code {
            if let Some(lang) = data {
                self.apply_code_language(editor, lang.clone(), cx);
                self.blocks[idx].lang = Some(lang);
            }
        } else {
            self.blocks[idx].data = data;
        }
        self.refresh_number_labels();
        self.body_list.remeasure_items(idx..idx + 1);
        self.refresh_md(block_id, cx);
        cx.notify();
        self.schedule_save(cx);
    }

    /// Detecta um `[[query` aberto (sem `]]`) antes do cursor → abre o autocomplete de notas.
    fn detect_wikilink(&mut self, block_id: usize, editor: Entity<Editor>, cx: &mut Context<Self>) {
        let text = editor.read(cx).text(cx);
        let query = text.rfind("[[").and_then(|pos| {
            let after = &text[pos + 2..];
            (!after.contains("]]") && !after.contains('\n')).then(|| after.to_string())
        });
        match query {
            Some(q) => {
                self.slash = None;
                self.link = Some(SlashState {
                    block_id,
                    query: q,
                    selected: 0,
                });
            }
            None => self.link = None,
        }
        cx.notify();
    }

    /// Notas cujo título casa com a query (autocomplete de `[[`). Exclui a nota atual.
    fn filtered_link_notes(&self, query: &str, cx: &App) -> Vec<String> {
        let q = query.trim().to_lowercase();
        let me = self.note_id.clone();
        NotebookDb::global(cx)
            .all_notes()
            .into_iter()
            .filter(|n| n.id != me && (q.is_empty() || n.title.to_lowercase().contains(&q)))
            .map(|n| n.title)
            .take(8)
            .collect()
    }

    /// Substitui o `[[query` pelo `[[Título]]` escolhido.
    fn insert_wikilink(
        &mut self,
        block_id: usize,
        title: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(b) = self.blocks.iter().find(|b| b.id == block_id) else {
            return;
        };
        let editor = b.editor.clone();
        let text = editor.read(cx).text(cx);
        if let Some(pos) = text.rfind("[[") {
            let next = format!("{}[[{title}]]", &text[..pos]);
            editor.update(cx, |e, cx| {
                e.set_text(next, window, cx);
                e.move_to_end(&editor::actions::MoveToEnd, window, cx);
            });
            // Autocomplete escolheu um wikilink → vira badge inline (crease).
            self.fold_wikilinks_in_editor(&editor, window, cx);
        }
    }

    fn slash_next(&mut self, _: &SlashNext, _window: &mut Window, cx: &mut Context<Self>) {
        if self.link.is_some() {
            let q = self
                .link
                .as_ref()
                .map(|s| s.query.clone())
                .unwrap_or_default();
            let n = self.filtered_link_notes(&q, cx).len();
            if let Some(s) = self.link.as_mut()
                && n > 0
            {
                s.selected = (s.selected + 1) % n;
            }
            cx.notify();
            return;
        }
        if let Some(state) = &mut self.slash {
            let n = filtered_menu(&state.query).len();
            if n > 0 {
                state.selected = (state.selected + 1) % n;
                self.scroll_to_selected(cx);
            }
        }
    }

    fn slash_prev(&mut self, _: &SlashPrev, _window: &mut Window, cx: &mut Context<Self>) {
        if self.link.is_some() {
            let q = self
                .link
                .as_ref()
                .map(|s| s.query.clone())
                .unwrap_or_default();
            let n = self.filtered_link_notes(&q, cx).len();
            if let Some(s) = self.link.as_mut()
                && n > 0
            {
                s.selected = (s.selected + n - 1) % n;
            }
            cx.notify();
            return;
        }
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
        // Fence: parágrafo começando com ```lang + Enter → vira bloco de código (em vez de
        // dividir). Depois é só digitar a fonte dentro (```plot → gráfico ao vivo).
        let ptext = self.blocks[idx].editor.read(cx).text(cx);
        if self.blocks[idx].kind == BlockKind::Paragraph
            && let Some(lang) = ptext.strip_prefix("```")
        {
            let lang = lang.trim().to_string();
            let bid = self.blocks[idx].id;
            self.convert_block(
                bid,
                BlockKind::Code,
                String::new(),
                (!lang.is_empty()).then_some(lang),
                window,
                cx,
            );
            return;
        }
        let cont = continuation_kind(self.blocks[idx].kind);
        let is_empty = self.blocks[idx]
            .editor
            .update(cx, |e, cx| e.text(cx).is_empty());

        // Enter num item de lista/citação/código VAZIO sai do modo (vira parágrafo),
        // igual ao Notion — em vez de criar mais um item vazio.
        if is_empty && cont != BlockKind::Paragraph {
            let editor = self.blocks[idx].editor.clone();
            editor.update(cx, |e, _| {
                e.set_text_style_refinement(style_for(BlockKind::Paragraph))
            });
            self.blocks[idx].kind = BlockKind::Paragraph;
            self.refresh_number_labels();
            self.body_list.remeasure_items(idx..idx + 1);
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
        self.block_inserted(idx + 1, handle.clone());
        window.focus(&handle, cx);
        cx.notify();
        self.schedule_save(cx);
    }

    fn merge_backward(
        &mut self,
        _: &MergeBlockBackward,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(idx) = self.focused_index(window, cx) else {
            return;
        };
        // Só funde/desce nível quando o cursor está no 1º caractere E NÃO há seleção.
        // Com seleção, backspace deve apagar a seleção (senão "some" pro bloco de cima).
        let at_start = self.blocks[idx].editor.update(cx, |e, cx| {
            let snapshot = e.snapshot(window, cx);
            let sel = e.selections.newest::<Point>(&snapshot.display_snapshot);
            let head = sel.head();
            sel.start == sel.end && head.row == 0 && head.column == 0
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
            self.refresh_number_labels();
            self.body_list.remeasure_items(idx..idx + 1);
            cx.notify();
            self.schedule_save(cx);
            return;
        }

        // No 1º caractere: bloco não-parágrafo vira parágrafo (fecha o modo lista/citação/código).
        if self.blocks[idx].kind != BlockKind::Paragraph {
            let editor = self.blocks[idx].editor.clone();
            editor.update(cx, |e, _| {
                e.set_text_style_refinement(style_for(BlockKind::Paragraph))
            });
            self.blocks[idx].kind = BlockKind::Paragraph;
            self.refresh_number_labels();
            self.body_list.remeasure_items(idx..idx + 1);
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
        if matches!(
            self.blocks[idx - 1].kind,
            BlockKind::Divider | BlockKind::Image
        ) {
            self.blocks.remove(idx - 1);
            self.block_removed(idx - 1);
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
        self.block_removed(idx);
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
        self.blocks_reordered(from.min(target)..from.max(target) + 1, cx);
        cx.notify();
        self.schedule_save(cx);
    }

    /// `+` no gutter: abre o menu de tipo pra a PRÓPRIA linha (transforma ela, mantendo
    /// o texto). Ex.: parágrafo "oi" → escolher Título → vira Título "oi". Não cria linha.
    fn open_type_menu(&mut self, block_id: usize, cx: &mut Context<Self>) {
        // Pré-seleciona o item do tipo ATUAL do bloco (discriminant p/ casar Todo(bool) etc.).
        let selected = self
            .blocks
            .iter()
            .find(|b| b.id == block_id)
            .map(|b| b.kind)
            .and_then(|kind| {
                filtered_menu("").iter().position(|item| {
                    std::mem::discriminant(&item.4) == std::mem::discriminant(&kind)
                })
            })
            .unwrap_or(0);
        self.slash = Some(SlashState {
            block_id,
            query: String::new(),
            selected,
        });
        // Rola o menu até o item pré-selecionado (senão fica escondido lá embaixo).
        self.scroll_to_selected(cx);
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
            let index = self.blocks.len();
            self.blocks.push(new);
            self.block_inserted(index, handle.clone());
            window.focus(&handle, cx);
            cx.notify();
        } else if let Some(last) = self.blocks.last() {
            self.body_list
                .scroll_to_reveal_item(self.blocks.len().saturating_sub(1));
            let handle = last.editor.focus_handle(cx);
            window.focus(&handle, cx);
        }
    }

    /// Tab: indenta a linha (no máximo um nível a mais que a linha de cima).
    fn indent(&mut self, _: &Indent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(idx) = self.focused_index(window, cx) else {
            return;
        };
        let max = if idx > 0 {
            self.blocks[idx - 1].indent + 1
        } else {
            0
        };
        let new = (self.blocks[idx].indent + 1).min(max);
        if new != self.blocks[idx].indent {
            self.blocks[idx].indent = new;
            self.refresh_number_labels();
            self.body_list.remeasure_items(idx..idx + 1);
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
            self.refresh_number_labels();
            self.body_list.remeasure_items(idx..idx + 1);
            cx.notify();
            self.schedule_save(cx);
        }
    }

    fn slash_confirm(&mut self, _: &SlashConfirm, window: &mut Window, cx: &mut Context<Self>) {
        // Autocomplete de wikilink tem prioridade.
        if let Some(state) = self.link.clone() {
            let notes = self.filtered_link_notes(&state.query, cx);
            if let Some(title) = notes.get(state.selected).cloned() {
                self.insert_wikilink(state.block_id, &title, window, cx);
            }
            self.link = None;
            cx.notify();
            return;
        }
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
        self.link = None;
        cx.notify();
    }

    /// Menu de autocomplete de `[[wikilink]]`: lista notas que casam com a query.
    fn render_link_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (query, selected) = self
            .link
            .as_ref()
            .map(|s| (s.query.clone(), s.selected))
            .unwrap_or_default();
        let notes = self.filtered_link_notes(&query, cx);
        let sel_bg = cx.theme().colors().element_selected;
        let hover = cx.theme().colors().element_hover;
        v_flex()
            .id("link-menu")
            .w(px(280.))
            .max_h(px(300.))
            .overflow_y_scroll()
            .bg(cx.theme().colors().elevated_surface_background)
            .rounded_md()
            .border_1()
            .border_color(cx.theme().colors().border)
            .p_1()
            .gap_0p5()
            .when(notes.is_empty(), |el| {
                el.child(
                    div().p_2().child(
                        Label::new("Nenhuma nota encontrada")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
                )
            })
            .children(notes.into_iter().enumerate().map(|(i, title)| {
                let t = title.clone();
                div()
                    .id(SharedString::from(format!("link-{i}")))
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .when(i == selected, |el| el.bg(sel_bg))
                    .hover(move |s| s.bg(hover))
                    .child(
                        Icon::new(IconName::FileDoc)
                            .size(IconSize::XSmall)
                            .color(Color::Muted),
                    )
                    .child(Label::new(title).size(LabelSize::Small))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(bid) = this.link.as_ref().map(|s| s.block_id) {
                            this.insert_wikilink(bid, &t, window, cx);
                            this.link = None;
                            cx.notify();
                        }
                    }))
            }))
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
        self.refresh_number_labels();
        self.remeasure_block(block_id);
        // Tabela nova: semeia um markdown inicial 2x2 se vazio, depois monta a grade de células.
        if kind == BlockKind::Table
            && let Some(block) = self.blocks.iter().find(|b| b.id == block_id)
        {
            let editor = block.editor.clone();
            if editor.read(cx).text(cx).trim().is_empty() {
                editor.update(cx, |e, cx| e.set_text(TABLE_STARTER, window, cx));
            }
            let text = editor.read(cx).text(cx);
            self.rebuild_table_cells(block_id, &text, window, cx);
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
                self.block_inserted(idx + 1, handle.clone());
                window.focus(&handle, cx);
            }
        }
        if kind == BlockKind::Image {
            self.pick_image(block_id, cx);
        }
        self.refresh_md(block_id, cx);
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
                    this.remeasure_block(block_id);
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
                    .on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.set_lang(block_id, lang.clone(), cx)
                        }),
                    )
            }))
    }

    fn markdown(&self, cx: &App) -> String {
        #[cfg(test)]
        SERIALIZED_NOTES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

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
                    // Citação multi-linha: cada linha ganha `> ` (pode conter fence de código).
                    BlockKind::Quote => {
                        return text
                            .lines()
                            .map(|l| format!("> {l}"))
                            .collect::<Vec<_>>()
                            .join("\n");
                    }
                    BlockKind::Code => {
                        return format!("```{}\n{text}\n```", b.lang.clone().unwrap_or_default());
                    }
                    // Tabela: serializa a grade de células de volta pro markdown.
                    BlockKind::Table => {
                        if b.cells.is_empty() {
                            return text;
                        }
                        let grid: Vec<Vec<String>> = b
                            .cells
                            .iter()
                            .map(|row| row.iter().map(|e| e.read(cx).text(cx)).collect())
                            .collect();
                        return table_to_markdown(&grid);
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
        let timer = cx.background_executor().timer(SAVE_DEBOUNCE);
        self.save_task = Some(cx.spawn(async move |this, cx| {
            timer.await;

            // O snapshot caro só acontece se este debounce não for substituído pelo próximo edit.
            let Ok((id, content, skill_front, title, is_meeting)) = this.update(cx, |this, cx| {
                let is_meeting = matches!(this.kind, DocKind::Meeting { .. });
                let title = this.title.read(cx).text(cx).trim().to_string();
                let title = if !title.is_empty() {
                    title
                } else if is_meeting {
                    "Reunião".to_string()
                } else {
                    "Nova nota".to_string()
                };
                let skill_front = match &this.kind {
                    DocKind::Skill { front } => Some(front.clone()),
                    _ => None,
                };
                (
                    this.note_id.clone(),
                    this.markdown(cx),
                    skill_front,
                    title,
                    is_meeting,
                )
            }) else {
                return;
            };

            if let Some(front) = skill_front {
                let path = skill_md_path(&id);
                let file = format!("---\n{}\n---\n\n{}\n", front.join("\n"), content.trim_end());
                std::fs::write(path, file).log_err();
                cx.update(|cx| crate::panel::refresh_panel(cx));
                return;
            }

            let db = cx.update(|cx| NotebookDb::global(cx));
            if is_meeting {
                db.update_meeting_content(id, title, content)
                    .await
                    .log_err();
            } else {
                db.update_note(id, title, content).await.log_err();
            }
            cx.update(|cx| crate::panel::refresh_panel(cx));
        }));
    }

    fn save_chrome(&mut self, cx: &mut Context<Self>) {
        // Skill não tem colunas icon/cover; nota e reunião persistem (cada uma na sua tabela).
        if matches!(self.kind, DocKind::Skill { .. }) {
            return;
        }
        let id = self.note_id.clone();
        let icon = self.icon.clone();
        let cover = self.cover.clone();
        let is_meeting = matches!(self.kind, DocKind::Meeting { .. });
        cx.spawn(async move |_this, cx| {
            let db = cx.update(|cx| NotebookDb::global(cx));
            if is_meeting {
                db.update_meeting_chrome(id, icon, cover).await.log_err();
            } else {
                db.update_note_chrome(id, icon, cover).await.log_err();
            }
            cx.update(|cx| crate::panel::refresh_panel(cx));
        })
        .detach();
    }

    /// Barra fixa da reunião: data + abas (Notas/Transcrição/Resumo/Uso). None p/ nota comum.
    fn render_meeting_tabbar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let DocKind::Meeting { date, .. } = &self.kind else {
            return None;
        };
        let date = meeting_date(date);
        Some(
            v_flex()
                .flex_none()
                .w_full()
                .px_12()
                .pt_1()
                .gap_1()
                .child(Label::new(date).size(LabelSize::Small).color(Color::Muted))
                .child(
                    h_flex()
                        .gap_2()
                        .child(self.meeting_tab_btn("Notas", MeetingTab::Notas, cx))
                        .child(self.meeting_tab_btn("Transcrição", MeetingTab::Transcricao, cx))
                        .child(self.meeting_tab_btn("Resumo", MeetingTab::Resumo, cx))
                        .child(self.meeting_tab_btn("Uso", MeetingTab::Uso, cx)),
                )
                .into_any_element(),
        )
    }

    fn meeting_tab_btn(
        &self,
        label: &'static str,
        which: MeetingTab,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = self.meeting_tab == which;
        div()
            .id(label)
            .px_2()
            .py_1()
            .cursor_pointer()
            .border_b_2()
            .border_color(if active {
                Color::Accent.color(cx)
            } else {
                gpui::transparent_black()
            })
            .child(Label::new(label).size(LabelSize::Small).color(if active {
                Color::Default
            } else {
                Color::Muted
            }))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.meeting_tab = which;
                cx.notify();
            }))
            .into_any_element()
    }

    /// Conteúdo da aba ativa da reunião (Transcrição/Resumo/Uso). Notas usa os blocos → None.
    fn render_meeting_section(&self, cx: &App) -> Option<AnyElement> {
        let DocKind::Meeting {
            transcript,
            summary,
            uso,
            ..
        } = &self.kind
        else {
            return None;
        };
        let placeholder = |txt: &str| {
            div()
                .w_full()
                .pt_4()
                .child(Label::new(txt.to_string()).color(Color::Muted))
                .into_any_element()
        };
        let section = match self.meeting_tab {
            MeetingTab::Notas => return None,
            MeetingTab::Transcricao if transcript.trim().is_empty() => {
                placeholder("Sem transcrição ainda.")
            }
            MeetingTab::Transcricao => render_convo(transcript, cx),
            MeetingTab::Resumo if summary.trim().is_empty() => placeholder("Sem resumo ainda."),
            MeetingTab::Resumo => div()
                .w_full()
                .pt_2()
                .text_ui(cx)
                .child(summary.clone())
                .into_any_element(),
            MeetingTab::Uso if uso.trim().is_empty() => {
                placeholder("A conversa completa com a IA do chat aparece aqui.")
            }
            MeetingTab::Uso => render_convo(uso, cx),
        };
        Some(div().w_full().mt_2().child(section).into_any_element())
    }
}

/// Data da reunião "dd/mm/aaaa hh:mm" a partir de RFC3339.
fn meeting_date(date: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(date)
        .map(|d| d.format("%d/%m/%Y %H:%M").to_string())
        .unwrap_or_else(|_| date.to_string())
}

/// Conversa "Falante: fala" por linha — falante numa coluna colorida (accent p/ "Você",
/// verde p/ os demais). Usado por Transcrição e Uso.
fn render_convo(text: &str, cx: &App) -> AnyElement {
    v_flex()
        .w_full()
        .gap_1p5()
        .children(text.lines().filter(|l| !l.trim().is_empty()).map(|line| {
            match line.split_once(": ") {
                Some((speaker, body)) => {
                    let color = if speaker.trim() == "Você" {
                        Color::Accent
                    } else {
                        Color::Success
                    };
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_start()
                        .child(
                            div().flex_none().w(px(90.)).child(
                                Label::new(speaker.trim().to_string())
                                    .size(LabelSize::Small)
                                    .color(color),
                            ),
                        )
                        // min_w(0): sem isso o filho flex não encolhe abaixo do conteúdo
                        // (min-width:auto) e o texto estoura/corta em vez de quebrar linha.
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .text_ui(cx)
                                .child(body.to_string()),
                        )
                        .into_any_element()
                }
                None => div()
                    .w_full()
                    .text_ui(cx)
                    .child(line.to_string())
                    .into_any_element(),
            }
        }))
        .into_any_element()
}

/// Notas cujo corpo referencia `[[title]]` (backlinks). Exclui a própria nota.
fn compute_backlinks(me_id: &str, title: &str, cx: &App) -> Vec<(String, String)> {
    let title = title.trim();
    if title.is_empty() {
        return Vec::new();
    }
    let needle = format!("[[{title}]]");
    NotebookDb::global(cx)
        .all_notes()
        .into_iter()
        .filter(|n| n.id != me_id && n.content.contains(&needle))
        .map(|n| (n.id, n.title))
        .collect()
}

/// Mapa título → ícone custom (emoji/imagem) de notas E reuniões — pro badge de wikilink.
fn compute_link_icons(cx: &App) -> HashMap<String, String> {
    let db = NotebookDb::global(cx);
    let mut map = HashMap::new();
    for n in db.all_notes() {
        if let Some(ic) = n.icon.filter(|s| !s.is_empty()) {
            map.insert(n.title, ic);
        }
    }
    for m in db.all_meetings() {
        if let Some(ic) = m.icon.filter(|s| !s.is_empty()) {
            map.entry(m.title).or_insert(ic);
        }
    }
    map
}

/// Offset (byte) do começo da linha `row` + `col` colunas.
fn offset_of(text: &str, row: usize, col: usize) -> usize {
    let mut off = 0usize;
    for (i, line) in text.split('\n').enumerate() {
        if i == row {
            return (off + col).min(text.len());
        }
        off += line.len() + 1;
    }
    text.len()
}

/// Markdown do bloco COM o prefixo do tipo (pro render WYSIWYG mostrar heading/quote certo).
/// Ex.: H2 "Oi" → "## Oi"; Quote multi-linha → cada linha com "> ".
fn block_markdown(kind: BlockKind, text: &str) -> String {
    match kind {
        BlockKind::H1 => format!("# {text}"),
        BlockKind::H2 => format!("## {text}"),
        BlockKind::H3 => format!("### {text}"),
        BlockKind::Quote => text
            .lines()
            .map(|l| format!("> {l}"))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => text.to_string(),
    }
}

/// Markdown de tabela → grade de strings (linhas × colunas). Pula a linha separadora
/// (`| --- | --- |`) e normaliza todas as linhas pro maior nº de colunas.
fn parse_table(text: &str) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            continue;
        }
        let cells: Vec<String> = line
            .trim_matches('|')
            .split('|')
            .map(|c| c.trim().to_string())
            .collect();
        // Linha separadora: todas as células só com `-`/`:`/espaço (e não vazias).
        let is_sep = !cells.is_empty()
            && cells
                .iter()
                .all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':' | ' ')));
        if is_sep {
            continue;
        }
        rows.push(cells);
    }
    let ncols = rows.iter().map(|r| r.len()).max().unwrap_or(2).max(1);
    for r in &mut rows {
        while r.len() < ncols {
            r.push(String::new());
        }
    }
    if rows.is_empty() {
        rows.push(vec![String::new(), String::new()]);
    }
    rows
}

/// Grade de strings → markdown de tabela (com a linha separadora após o cabeçalho).
fn table_to_markdown(grid: &[Vec<String>]) -> String {
    if grid.is_empty() {
        return String::new();
    }
    let ncols = grid[0].len().max(1);
    let row_md = |r: &[String]| {
        format!(
            "| {} |",
            r.iter().map(|c| c.trim()).collect::<Vec<_>>().join(" | ")
        )
    };
    let mut out = vec![
        row_md(&grid[0]),
        format!("| {} |", vec!["---"; ncols].join(" | ")),
    ];
    for r in &grid[1..] {
        out.push(row_md(r));
    }
    out.join("\n")
}

/// Tipos que renderizam via markdown (WYSIWYG) quando fora de foco.
fn kind_renders_md(kind: BlockKind) -> bool {
    matches!(
        kind,
        BlockKind::Paragraph
            | BlockKind::Quote
            | BlockKind::H1
            | BlockKind::H2
            | BlockKind::H3
            // Listas também renderizam o conteúdo (negrito, código, links) fora de foco;
            // o marcador (•, número, checkbox) é desenhado por nós.
            | BlockKind::Bullet
            | BlockKind::Numbered
            | BlockKind::Todo(_)
    )
}

/// Marcador visual de listas (•, número, checkbox), compartilhado entre a view renderizada
/// e a de edição. `None` para tipos sem marcador (parágrafo, quote, títulos).
fn is_list_kind(kind: BlockKind) -> bool {
    matches!(
        kind,
        BlockKind::Bullet | BlockKind::Numbered | BlockKind::Todo(_)
    )
}

/// Se o texto é EXATAMENTE um `[[wikilink]]` (nada mais), retorna o título — pra renderizar
/// como badge no lugar do parágrafo cru.
fn lone_wikilink(text: &str) -> Option<String> {
    let inner = text.trim().strip_prefix("[[")?.strip_suffix("]]")?;
    (!inner.contains("[[") && !inner.trim().is_empty()).then(|| inner.trim().to_string())
}

/// Acha todos os `[[Título]]` no texto → (offset de `[[`, offset após `]]`, título trimado).
fn find_wikilinks(text: &str) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    let mut search = 0;
    while let Some(rel) = text[search..].find("[[") {
        let open = search + rel;
        let Some(rel_close) = text[open + 2..].find("]]") else {
            break;
        };
        let close = open + 2 + rel_close;
        let title = &text[open + 2..close];
        if !title.contains("[[") && !title.contains('\n') && !title.trim().is_empty() {
            out.push((open, close + 2, title.trim().to_string()));
        }
        search = close + 2;
    }
    out
}

/// Render do badge de wikilink (crease) dentro da nota — MESMO componente do chat: ícone
/// custom/por-tipo + nome; clicar ABRE a nota linkada; hover troca o ícone por um X que remove.
fn wikilink_badge_render(
    title: String,
    icon: String,
    range: std::ops::Range<Anchor>,
    editor: WeakEntity<Editor>,
    note_doc: WeakEntity<NoteDoc>,
) -> Arc<dyn Send + Sync + Fn(FoldId, std::ops::Range<Anchor>, &mut App) -> AnyElement> {
    Arc::new(move |_fold_id, _fold_range, cx| {
        let title = title.clone();
        let icon = icon.clone();
        let range = range.clone();
        let editor = editor.clone();
        let note_doc = note_doc.clone();
        let g = SharedString::from("wl-badge");
        let icon_el = if icon.contains('/') || icon.contains('\\') {
            img(PathBuf::from(icon.clone()))
                .size(px(13.))
                .rounded_sm()
                .into_any_element()
        } else if !icon.is_empty() {
            div()
                .text_size(px(12.))
                .child(icon.clone())
                .into_any_element()
        } else {
            Icon::new(IconName::FileDoc)
                .size(IconSize::XSmall)
                .color(Color::Accent)
                .into_any_element()
        };
        h_flex()
            .id(SharedString::from(format!("wl-badge-{title}")))
            .group(g.clone())
            .flex_none()
            .items_center()
            .gap_1()
            .px_1()
            .rounded_sm()
            .border_1()
            .border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().element_background)
            .cursor_pointer()
            .on_click({
                let title = title.clone();
                let note_doc = note_doc.clone();
                move |_e, _window, cx| {
                    let title = title.clone();
                    note_doc
                        .update(cx, |_doc, cx| {
                            cx.emit(NoteDocEvent::OpenNote(title.clone()))
                        })
                        .ok();
                }
            })
            .child(
                div()
                    .relative()
                    .flex_none()
                    .size(px(14.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .group_hover(g.clone(), |s| s.invisible())
                            .child(icon_el),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("wl-x-{title}")))
                            .absolute()
                            .inset_0()
                            .invisible()
                            .flex()
                            .items_center()
                            .justify_center()
                            .group_hover(g.clone(), |s| s.visible())
                            .cursor_pointer()
                            .child(
                                Icon::new(IconName::Close)
                                    .size(IconSize::XSmall)
                                    .color(Color::Muted),
                            )
                            .on_click(move |_e, _window, cx| {
                                cx.stop_propagation();
                                let range = range.clone();
                                editor
                                    .update(cx, |editor, cx| {
                                        editor.unfold_ranges(
                                            std::slice::from_ref(&range),
                                            true,
                                            false,
                                            cx,
                                        );
                                        editor.edit([(range.clone(), "")], cx);
                                    })
                                    .ok();
                            }),
                    ),
            )
            .child(
                Label::new(title)
                    .size(LabelSize::Small)
                    .color(Color::Accent),
            )
            .into_any_element()
    })
}

/// Título do `[[wikilink]]` que contém o offset do cursor, se houver.
fn wikilink_at(text: &str, offset: usize) -> Option<String> {
    let before = text.get(..offset.min(text.len()))?;
    let open = before.rfind("[[")?;
    let rest = text.get(open + 2..)?;
    let close_rel = rest.find("]]")?;
    let close = open + 2 + close_rel;
    if offset <= close + 2 {
        let title = text.get(open + 2..close)?.trim().to_string();
        return (!title.is_empty()).then_some(title);
    }
    None
}

/// Caminho do SKILL.md de uma skill (~/.claude/skills/<name>/SKILL.md).
fn skill_md_path(name: &str) -> std::path::PathBuf {
    paths::home_dir()
        .join(".claude")
        .join("skills")
        .join(name)
        .join("SKILL.md")
}

fn filtered_menu(
    query: &str,
) -> Vec<&'static (
    &'static str,
    &'static str,
    &'static str,
    IconName,
    BlockKind,
)> {
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
        div()
            .text_size(px(56.))
            .child(icon.to_string())
            .into_any_element()
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
    if matches!(kind, BlockKind::Code | BlockKind::Table) {
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
        // Tabela: linhas consecutivas começando com `|` viram um bloco Table.
        if line.trim_start().starts_with('|') {
            let mut rows = vec![line.to_string()];
            while let Some(l) = lines.peek() {
                if l.trim_start().starts_with('|') {
                    rows.push(lines.next().unwrap().to_string());
                } else {
                    break;
                }
            }
            out.push((BlockKind::Table, rows.join("\n"), None, 0));
            continue;
        }
        // Citação: linhas consecutivas `> ` viram UM bloco Quote multi-linha (que pode conter
        // um bloco de código etc. — "blocos dentro de blocos" via markdown aninhado).
        let is_quote_line = |l: &str| {
            let t = l.trim_start();
            t == ">" || t.starts_with("> ")
        };
        if is_quote_line(line) {
            let strip = |l: &str| {
                let t = l.trim_start();
                t.strip_prefix("> ")
                    .or_else(|| t.strip_prefix('>'))
                    .unwrap_or(t)
                    .to_string()
            };
            let mut qlines = vec![strip(line)];
            while let Some(l) = lines.peek() {
                if is_quote_line(l) {
                    qlines.push(strip(lines.next().unwrap()));
                } else {
                    break;
                }
            }
            out.push((BlockKind::Quote, qlines.join("\n"), None, 0));
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

impl NoteDoc {
    fn render_body_item(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(block) = self.blocks.get(index) else {
            return v_flex()
                .when(!self.backlinks.is_empty(), |element| {
                    element.child(self.render_backlinks(cx))
                })
                .child(
                    div()
                        .id("note-tail")
                        .w_full()
                        .min_h(px(160.))
                        .cursor_text()
                        .on_click(
                            cx.listener(|this, _, window, cx| this.ensure_trailing(window, cx)),
                        ),
                )
                .pb_16()
                .into_any_element();
        };

        let number_label = self
            .number_labels
            .get(index)
            .map(String::as_str)
            .unwrap_or_default();
        let block_element = self
            .render_block(block, number_label, window, cx)
            .into_any_element();
        let slash_here = self
            .slash
            .as_ref()
            .is_some_and(|state| state.block_id == block.id);
        let link_here = self
            .link
            .as_ref()
            .is_some_and(|state| state.block_id == block.id);
        let row = if slash_here || link_here {
            let menu = if slash_here {
                self.render_slash_menu(block.id, cx).into_any_element()
            } else {
                self.render_link_menu(cx).into_any_element()
            };
            div()
                .relative()
                .child(block_element)
                .child(gpui::deferred(
                    div().absolute().top_full().left_0().child(
                        gpui::anchored()
                            .snap_to_window_with_margin(px(8.))
                            .child(menu),
                    ),
                ))
                .into_any_element()
        } else {
            block_element
        };

        div()
            .w_full()
            .ml(px(-48.))
            .when(index == 0, |element| element.mt_2())
            .pb_1()
            .child(row)
            .into_any_element()
    }
}

impl Focusable for NoteDoc {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for NoteDoc {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        debug_assert_eq!(self.body_list.item_count(), self.blocks.len() + 1);
        debug_assert_eq!(self.number_labels.len(), self.blocks.len());

        // Aplica conversão markdown pendente (detectada num edit) — aqui há `window`.
        if let Some((bid, kind, text, data)) = self.pending_convert.take() {
            self.convert_block(bid, kind, text, data, window, cx);
        }
        // Reunião: barra de abas (Notas/Transcrição/Resumo/Uso) e conteúdo só da aba ativa.
        // Nota comum: sempre mostra os blocos.
        let show_blocks =
            !matches!(self.kind, DocKind::Meeting { .. }) || self.meeting_tab == MeetingTab::Notas;
        let tabbar = self.render_meeting_tabbar(cx);
        let scroll_body: AnyElement = if show_blocks {
            let body_list = self.body_list.clone();
            div()
                .flex_1()
                .min_h(px(0.))
                .w_full()
                .relative()
                .child(
                    gpui::list(body_list.clone(), cx.processor(Self::render_body_item))
                        .size_full()
                        .px_12(),
                )
                .custom_scrollbars(
                    Scrollbars::new(ScrollAxes::Vertical).tracked_scroll_handle(&body_list),
                    window,
                    cx,
                )
                .into_any_element()
        } else {
            let body = self
                .render_meeting_section(cx)
                .unwrap_or_else(|| div().into_any_element());
            div()
                .flex_1()
                .min_h(px(0.))
                .w_full()
                .relative()
                .child(
                    v_flex()
                        .id("note-scroll")
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.body_scroll)
                        .px_12()
                        .pb_16()
                        .child(body),
                )
                .custom_scrollbars(
                    Scrollbars::new(ScrollAxes::Vertical).tracked_scroll_handle(&self.body_scroll),
                    window,
                    cx,
                )
                .into_any_element()
        };

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
                    this.remeasure_block(id);
                    this.img_resize = Some((id, x));
                    cx.notify();
                    this.schedule_save(cx);
                },
            ))
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, _| this.img_resize = None),
            )
            // Arrastar nota/reunião/pasta da sidebar pra cá → insere um [[wikilink]].
            .drag_over::<crate::panel::DraggedNotebookItem>(|el, _, _, cx| {
                el.bg(cx.theme().colors().element_hover)
            })
            .on_drop(cx.listener(
                |this, item: &crate::panel::DraggedNotebookItem, window, cx| {
                    this.drop_wikilink(item.label.clone(), window, cx)
                },
            ))
            // Capa e cabeçalho ficam fixos no topo.
            .child(self.render_cover(cx))
            .child(
                div()
                    .flex_none()
                    .w_full()
                    .px_12()
                    .pt_2()
                    .child(self.render_header(cx)),
            )
            // Abas da reunião (fixas, só p/ reunião).
            .children(tabbar)
            .child(scroll_body)
    }
}

impl NoteDoc {
    fn render_slash_menu(&self, block_id: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self
            .slash
            .as_ref()
            .map(|s| s.query.clone())
            .unwrap_or_default();
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
                    .child(
                        Label::new("Nenhum bloco")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
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
            let color = COVERS
                .iter()
                .find(|(c, _)| *c == cover)
                .map(|(_, rgb)| *rgb);
            // Altura proporcional à LARGURA (banner ~3.5:1): nota mais larga → capa mais alta,
            // mais estreita → mais baixa. `aspect_ratio` = largura/altura (taffy).
            el.w_full()
                .flex_none()
                .aspect_ratio(3.5)
                .max_h(px(360.))
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
                            .child(
                                Button::new("cover-image", "Imagem")
                                    .on_click(cx.listener(|this, _, _, cx| this.pick_cover(cx))),
                            ),
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
                    div().absolute().top(px(72.)).left_0().child(
                        gpui::anchored()
                            .snap_to_window_with_margin(px(8.))
                            .child(self.render_emoji_grid(cx)),
                    ),
                ))
            })
            .child(
                h_flex()
                    .gap_2()
                    .when(self.icon.is_none(), |el| {
                        el.child(
                            Button::new("add-icon", "Adicionar ícone").on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.icon_menu_open = !this.icon_menu_open;
                                    cx.notify();
                                },
                            )),
                        )
                    })
                    .when(self.cover.is_none(), |el| {
                        el.child(
                            Button::new("add-cover", "Adicionar capa").on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.cover = Some("1".to_string());
                                    this.save_chrome(cx);
                                    cx.notify();
                                },
                            )),
                        )
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
                    .child(
                        Icon::new(IconName::Menu)
                            .size(IconSize::Small)
                            .color(Color::Muted),
                    ),
            )
    }

    /// Preview renderizado de um code block "rico" (```plot). None p/ blocos normais ou
    /// quando o render falha → cai de volta em mostrar só o código (degradação graciosa).
    fn rich_preview(&self, block: &Block, cx: &App) -> Option<AnyElement> {
        let lang = block.lang.as_deref()?;
        if !crate::rich_block::is_rich_lang(lang) {
            return None;
        }
        let source = block.editor.read(cx).text(cx);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        lang.hash(&mut hasher);
        source.hash(&mut hasher);
        let key = hasher.finish();
        let renderer = cx.svg_renderer();
        let scale = crate::rich_block::rich_scale(lang);
        let image = RICH_CACHE.with(|c| {
            c.borrow_mut()
                .entry(key)
                .or_insert_with(|| {
                    let svg = crate::rich_block::render_rich_svg(lang, &source)?;
                    renderer.render_single_frame(svg.as_bytes(), scale).ok()
                })
                .clone()
        })?;
        Some(
            div()
                .pt_2()
                .w_full()
                .flex()
                .justify_center()
                .child(img(image).max_w(px(620.)))
                .into_any_element(),
        )
    }

    /// Atualiza o render markdown de um bloco a partir do seu tipo+texto atuais.
    /// Chamado a cada edit e após mudança de tipo (converter/menu).
    fn refresh_md(&self, block_id: usize, cx: &mut Context<Self>) {
        if let Some(b) = self.blocks.iter().find(|b| b.id == block_id) {
            let src = block_markdown(b.kind, &b.editor.read(cx).text(cx));
            if let Some(md) = b.md.borrow().clone() {
                md.update(cx, |m, cx| m.replace(src, cx));
            }
        }
    }

    /// + Linha: adiciona uma linha vazia no fim da tabela (nº de colunas = da 1ª linha).
    fn add_table_row(&mut self, block_id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let cols = self
            .blocks
            .iter()
            .find(|b| b.id == block_id)
            .and_then(|b| b.cells.first())
            .map_or(2, |r| r.len())
            .max(1);
        // Cria os editores da nova linha ANTES de pegar o bloco emprestado mutável.
        let mut new_row = Vec::new();
        let mut new_subs = Vec::new();
        for _ in 0..cols {
            let (ed, sub) = self.make_cell_editor(block_id, "", window, cx);
            new_row.push(ed);
            new_subs.push(sub);
        }
        if let Some(b) = self.blocks.iter_mut().find(|b| b.id == block_id) {
            b.cells.push(new_row);
            b._cell_subs.extend(new_subs);
        }
        self.remeasure_block(block_id);
        cx.notify();
        self.schedule_save(cx);
    }

    /// + Coluna: adiciona uma célula em cada linha da grade.
    fn add_table_col(&mut self, block_id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let nrows = self
            .blocks
            .iter()
            .find(|b| b.id == block_id)
            .map_or(0, |b| b.cells.len());
        let mut new_cells = Vec::new();
        let mut new_subs = Vec::new();
        for _ in 0..nrows {
            let (ed, sub) = self.make_cell_editor(block_id, "", window, cx);
            new_cells.push(ed);
            new_subs.push(sub);
        }
        if let Some(b) = self.blocks.iter_mut().find(|b| b.id == block_id) {
            for (row, ed) in b.cells.iter_mut().zip(new_cells) {
                row.push(ed);
            }
            b._cell_subs.extend(new_subs);
        }
        self.remeasure_block(block_id);
        cx.notify();
        self.schedule_save(cx);
    }

    /// Segue o `[[wikilink]]` sob o cursor do bloco (Cmd/Ctrl+clique) → abre a nota.
    fn follow_link(&mut self, block_id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(block) = self.blocks.iter().find(|b| b.id == block_id) else {
            return;
        };
        let (text, point) = block.editor.update(cx, |e, cx| {
            let snap = e.snapshot(window, cx);
            let head = e.selections.newest::<Point>(&snap.display_snapshot).head();
            (e.text(cx), head)
        });
        let offset = offset_of(&text, point.row as usize, point.column as usize);
        if let Some(title) = wikilink_at(&text, offset) {
            cx.emit(NoteDocEvent::OpenNote(title));
        }
    }

    /// Soltar uma nota/reunião/pasta (da sidebar) na nota → insere `[[Título]]` no bloco
    /// focado (ou num parágrafo novo). Espelha o "chip" que o chat cria ao receber o drop.
    fn drop_wikilink(&mut self, title: String, window: &mut Window, cx: &mut Context<Self>) {
        let title = title.trim();
        if title.is_empty() {
            return;
        }
        let link = format!("[[{title}]]");
        if let Some(idx) = self.focused_index(window, cx) {
            let editor = self.blocks[idx].editor.clone();
            editor.update(cx, |e, cx| {
                let cur = e.text(cx);
                let sep = if cur.is_empty() || cur.ends_with([' ', '\n']) {
                    ""
                } else {
                    " "
                };
                e.set_text(format!("{cur}{sep}{link}"), window, cx);
                e.move_to_end(&editor::actions::MoveToEnd, window, cx);
            });
            // `[[Título]]` recém-inserido → badge inline (crease).
            self.fold_wikilinks_in_editor(&editor, window, cx);
        } else {
            let block = self.make_block(BlockKind::Paragraph, &link, window, cx);
            let handle = block.editor.focus_handle(cx);
            let pos = self.blocks.len().saturating_sub(1);
            self.blocks.insert(pos, block);
            self.block_inserted(pos, handle);
        }
        cx.notify();
        self.schedule_save(cx);
    }

    /// Rodapé de backlinks: notas que apontam pra esta (clicáveis → abrem).
    fn render_backlinks(&self, cx: &mut Context<Self>) -> AnyElement {
        let hover = cx.theme().colors().element_hover;
        v_flex()
            .mt_8()
            .gap_1()
            .child(
                Label::new("REFERÊNCIAS")
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            )
            .children(self.backlinks.iter().map(|(id, title)| {
                let t = title.clone();
                div()
                    .id(SharedString::from(format!("backlink-{id}")))
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .child(
                        Label::new(title.clone())
                            .size(LabelSize::Small)
                            .color(Color::Accent),
                    )
                    .on_click(cx.listener(move |_this, _, _, cx| {
                        cx.emit(NoteDocEvent::OpenNote(t.clone()))
                    }))
            }))
            .into_any_element()
    }

    /// Marcador de lista (•, número, checkbox) desenhado por nós — compartilhado entre a
    /// view renderizada e a de edição. `None` para tipos sem marcador.
    fn list_marker(
        &self,
        kind: BlockKind,
        id: usize,
        number_label: &str,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        match kind {
            BlockKind::Bullet => Some(
                div()
                    .pt(px(4.))
                    .child(Label::new("•").color(Color::Muted))
                    .into_any_element(),
            ),
            BlockKind::Numbered => Some(
                div()
                    .pt(px(2.))
                    .child(Label::new(format!("{number_label}.")).color(Color::Muted))
                    .into_any_element(),
            ),
            BlockKind::Todo(done) => Some(
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
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_todo(id, cx)))
                    .into_any_element(),
            ),
            _ => None,
        }
    }

    /// Dobra (fold) cada `[[wikilink]]` do editor num crease → vira badge inline (ícone + nome,
    /// click abre, hover→X remove). Mesmo componente do chat. Chamado ao criar/carregar o bloco
    /// e ao inserir um wikilink (drag/autocomplete).
    fn fold_wikilinks_in_editor(
        &self,
        editor: &Entity<Editor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = editor.read(cx).text(cx);
        let links = find_wikilinks(&text);
        if links.is_empty() {
            return;
        }
        let icons: Vec<String> = links
            .iter()
            .map(|(_, _, t)| self.link_icons.get(t).cloned().unwrap_or_default())
            .collect();
        let note_doc = cx.weak_entity();
        let editor_weak = editor.downgrade();
        editor.update(cx, |editor, cx| {
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            let mut creases = Vec::new();
            for ((start_off, end_off, title), icon) in links.iter().zip(icons) {
                let range = snapshot.anchor_before(MultiBufferOffset(*start_off))
                    ..snapshot.anchor_after(MultiBufferOffset(*end_off));
                let render = wikilink_badge_render(
                    title.clone(),
                    icon,
                    range.clone(),
                    editor_weak.clone(),
                    note_doc.clone(),
                );
                creases.push(Crease::Inline {
                    range: range.clone(),
                    placeholder: FoldPlaceholder {
                        render,
                        merge_adjacent: false,
                        ..Default::default()
                    },
                    render_toggle: None,
                    render_trailer: None,
                    metadata: Some(CreaseMetadata {
                        label: title.clone().into(),
                        icon_path: IconName::FileDoc.path().into(),
                    }),
                });
            }
            editor.insert_creases(creases.clone(), cx);
            editor.fold_creases(creases, false, window, cx);
        });
    }

    fn render_block(
        &self,
        block: &Block,
        number_label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        #[cfg(test)]
        RENDERED_BLOCKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let id = block.id;
        let indent_px = block.indent as f32 * 24.0;
        // "SlashMenu" também quando o autocomplete de `[[` está ativo (mesmas teclas).
        let is_slash = self.slash.as_ref().is_some_and(|s| s.block_id == id)
            || self.link.as_ref().is_some_and(|s| s.block_id == id);

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
                } else if matches!(
                    block.kind,
                    BlockKind::Code | BlockKind::Table | BlockKind::Quote
                ) {
                    "NoteCode"
                } else {
                    "NoteBlock"
                };
                // Parágrafo FORA de foco → markdown renderizado (esconde `**` etc.); clicar
                // volta pro editor. Demais blocos (e o focado) usam o editor cru.
                let focused = block.editor.focus_handle(cx).is_focused(window);
                let text = block.editor.read(cx).text(cx);
                // Blocos com `[[wikilink]]` SEMPRE usam o editor (os creases/badges só
                // aparecem nele, não no MarkdownElement).
                let render_md = kind_renders_md(block.kind)
                    && !focused
                    && !text.trim().is_empty()
                    && !text.contains("[[");
                if render_md {
                    if !is_list_kind(block.kind)
                        && let Some(link_title) = lone_wikilink(&text)
                    {
                        // Parágrafo que é só `[[X]]` → BADGE (ícone + nome), clicável → abre.
                        let t = link_title.clone();
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .child(
                                h_flex()
                                    .id(SharedString::from(format!("wl-{id}")))
                                    .flex_none()
                                    .w_auto()
                                    .px_2()
                                    .py(px(2.))
                                    .gap_1()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(cx.theme().colors().border)
                                    .bg(cx.theme().colors().element_background)
                                    .cursor_pointer()
                                    .hover(|s| s.bg(cx.theme().colors().element_hover))
                                    .child(
                                        match self
                                            .link_icons
                                            .get(&link_title)
                                            .filter(|s| !s.is_empty())
                                        {
                                            Some(ic) if ic.contains('/') || ic.contains('\\') => {
                                                img(PathBuf::from(ic.clone()))
                                                    .size(px(14.))
                                                    .rounded_sm()
                                                    .object_fit(gpui::ObjectFit::Cover)
                                                    .into_any_element()
                                            }
                                            Some(ic) => div()
                                                .text_size(px(13.))
                                                .child(ic.clone())
                                                .into_any_element(),
                                            None => Icon::new(IconName::FileDoc)
                                                .size(IconSize::XSmall)
                                                .color(Color::Accent)
                                                .into_any_element(),
                                        },
                                    )
                                    .child(
                                        Label::new(link_title)
                                            .size(LabelSize::Small)
                                            .color(Color::Accent),
                                    )
                                    .on_click(cx.listener(move |_this, _, _, cx| {
                                        cx.emit(NoteDocEvent::OpenNote(t.clone()))
                                    })),
                            )
                            .into_any_element()
                    } else {
                        // Conteúdo renderizado (negrito, código, links). Clicar → volta pro editor.
                        let cached_md = block.md.borrow().clone();
                        let md = if let Some(md) = cached_md {
                            md
                        } else {
                            #[cfg(test)]
                            CREATED_MARKDOWNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let source = block_markdown(block.kind, &text);
                            let languages = self.languages.clone();
                            let md = cx
                                .new(|cx| Markdown::new(source.into(), Some(languages), None, cx));
                            *block.md.borrow_mut() = Some(md.clone());
                            md
                        };
                        let md_el = div()
                            .id(SharedString::from(format!("md-{id}")))
                            .flex_1()
                            .min_w(px(0.))
                            .cursor_text()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(b) = this.blocks.iter().find(|b| b.id == id) {
                                    let handle = b.editor.focus_handle(cx);
                                    window.focus(&handle, cx);
                                }
                            }))
                            .child(MarkdownElement::new(
                                md,
                                MarkdownStyle::themed(MarkdownFont::Preview, window, cx),
                            ));
                        // Listas mantêm o marcador (•, número, checkbox) ao lado do conteúdo.
                        match self.list_marker(block.kind, id, number_label, cx) {
                            Some(marker) => h_flex()
                                .w_full()
                                .gap_2()
                                .items_start()
                                .child(marker)
                                .child(md_el)
                                .into_any_element(),
                            None => md_el.into_any_element(),
                        }
                    }
                } else {
                    let body = div().flex_1().min_w(px(0.)).child(block.editor.clone());
                    let row = h_flex().key_context(ctx).w_full().gap_2().items_start();
                    match block.kind {
                        BlockKind::Bullet | BlockKind::Numbered | BlockKind::Todo(_) => row
                            .children(self.list_marker(block.kind, id, number_label, cx))
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
                        BlockKind::Table => {
                            let border = cx.theme().colors().border;
                            let hover = cx.theme().colors().element_hover;
                            let header_bg = cx.theme().colors().element_background;
                            // Grade editável: cada célula é seu próprio editor. Clicar numa célula
                            // edita SÓ aquele texto — a tabela nunca vira markdown cru.
                            let grid = v_flex().flex_1().overflow_hidden().children(
                                block.cells.iter().enumerate().map(|(r, cells_row)| {
                                    let is_header = r == 0;
                                    h_flex()
                                        .w_full()
                                        .items_stretch()
                                        .when(r > 0, |el| el.border_t_1().border_color(border))
                                        .children(cells_row.iter().enumerate().map(|(c, cell)| {
                                            div()
                                                .flex_1()
                                                .min_w(px(60.))
                                                .px_2()
                                                .py_1()
                                                .when(c > 0, |el| {
                                                    el.border_l_1().border_color(border)
                                                })
                                                .when(is_header, |el| el.bg(header_bg))
                                                .child(cell.clone())
                                        }))
                                }),
                            );
                            row.child(
                                div()
                                    .flex_1()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(border)
                                    .overflow_hidden()
                                    .child(grid)
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .px_2()
                                            .pb_1()
                                            .child(
                                                div()
                                                    .id(SharedString::from(format!("addrow-{id}")))
                                                    .px_2()
                                                    .rounded_sm()
                                                    .cursor_pointer()
                                                    .hover(move |s| s.bg(hover))
                                                    .child(
                                                        Label::new("+ Linha")
                                                            .size(LabelSize::XSmall)
                                                            .color(Color::Muted),
                                                    )
                                                    .on_click(cx.listener(
                                                        move |this, _, window, cx| {
                                                            this.add_table_row(id, window, cx)
                                                        },
                                                    )),
                                            )
                                            .child(
                                                div()
                                                    .id(SharedString::from(format!("addcol-{id}")))
                                                    .px_2()
                                                    .rounded_sm()
                                                    .cursor_pointer()
                                                    .hover(move |s| s.bg(hover))
                                                    .child(
                                                        Label::new("+ Coluna")
                                                            .size(LabelSize::XSmall)
                                                            .color(Color::Muted),
                                                    )
                                                    .on_click(cx.listener(
                                                        move |this, _, window, cx| {
                                                            this.add_table_col(id, window, cx)
                                                        },
                                                    )),
                                            ),
                                    ),
                            )
                            .into_any_element()
                        }
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
                                    // Bloco rico (```plot): mostra a imagem renderizada abaixo da fonte.
                                    .when_some(self.rich_preview(block, cx), |el, preview| {
                                        el.child(div().px_2().pb_2().child(preview))
                                    })
                                    .when(menu_open, |el| {
                                        el.child(gpui::deferred(
                                            div().absolute().top_8().right_2().child(
                                                // snap_to_window: reposiciona pra não cortar na borda.
                                                gpui::anchored()
                                                    .snap_to_window_with_margin(px(8.))
                                                    .child(self.render_lang_menu(id, cx)),
                                            ),
                                        ))
                                    }),
                            )
                            .into_any_element()
                        }
                        _ => row.child(body).into_any_element(),
                    }
                }
            }
        };

        // Envelope comum: gutter (hover, em fluxo) + alvo de drop pra reordenar.
        h_flex()
            .group("blk")
            .w_full()
            .items_start()
            .drag_over::<DraggedBlock>(|el, _, _, cx| {
                el.border_t_2()
                    .border_color(cx.theme().colors().border_focused)
            })
            .on_drop(cx.listener(move |this, dragged: &DraggedBlock, _, cx| {
                this.move_block(dragged.0, id, cx)
            }))
            .child(self.render_gutter(id, cx))
            .child(
                div()
                    .id(SharedString::from(format!("blk-content-{id}")))
                    .flex_1()
                    .min_w(px(0.))
                    .pl(px(indent_px))
                    // Cmd/Ctrl+clique num [[wikilink]] segue o link (clique normal edita).
                    .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, window, cx| {
                        if ev.modifiers().secondary() {
                            this.follow_link(id, window, cx);
                        }
                    }))
                    .child(content),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    use settings::SettingsStore;
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    #[gpui::test]
    fn large_note_only_materializes_viewport_blocks(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings = SettingsStore::test(cx);
            cx.set_global(settings);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
        });

        let languages = Arc::new(LanguageRegistry::test(cx.executor()));
        let body = (0..1_000)
            .map(|index| format!("Paragrafo {index}"))
            .collect::<Vec<_>>()
            .join("\n");

        RENDERED_BLOCKS.store(0, Ordering::Relaxed);
        CREATED_MARKDOWNS.store(0, Ordering::Relaxed);
        let open_started = Instant::now();
        let window = cx.add_window(move |window, cx| {
            NoteDoc::new(
                Note {
                    id: "large-note-regression".into(),
                    folder_id: None,
                    title: "Large note regression".into(),
                    content: body,
                    sort_order: 0,
                    icon: None,
                    cover: None,
                },
                languages,
                window,
                cx,
            )
        });
        let open_elapsed = open_started.elapsed();
        let cx = &mut VisualTestContext::from_window(*window, cx);
        let draw_started = Instant::now();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let draw_elapsed = draw_started.elapsed();

        let rendered = RENDERED_BLOCKS.load(Ordering::Relaxed);
        let markdowns = CREATED_MARKDOWNS.load(Ordering::Relaxed);
        let doc = window.root(cx).expect("NoteDoc raiz");
        let body_list = doc.read_with(cx, |doc, _| doc.body_list.clone());
        RENDERED_BLOCKS.store(0, Ordering::Relaxed);
        CREATED_MARKDOWNS.store(0, Ordering::Relaxed);
        body_list.scroll_to_reveal_item(900);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let scrolled_rendered = RENDERED_BLOCKS.load(Ordering::Relaxed);
        let scrolled_markdowns = CREATED_MARKDOWNS.load(Ordering::Relaxed);
        assert!(
            body_list.logical_scroll_top().item_ix > 800
                && scrolled_rendered < 200
                && scrolled_markdowns < 100,
            "salto distante parou em {:?}, materializou {scrolled_rendered} blocos e criou {scrolled_markdowns} previews",
            body_list.logical_scroll_top()
        );

        SERIALIZED_NOTES.store(0, Ordering::Relaxed);
        let editor = doc.read_with(cx, |doc, _| doc.blocks[0].editor.clone());
        for index in 0..5 {
            editor.update_in(cx, |editor, window, cx| {
                editor.set_text(format!("Edicao {index}"), window, cx);
            });
        }
        let serializations = SERIALIZED_NOTES.load(Ordering::Relaxed);
        assert!(
            rendered < 200 && markdowns < 100 && serializations == 0,
            "um frame materializou {rendered} blocos, criou {markdowns} parsers Markdown de 1.000 e cinco edits serializaram {serializations} vezes; esperado apenas o viewport e zero serializacoes antes do debounce"
        );

        cx.executor().advance_clock(SAVE_DEBOUNCE);
        cx.run_until_parked();
        assert_eq!(
            SERIALIZED_NOTES.load(Ordering::Relaxed),
            1,
            "a ultima edicao deve gerar exatamente um snapshot apos o debounce"
        );
        let snapshot_started = Instant::now();
        let snapshot_len = doc.read_with(cx, |doc, cx| doc.markdown(cx).len());
        let snapshot_elapsed = snapshot_started.elapsed();
        assert!(snapshot_len > 10_000);
        eprintln!(
            "large-note metrics: open={open_elapsed:?}, draw={draw_elapsed:?}, snapshot={snapshot_elapsed:?}, rendered={rendered}, markdowns={markdowns}, scrolled_rendered={scrolled_rendered}, scrolled_markdowns={scrolled_markdowns}, immediate_serializations={serializations}"
        );
    }
}
