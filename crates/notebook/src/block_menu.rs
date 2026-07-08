//! Menu `/` de blocos estilo Notion. Digitar `/` num bloco lista os tipos (ícone +
//! nome + descrição); escolher muda o tipo do bloco atual (via `NoteDoc::set_block_kind`).

use std::rc::Rc;
use std::sync::Arc;

use anyhow::Result;
use editor::{CompletionContext, CompletionProvider, Editor};
use gpui::{Context, Entity, Task, WeakEntity, Window};
use language::{Buffer, CodeLabel};
use project::lsp_store::CompletionDocumentation;
use project::{
    Completion, CompletionDisplayOptions, CompletionIntent, CompletionResponse, CompletionSource,
};
use rope::Point;
use text::ToPoint as _;
use ui::IconName;

use crate::note_editor::{BlockKind, NoteDoc};

/// (rótulo, aliases pra busca, descrição, ícone, tipo).
const BLOCKS: &[(&str, &str, &str, IconName, BlockKind)] = &[
    (
        "Texto",
        "text paragrafo paragraph",
        "Comece a escrever texto simples",
        IconName::Menu,
        BlockKind::Paragraph,
    ),
    (
        "Título 1",
        "h1 titulo title heading",
        "Título de seção grande",
        IconName::Hash,
        BlockKind::H1,
    ),
    (
        "Título 2",
        "h2 titulo title heading",
        "Título de seção média",
        IconName::Hash,
        BlockKind::H2,
    ),
    (
        "Título 3",
        "h3 titulo title heading",
        "Título de subseção",
        IconName::Hash,
        BlockKind::H3,
    ),
    (
        "Lista com marcadores",
        "bullet lista marcador ul unordered",
        "Lista simples com marcadores",
        IconName::ListTree,
        BlockKind::Bullet,
    ),
    (
        "Lista numerada",
        "numbered lista numerada ol numero ordered",
        "Lista ordenada por números",
        IconName::ListTree,
        BlockKind::Numbered,
    ),
    (
        "Tarefa",
        "todo task tarefa checkbox caixa check",
        "Lista com caixas de seleção",
        IconName::ListTodo,
        BlockKind::Todo(false),
    ),
    (
        "Citação",
        "quote citacao blockquote",
        "Destaque uma citação",
        IconName::Quote,
        BlockKind::Quote,
    ),
    (
        "Código",
        "code codigo bloco",
        "Bloco de código",
        IconName::Code,
        BlockKind::Code,
    ),
];

pub fn install(editor: &mut Editor, doc: WeakEntity<NoteDoc>, block_id: usize) {
    editor.set_completion_provider(Some(Rc::new(BlockMenu { doc, block_id })));
}

struct BlockMenu {
    doc: WeakEntity<NoteDoc>,
    block_id: usize,
}

impl CompletionProvider for BlockMenu {
    fn completions(
        &self,
        buffer: &Entity<Buffer>,
        buffer_position: text::Anchor,
        _trigger: CompletionContext,
        _window: &mut Window,
        cx: &mut Context<Editor>,
    ) -> Task<Result<Vec<CompletionResponse>>> {
        let (offset_to_line, position, line) = buffer.update(cx, |buffer, _| {
            let position = buffer_position.to_point(buffer);
            let line_start = Point::new(position.row, 0);
            let offset_to_line = buffer.point_to_offset(line_start);
            let line: String = buffer.text_for_range(line_start..position).collect();
            (offset_to_line, position, line)
        });

        let Some(slash_rel) = line.rfind('/') else {
            return Task::ready(Ok(Vec::new()));
        };
        let preceded_ok = line[..slash_rel]
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace());
        if !preceded_ok {
            return Task::ready(Ok(Vec::new()));
        }
        let query = line[slash_rel + 1..].to_lowercase();

        let snapshot = buffer.read(cx).snapshot();
        let cursor_offset = snapshot.point_to_offset(position);
        let slash_offset = offset_to_line + slash_rel;
        let replace_range =
            snapshot.anchor_before(slash_offset)..snapshot.anchor_after(cursor_offset);

        let doc = self.doc.clone();
        let block_id = self.block_id;

        let completions: Vec<Completion> = BLOCKS
            .iter()
            .filter(|(label, alias, _, _, _)| {
                query.is_empty()
                    || label.to_lowercase().contains(&query)
                    || alias.contains(query.as_str())
            })
            .map(|(label, _, description, icon, kind)| {
                let doc = doc.clone();
                let kind = *kind;
                Completion {
                    replace_range: replace_range.clone(),
                    new_text: String::new(),
                    label: CodeLabel::plain(label.to_string(), None),
                    documentation: Some(CompletionDocumentation::SingleLine(
                        (*description).into(),
                    )),
                    source: CompletionSource::Custom,
                    icon_path: Some(icon.path().into()),
                    match_start: None,
                    snippet_deduplication_key: None,
                    insert_text_mode: None,
                    // Deferido: o confirm roda DENTRO do update do editor do bloco; mudar o
                    // tipo (que dá outro update no mesmo editor) na hora = double-lease/crash.
                    confirm: Some(Arc::new(move |_intent: CompletionIntent, _window, cx| {
                        let doc = doc.clone();
                        cx.defer(move |cx| {
                            doc.update(cx, |doc, cx| doc.set_block_kind(block_id, kind, cx))
                                .ok();
                        });
                        false
                    })),
                }
            })
            .collect();

        Task::ready(Ok(vec![CompletionResponse {
            completions,
            display_options: CompletionDisplayOptions::default(),
            is_incomplete: false,
        }]))
    }

    fn is_completion_trigger(
        &self,
        _buffer: &Entity<Buffer>,
        _position: language::Anchor,
        text: &str,
        _trigger_in_words: bool,
        _cx: &mut Context<Editor>,
    ) -> bool {
        text == "/"
    }

    fn sort_completions(&self) -> bool {
        false
    }

    fn filter_completions(&self) -> bool {
        false
    }
}
