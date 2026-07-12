//! Notebook: workspace "Notion" do app — pastas, notas (markdown) e reuniões
//! (transcrições + resumo por IA), num painel lateral alternável.

mod note_editor;
mod panel;
mod rich_block;
mod store;

pub use panel::{
    DraggedNotebookItem, NotebookPanel, ToggleFocus, drop_badge_icon, drop_icon, drop_text, init,
    open_item, refresh_panel,
};
pub use store::{Folder, Meeting, Note, NotebookDb};
