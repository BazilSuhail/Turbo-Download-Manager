//! Main menu bar.
//!
//! Commands are a plain enum rather than numeric ids, so the shell's `match`
//! is checked for exhaustiveness and a renamed command cannot drift.

/// Everything the menu bar and toolbar can ask the shell to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cmd {
    AddUrl,
    BatchAdd,
    PasteUrl,
    ImportList,
    ExportList,
    Exit,
    Resume,
    Pause,
    /// Requeue every failed download, clearing its retry count.
    RetryFailed,
    Delete,
    /// Enter select mode and mark every visible download.
    SelectMode,
    /// Open the confirmation for every marked download.
    DeleteSelected,
    OpenFile,
    ShowInFolder,
    VerifySha256,
    StartQueue,
    StopQueue,
    PauseAll,
    DeleteCompleted,
    Scheduler,
    Options,
    ChangeDownloadFolder,
    OpenDownloadFolder,
    ToggleSidebar,
    ToggleTheme,
    Find,
    UserGuide,
    CheckUpdates,
    About,
}

impl Cmd {
    /// Commands this build cannot perform yet. They are drawn greyed rather
    /// than accepting a click that does nothing.
    pub fn implemented(self) -> bool {
        !matches!(self, Cmd::ImportList | Cmd::ExportList | Cmd::UserGuide | Cmd::CheckUpdates)
    }
}

/// One entry in a drop-down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    Command { label: &'static str, cmd: Cmd, accel: Option<&'static str> },
    Separator,
}

/// A top-level menu and its entries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Menu {
    pub title: &'static str,
    pub items: &'static [Item],
}

const fn cmd(label: &'static str, cmd: Cmd) -> Item {
    Item::Command { label, cmd, accel: None }
}

const fn key(label: &'static str, cmd: Cmd, accel: &'static str) -> Item {
    Item::Command { label, cmd, accel: Some(accel) }
}

pub const MENU_BAR: &[Menu] = &[
    Menu {
        title: "File",
        items: &[
            key("New download", Cmd::AddUrl, "Ctrl+N"),
            cmd("Add a batch of links\u{2026}", Cmd::BatchAdd),
            key("Paste link from clipboard", Cmd::PasteUrl, "Ctrl+Shift+V"),
            Item::Separator,
            cmd("Import list\u{2026}", Cmd::ImportList),
            cmd("Export list\u{2026}", Cmd::ExportList),
            Item::Separator,
            cmd("Quit", Cmd::Exit),
        ],
    },
    Menu {
        title: "Downloads",
        items: &[
            key("Resume", Cmd::Resume, "Space"),
            cmd("Pause", Cmd::Pause),
            key("Retry failed downloads", Cmd::RetryFailed, "Ctrl+R"),
            key("Select downloads", Cmd::SelectMode, "Ctrl+A"),
            key("Delete\u{2026}", Cmd::Delete, "Del"),
            cmd("Delete selected\u{2026}", Cmd::DeleteSelected),
            Item::Separator,
            cmd("Open file", Cmd::OpenFile),
            key("Show in folder", Cmd::ShowInFolder, "F2"),
            cmd("Verify SHA-256\u{2026}", Cmd::VerifySha256),
            Item::Separator,
            cmd("Start queue", Cmd::StartQueue),
            cmd("Stop queue", Cmd::StopQueue),
            key("Pause everything", Cmd::PauseAll, "Ctrl+P"),
            cmd("Clear finished", Cmd::DeleteCompleted),
            Item::Separator,
            cmd("Schedule\u{2026}", Cmd::Scheduler),
            cmd("Preferences\u{2026}", Cmd::Options),
        ],
    },
    Menu {
        title: "Folder",
        items: &[
            cmd("Change default download folder\u{2026}", Cmd::ChangeDownloadFolder),
            cmd("Open download folder", Cmd::OpenDownloadFolder),
        ],
    },
    Menu {
        title: "View",
        items: &[
            cmd("Show or hide sidebar", Cmd::ToggleSidebar),
            cmd("Switch light / dark", Cmd::ToggleTheme),
            key("Search downloads", Cmd::Find, "Ctrl+F"),
        ],
    },
    Menu {
        title: "Help",
        items: &[
            cmd("User guide", Cmd::UserGuide),
            cmd("Check for updates\u{2026}", Cmd::CheckUpdates),
            Item::Separator,
            cmd("About TDM", Cmd::About),
        ],
    },
];

/// True when some menu lists this command.
pub fn contains(cmd: Cmd) -> bool {
    MENU_BAR
        .iter()
        .flat_map(|m| m.items)
        .any(|i| matches!(i, Item::Command { cmd: c, .. } if *c == cmd))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_level_menus_are_in_order() {
        let titles: Vec<_> = MENU_BAR.iter().map(|m| m.title).collect();
        assert_eq!(titles, ["File", "Downloads", "Folder", "View", "Help"]);
    }

    #[test]
    fn no_command_is_listed_twice() {
        let mut seen = std::collections::HashSet::new();
        for item in MENU_BAR.iter().flat_map(|m| m.items) {
            if let Item::Command { cmd, label, .. } = item {
                assert!(seen.insert(*cmd), "{label:?} repeats {cmd:?}");
            }
        }
    }

    #[test]
    fn no_menu_starts_or_ends_with_a_separator() {
        for m in MENU_BAR {
            assert_ne!(m.items.first(), Some(&Item::Separator), "{}", m.title);
            assert_ne!(m.items.last(), Some(&Item::Separator), "{}", m.title);
        }
    }
}
