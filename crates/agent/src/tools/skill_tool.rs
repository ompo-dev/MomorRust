use std::sync::Arc;

use agent_client_protocol::schema as acp;
use gpui::{App, SharedString, Task};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AgentTool, ToolCallEventStream, ToolInput};

/// Load a skill's full instructions on demand. A skill is a reusable capability
/// (its steps, scripts and references) that the user installed. First read the
/// "Available Skills" list in the system prompt; when a skill matches the task,
/// call this with its exact `name` to get the instructions, then follow them.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SkillToolInput {
    /// The exact `name` of the skill to load, from the Available Skills list.
    pub name: String,
}

pub struct SkillTool;

impl AgentTool for SkillTool {
    type Input = SkillToolInput;
    type Output = String;

    const NAME: &'static str = "skill";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Other
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(input) => format!("Skill: {}", input.name).into(),
            Err(_) => "Skill".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        _event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<String, String>> {
        cx.spawn(async move |_cx| {
            let input = input.recv().await.map_err(|e| e.to_string())?;
            match crate::skills::load_body(&input.name) {
                Some(body) => Ok(body),
                None => Err(format!(
                    "No skill named `{}`. Use one from the Available Skills list.",
                    input.name
                )),
            }
        })
    }
}
