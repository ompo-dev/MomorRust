use std::sync::Arc;

use agent_client_protocol::schema as acp;
use gpui::{App, SharedString, Task};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AgentTool, ToolCallEventStream, ToolInput};

/// Read and write the user's notebook (the "Workspace" side panel): folders,
/// notes (markdown) and meetings (transcripts). Use this to create folders and
/// notes for the user, to list what already exists, or to read/rewrite a note.
///
/// This is the ONLY way to put content in the notebook — it is NOT the
/// filesystem, so do not use file tools for notes. Typical flow: call `list`
/// first to get folder/note ids, then `create_note` / `update_note`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct NotebookToolInput {
    /// What to do. One of:
    /// - `list`: list every folder, note and meeting with their ids.
    /// - `read`: return the full content of a note or meeting (needs `id`).
    /// - `create_folder`: create a folder (needs `name`; optional `folder_id` as parent).
    /// - `create_note`: create a markdown note (needs `name` as the title; optional
    ///   `content` and `folder_id` to place it inside a folder).
    /// - `update_note`: overwrite a note's markdown (needs `id` and `content`).
    pub action: String,
    /// Folder name or note title, for `create_folder` / `create_note`.
    #[serde(default)]
    pub name: Option<String>,
    /// Markdown body, for `create_note` / `update_note`.
    #[serde(default)]
    pub content: Option<String>,
    /// Parent folder id (from `list`), for `create_folder` / `create_note`. Omit for the root.
    #[serde(default)]
    pub folder_id: Option<String>,
    /// Target note or meeting id (from `list`), for `read` / `update_note`.
    #[serde(default)]
    pub id: Option<String>,
}

pub struct NotebookTool;

impl AgentTool for NotebookTool {
    type Input = NotebookToolInput;
    type Output = String;

    const NAME: &'static str = "notebook";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Other
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(input) => match input.action.as_str() {
                "create_folder" => "Create notebook folder".into(),
                "create_note" => "Create note".into(),
                "update_note" => "Update note".into(),
                "read" => "Read note".into(),
                _ => "List notebook".into(),
            },
            Err(_) => "Notebook".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        _event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<String, String>> {
        cx.spawn(async move |cx| {
            let input = input.recv().await.map_err(|e| e.to_string())?;
            let db = cx.update(|cx| notebook::NotebookDb::global(cx));

            let result = match input.action.trim() {
                "list" => list(&db),
                "read" => read(&db, req(input.id, "id")?),
                "create_folder" => {
                    let id = uuid::Uuid::new_v4().to_string();
                    db.insert_folder(id.clone(), input.folder_id, req(input.name, "name")?)
                        .await
                        .map_err(|e| e.to_string())?;
                    Ok(format!("Created folder [folder:{id}]."))
                }
                "create_note" => {
                    let id = uuid::Uuid::new_v4().to_string();
                    db.insert_note(
                        id.clone(),
                        input.folder_id,
                        req(input.name, "name")?,
                        input.content.unwrap_or_default(),
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                    Ok(format!("Created note [note:{id}]."))
                }
                "update_note" => {
                    let id = req(input.id, "id")?;
                    let content = req(input.content, "content")?;
                    // Título = primeira linha (igual ao editor do painel).
                    let title = content
                        .lines()
                        .next()
                        .map(|l| l.trim().trim_start_matches('#').trim().to_string())
                        .filter(|t| !t.is_empty())
                        .unwrap_or_else(|| "Nova nota".into());
                    db.update_note(id.clone(), title, content)
                        .await
                        .map_err(|e| e.to_string())?;
                    Ok(format!("Updated note [note:{id}]."))
                }
                other => Err(format!(
                    "Unknown action `{other}`. Valid: list, read, create_folder, create_note, update_note."
                )),
            };

            // Reflete no painel na hora (a IA acabou de mudar o notebook).
            cx.update(|cx| notebook::refresh_panel(cx));
            result
        })
    }
}

/// Campo obrigatório ausente → erro claro pro modelo (não silencioso).
fn req(value: Option<String>, field: &str) -> Result<String, String> {
    value
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| format!("Missing required field `{field}` for this action."))
}

fn list(db: &notebook::NotebookDb) -> Result<String, String> {
    let folders = db.all_folders();
    let notes = db.all_notes();
    let meetings = db.all_meetings();

    let mut out = String::new();
    out.push_str("Folders:\n");
    if folders.is_empty() {
        out.push_str("  (none)\n");
    }
    for f in &folders {
        let parent = f
            .parent_id
            .as_deref()
            .map(|p| format!(" (in [folder:{p}])"))
            .unwrap_or_default();
        out.push_str(&format!("  [folder:{}] {}{}\n", f.id, f.name, parent));
    }

    out.push_str("Notes:\n");
    if notes.is_empty() {
        out.push_str("  (none)\n");
    }
    for n in &notes {
        let folder = n
            .folder_id
            .as_deref()
            .map(|p| format!(" (in [folder:{p}])"))
            .unwrap_or_default();
        out.push_str(&format!("  [note:{}] {}{}\n", n.id, n.title, folder));
    }

    out.push_str("Meetings:\n");
    if meetings.is_empty() {
        out.push_str("  (none)\n");
    }
    for m in &meetings {
        out.push_str(&format!("  [meeting:{}] {} — {}\n", m.id, m.title, m.date));
    }

    Ok(out)
}

fn read(db: &notebook::NotebookDb, id: String) -> Result<String, String> {
    if let Some(note) = db.all_notes().into_iter().find(|n| n.id == id) {
        return Ok(format!("# {}\n\n{}", note.title, note.content));
    }
    if let Some(m) = db.all_meetings().into_iter().find(|m| m.id == id) {
        let summary = if m.summary.trim().is_empty() {
            String::new()
        } else {
            format!("## Resumo\n{}\n\n", m.summary)
        };
        return Ok(format!(
            "# {} ({})\n\n{}## Transcrição\n{}",
            m.title, m.date, summary, m.transcript
        ));
    }
    Err(format!("No note or meeting found with id `{id}`."))
}
