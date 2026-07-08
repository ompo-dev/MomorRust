mod agent_notification;
mod hold_for_default;
mod mention_crease;
mod model_selector_components;
mod undo_reject_toast;

pub use agent_notification::*;
pub use hold_for_default::*;
pub use mention_crease::*;
pub use model_selector_components::*;
pub use undo_reject_toast::*;

/// Returns the appropriate [`DocumentationSide`] for documentation asides
/// in the agent panel, based on the current dock position.
pub fn documentation_aside_side(_cx: &gpui::App) -> ui::DocumentationSide {
    // ponytail: chat-only — o painel É a janela; aside pro lado "de fora" do dock
    // era cortado na borda. Os menus com aside ficam à direita, então abre à esquerda.
    ui::DocumentationSide::Left
}
