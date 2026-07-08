//! Skills estilo Claude Code / openclaude: cada skill é uma pasta em
//! `~/.claude/skills/<nome>/SKILL.md` com frontmatter (`name`, `description`) e
//! um corpo de instruções. A lista (nome + descrição) vai pro system prompt e a
//! tool `skill` carrega o corpo sob demanda — mesmo padrão do Claude Code.

use std::path::PathBuf;
use std::sync::OnceLock;

use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// Caminho do SKILL.md — o corpo é lido sob demanda pela tool.
    #[serde(skip)]
    pub path: PathBuf,
}

/// Raiz das skills do usuário. ponytail: só o global por ora; adicionar
/// `<worktree>/.claude/skills` quando um projeto precisar de skills locais.
fn skills_root() -> PathBuf {
    paths::home_dir().join(".claude").join("skills")
}

/// Skills descobertas (memoizadas no processo). ponytail: varre uma vez; skills
/// novas só aparecem ao reiniciar. Upgrade: invalidar via watcher se incomodar.
pub fn all() -> &'static [Skill] {
    static SKILLS: OnceLock<Vec<Skill>> = OnceLock::new();
    SKILLS.get_or_init(discover)
}

fn discover() -> Vec<Skill> {
    let Ok(entries) = std::fs::read_dir(skills_root()) else {
        return Vec::new();
    };
    let mut skills: Vec<Skill> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| {
            let md = e.path().join("SKILL.md");
            let text = std::fs::read_to_string(&md).ok()?;
            let (name, description) = parse_frontmatter(&text)?;
            Some(Skill {
                name,
                description,
                path: md,
            })
        })
        .collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// Corpo da skill (sem frontmatter), lido fresco do disco pra pegar edições.
pub fn load_body(name: &str) -> Option<String> {
    let skill = all().iter().find(|s| s.name == name)?;
    let text = std::fs::read_to_string(&skill.path).ok()?;
    Some(strip_frontmatter(&text).trim().to_string())
}

/// ponytail: parser mínimo de frontmatter — só `name`/`description` de uma linha.
/// Descrições multi-linha (YAML `|`/`>`) não são suportadas; nenhuma skill aqui usa.
fn parse_frontmatter(text: &str) -> Option<(String, String)> {
    let rest = text.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    let (mut name, mut description) = (None, None);
    for line in rest[..end].lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("name:") {
            name = Some(unquote(v));
        } else if let Some(v) = line.strip_prefix("description:") {
            description = Some(unquote(v));
        }
    }
    Some((name?, description?))
}

fn strip_frontmatter(text: &str) -> &str {
    text.strip_prefix("---")
        .and_then(|rest| rest.find("\n---").map(|end| &rest[end + 4..]))
        .unwrap_or(text)
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches(['"', '\'']).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_name_and_description() {
        let md = "---\nname: pdf\ndescription: \"Work with PDFs\"\nother: x\n---\n\n# Body\ndo things\n";
        let (name, desc) = parse_frontmatter(md).unwrap();
        assert_eq!(name, "pdf");
        assert_eq!(desc, "Work with PDFs");
        assert_eq!(strip_frontmatter(md).trim(), "# Body\ndo things");
    }

    #[test]
    fn no_frontmatter_returns_none() {
        assert!(parse_frontmatter("# just markdown").is_none());
    }
}
