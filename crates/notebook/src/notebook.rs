//! Notebook: workspace "Notion" do app — pastas, notas (markdown) e reuniões
//! (transcrições + resumo por IA), num painel lateral alternável.

mod note_editor;
mod panel;
mod store;

pub use panel::{DraggedNotebookItem, NotebookPanel, ToggleFocus, drop_text, init, refresh_panel};
pub use store::{Folder, Meeting, Note, NotebookDb};
