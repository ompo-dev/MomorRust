//! Persistência do notebook (pastas, notas, reuniões) num SQLite próprio via sqlez.
//! ponytail: modelo enxuto inspirado no momor — pastas em árvore, notas markdown e
//! reuniões (transcrição + resumo).

use db::{query, sqlez::domain::Domain, sqlez_macros::sql};
use sqlez::thread_safe_connection::ThreadSafeConnection;

#[derive(Clone, Debug)]
pub struct Folder {
    pub id: String,
    pub parent_id: Option<String>,
    pub name: String,
    pub sort_order: i64,
}

#[derive(Clone, Debug)]
pub struct Note {
    pub id: String,
    pub folder_id: Option<String>,
    pub title: String,
    pub content: String,
    pub sort_order: i64,
    /// Emoji ou caminho de imagem (chrome estilo Notion). Vazio = sem ícone.
    pub icon: Option<String>,
    /// Identificador do cover (ex.: "gradient-1") ou caminho. Vazio = sem cover.
    pub cover: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Meeting {
    pub id: String,
    pub folder_id: Option<String>,
    pub title: String,
    pub date: String,
    pub transcript: String,
    pub summary: String,
    /// Corpo livre (block editor, mesmo formato markdown das notas).
    pub content: String,
    /// "Uso": conversa completa com a IA do chat (formato "Falante: fala" por linha).
    pub uso: String,
    /// Chrome estilo Notion (como as notas): emoji/caminho de imagem e cover.
    pub icon: Option<String>,
    pub cover: Option<String>,
}

pub struct NotebookDb(ThreadSafeConnection);

impl Domain for NotebookDb {
    const NAME: &str = stringify!(NotebookDb);

    const MIGRATIONS: &[&str] = &[
        sql!(
            CREATE TABLE folders(
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                name TEXT NOT NULL,
                sort_order INTEGER NOT NULL DEFAULT 0
            ) STRICT;

            CREATE TABLE notes(
                id TEXT PRIMARY KEY,
                folder_id TEXT,
                title TEXT NOT NULL,
                content TEXT NOT NULL,
                sort_order INTEGER NOT NULL DEFAULT 0
            ) STRICT;

            CREATE TABLE meetings(
                id TEXT PRIMARY KEY,
                folder_id TEXT,
                title TEXT NOT NULL,
                date TEXT NOT NULL,
                transcript TEXT NOT NULL,
                summary TEXT NOT NULL,
                sort_order INTEGER NOT NULL DEFAULT 0
            ) STRICT;
        ),
        // Chrome estilo Notion (colunas anuláveis — sem DEFAULT por causa do sql!).
        sql!(
            ALTER TABLE notes ADD COLUMN icon TEXT;
            ALTER TABLE notes ADD COLUMN cover TEXT;
        ),
        // Reunião = nota completa: corpo em blocos + "uso" (conversa com a IA do chat).
        sql!(
            ALTER TABLE meetings ADD COLUMN content TEXT;
            ALTER TABLE meetings ADD COLUMN uso TEXT;
        ),
        // Reuniões também têm chrome (ícone/capa) como as notas.
        sql!(
            ALTER TABLE meetings ADD COLUMN icon TEXT;
            ALTER TABLE meetings ADD COLUMN cover TEXT;
        ),
    ];
}

db::static_connection!(NotebookDb, []);

impl NotebookDb {
    query! {
        pub fn folders() -> Result<Vec<(String, Option<String>, String, i64)>> {
            SELECT id, parent_id, name, sort_order FROM folders ORDER BY sort_order, name
        }
    }

    query! {
        pub fn notes() -> Result<Vec<(String, Option<String>, String, String, i64, Option<String>, Option<String>)>> {
            SELECT id, folder_id, title, content, sort_order, icon, cover FROM notes ORDER BY sort_order, title
        }
    }

    query! {
        pub fn meetings() -> Result<Vec<(String, Option<String>, String, String, String, String, Option<String>, Option<String>, Option<String>, Option<String>)>> {
            SELECT id, folder_id, title, date, transcript, summary, content, uso, icon, cover
            FROM meetings ORDER BY date DESC
        }
    }

    query! {
        pub async fn insert_folder(id: String, parent_id: Option<String>, name: String) -> Result<()> {
            INSERT INTO folders (id, parent_id, name) VALUES (?, ?, ?)
        }
    }

    query! {
        pub async fn insert_note(id: String, folder_id: Option<String>, title: String, content: String) -> Result<()> {
            INSERT INTO notes (id, folder_id, title, content) VALUES (?, ?, ?, ?)
        }
    }

    query! {
        pub async fn update_note(id: String, title: String, content: String) -> Result<()> {
            UPDATE notes SET title = ?2, content = ?3 WHERE id = ?1
        }
    }

    query! {
        pub async fn update_note_chrome(id: String, icon: Option<String>, cover: Option<String>) -> Result<()> {
            UPDATE notes SET icon = ?2, cover = ?3 WHERE id = ?1
        }
    }

    query! {
        pub async fn update_meeting_chrome(id: String, icon: Option<String>, cover: Option<String>) -> Result<()> {
            UPDATE meetings SET icon = ?2, cover = ?3 WHERE id = ?1
        }
    }

    query! {
        pub async fn insert_meeting(
            id: String,
            folder_id: Option<String>,
            title: String,
            date: String,
            transcript: String,
            summary: String
        ) -> Result<()> {
            INSERT INTO meetings (id, folder_id, title, date, transcript, summary)
            VALUES (?, ?, ?, ?, ?, ?)
        }
    }

    query! {
        pub async fn update_meeting_summary(id: String, title: String, summary: String) -> Result<()> {
            UPDATE meetings SET title = ?2, summary = ?3 WHERE id = ?1
        }
    }

    query! {
        pub async fn update_meeting_content(id: String, title: String, content: String) -> Result<()> {
            UPDATE meetings SET title = ?2, content = ?3 WHERE id = ?1
        }
    }

    query! {
        pub async fn rename_folder(id: String, name: String) -> Result<()> {
            UPDATE folders SET name = ?2 WHERE id = ?1
        }
    }

    query! {
        pub async fn set_note_folder(id: String, folder_id: Option<String>) -> Result<()> {
            UPDATE notes SET folder_id = ?2 WHERE id = ?1
        }
    }

    query! {
        pub async fn set_meeting_folder(id: String, folder_id: Option<String>) -> Result<()> {
            UPDATE meetings SET folder_id = ?2 WHERE id = ?1
        }
    }

    query! {
        pub async fn set_folder_parent(id: String, parent_id: Option<String>) -> Result<()> {
            UPDATE folders SET parent_id = ?2 WHERE id = ?1
        }
    }

    query! {
        pub async fn delete_folder(id: String) -> Result<()> {
            DELETE FROM folders WHERE id = ?
        }
    }

    query! {
        pub async fn delete_note(id: String) -> Result<()> {
            DELETE FROM notes WHERE id = ?
        }
    }

    query! {
        pub async fn delete_meeting(id: String) -> Result<()> {
            DELETE FROM meetings WHERE id = ?
        }
    }

    pub fn all_folders(&self) -> Vec<Folder> {
        self.folders()
            .unwrap_or_default()
            .into_iter()
            .map(|(id, parent_id, name, sort_order)| Folder {
                id,
                parent_id,
                name,
                sort_order,
            })
            .collect()
    }

    pub fn all_notes(&self) -> Vec<Note> {
        self.notes()
            .unwrap_or_default()
            .into_iter()
            .map(
                |(id, folder_id, title, content, sort_order, icon, cover)| Note {
                    id,
                    folder_id,
                    title,
                    content,
                    sort_order,
                    icon,
                    cover,
                },
            )
            .collect()
    }

    pub fn all_meetings(&self) -> Vec<Meeting> {
        self.meetings()
            .unwrap_or_default()
            .into_iter()
            .map(
                |(id, folder_id, title, date, transcript, summary, content, uso, icon, cover)| {
                    Meeting {
                        id,
                        folder_id,
                        title,
                        date,
                        transcript,
                        summary,
                        content: content.unwrap_or_default(),
                        uso: uso.unwrap_or_default(),
                        icon,
                        cover,
                    }
                },
            )
            .collect()
    }
}
