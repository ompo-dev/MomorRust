// ponytail: chat-only — menus mínimos (sem projeto, collab, debugger, feedback, welcome)
use gpui::{App, Menu, MenuItem, OsAction};
use terminal_view::terminal_panel;

pub fn app_menus(_cx: &mut App) -> Vec<Menu> {
    use momor_actions::Quit;

    vec![
        Menu {
            name: "Momor".into(),
            disabled: false,
            items: vec![
                MenuItem::action("About Momor", momor_actions::About),
                MenuItem::separator(),
                MenuItem::submenu(Menu::new("Settings").items([
                    MenuItem::action("Open Settings", momor_actions::OpenSettings),
                    MenuItem::action("Open Settings File", super::OpenSettingsFile),
                    MenuItem::action("Open Default Settings", super::OpenDefaultSettings),
                    MenuItem::separator(),
                    MenuItem::action("Open Keymap", momor_actions::OpenKeymap),
                    MenuItem::action("Open Keymap File", momor_actions::OpenKeymapFile),
                    MenuItem::action("Open Default Key Bindings", momor_actions::OpenDefaultKeymap),
                    MenuItem::separator(),
                    MenuItem::action(
                        "Select Theme...",
                        momor_actions::theme_selector::Toggle::default(),
                    ),
                    MenuItem::action(
                        "Select Icon Theme...",
                        momor_actions::icon_theme_selector::Toggle::default(),
                    ),
                ])),
                MenuItem::separator(),
                #[cfg(target_os = "macos")]
                MenuItem::os_submenu("Services", gpui::SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Extensions", momor_actions::Extensions::default()),
                MenuItem::separator(),
                #[cfg(target_os = "macos")]
                MenuItem::action("Hide Momor", super::Hide),
                #[cfg(target_os = "macos")]
                MenuItem::action("Hide Others", super::HideOthers),
                #[cfg(target_os = "macos")]
                MenuItem::action("Show All", super::ShowAll),
                MenuItem::separator(),
                MenuItem::action("Quit Momor", Quit),
            ],
        },
        Menu {
            name: "Edit".into(),
            disabled: false,
            items: vec![
                MenuItem::os_action("Undo", editor::actions::Undo, OsAction::Undo),
                MenuItem::os_action("Redo", editor::actions::Redo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action("Cut", editor::actions::Cut, OsAction::Cut),
                MenuItem::os_action("Copy", editor::actions::Copy, OsAction::Copy),
                MenuItem::os_action("Paste", editor::actions::Paste, OsAction::Paste),
                MenuItem::separator(),
                MenuItem::os_action(
                    "Select All",
                    editor::actions::SelectAll,
                    OsAction::SelectAll,
                ),
                MenuItem::separator(),
                MenuItem::action("Find", search::buffer_search::Deploy::find()),
            ],
        },
        Menu {
            name: "View".into(),
            disabled: false,
            items: vec![
                MenuItem::action(
                    "Zoom In",
                    momor_actions::IncreaseBufferFontSize { persist: false },
                ),
                MenuItem::action(
                    "Zoom Out",
                    momor_actions::DecreaseBufferFontSize { persist: false },
                ),
                MenuItem::action(
                    "Reset Zoom",
                    momor_actions::ResetBufferFontSize { persist: false },
                ),
                MenuItem::separator(),
                MenuItem::action("Command Palette...", momor_actions::command_palette::Toggle),
                MenuItem::separator(),
                MenuItem::action("Terminal Panel", terminal_panel::ToggleFocus),
            ],
        },
        Menu {
            name: "Window".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Minimize", super::Minimize),
                MenuItem::action("Zoom", super::Zoom),
                MenuItem::separator(),
            ],
        },
        Menu {
            name: "Help".into(),
            disabled: false,
            items: vec![
                MenuItem::action("View Telemetry", momor_actions::OpenTelemetryLog),
                MenuItem::action("View Dependency Licenses", momor_actions::OpenLicenses),
            ],
        },
    ]
}
