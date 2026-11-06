//! Command palette data: the command list, the filter, and the keyboard help.
//!
//! Pure data and logic. No iced types live here, so the UI layer maps each
//! `Command::id` to a `Cmd` or `Message` in one place.

/// One entry in the command palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Command {
    /// Stable identifier. Never rename once shipped; the shell matches on it.
    pub id: &'static str,
    /// Text shown in the palette.
    pub label: &'static str,
    /// Keyboard shortcut shown beside the label, if one exists.
    pub shortcut: Option<&'static str>,
}

const COMMANDS: &[Command] = &[
    Command { id: "add_url", label: "New download", shortcut: Some("Ctrl+N") },
    Command { id: "add_batch", label: "Add a batch of links\u{2026}", shortcut: None },
    Command { id: "paste_link", label: "Paste link from clipboard", shortcut: Some("Ctrl+Shift+V") },
    Command { id: "grab_site", label: "Grab site\u{2026}", shortcut: None },
    Command { id: "open_torrent_file", label: "Open torrent file\u{2026}", shortcut: None },
    Command { id: "pause_all", label: "Pause everything", shortcut: Some("Ctrl+P") },
    Command { id: "start_queue", label: "Start queue", shortcut: None },
    Command { id: "stop_queue", label: "Stop queue", shortcut: None },
    Command { id: "resume_selected", label: "Resume", shortcut: Some("Space") },
    Command { id: "pause_selected", label: "Pause", shortcut: None },
    Command { id: "retry_failed", label: "Retry failed downloads", shortcut: Some("Ctrl+R") },
    Command { id: "select_all", label: "Select downloads", shortcut: Some("Ctrl+A") },
    Command { id: "remove_selected", label: "Delete download\u{2026}", shortcut: Some("Del") },
    Command { id: "remove_marked", label: "Delete selected downloads\u{2026}", shortcut: None },
    Command { id: "open_file", label: "Open file", shortcut: None },
    Command { id: "show_in_folder", label: "Show in folder", shortcut: Some("F2") },
    Command { id: "verify_checksum", label: "Verify SHA-256\u{2026}", shortcut: None },
    Command { id: "copy_link", label: "Copy link", shortcut: None },
    Command { id: "open_folder", label: "Open download folder", shortcut: None },
    Command { id: "change_download_folder", label: "Change default download folder\u{2026}", shortcut: None },
    Command { id: "toggle_sidebar", label: "Show or hide sidebar", shortcut: None },
    Command { id: "toggle_theme", label: "Switch light / dark", shortcut: None },
    Command { id: "search_focus", label: "Search downloads", shortcut: Some("Ctrl+F") },
    Command { id: "options", label: "Preferences\u{2026}", shortcut: None },
    Command { id: "schedule", label: "Schedule\u{2026}", shortcut: None },
    Command { id: "export_list", label: "Export list\u{2026}", shortcut: None },
    Command { id: "import_list", label: "Import list\u{2026}", shortcut: None },
    Command { id: "clear_finished", label: "Clear finished", shortcut: None },
    Command { id: "command_palette", label: "Command palette\u{2026}", shortcut: Some("Ctrl+K") },
];

/// Every command the palette offers, in display order.
pub fn commands() -> Vec<Command> {
    COMMANDS.to_vec()
}

/// Indices into `cmds` whose label matches `query`, best match first.
///
/// The query is trimmed and matched case-insensitively as a subsequence of the
/// label. Ranking: labels that start with the query come first, then the
/// earlier the first matched character, then the shorter label. Ties keep the
/// original order. An empty query returns every index in original order.
pub fn filter(query: &str, cmds: &[Command]) -> Vec<usize> {
    let q: Vec<char> = query.trim().to_lowercase().chars().collect();
    if q.is_empty() {
        return (0..cmds.len()).collect();
    }
    // (not a prefix, match start, label length, index). Tuples sort field by field.
    let mut hits: Vec<(bool, usize, usize, usize)> = Vec::new();
    for (i, cmd) in cmds.iter().enumerate() {
        let label: Vec<char> = cmd.label.to_lowercase().chars().collect();
        if let Some(start) = subsequence_start(&label, &q) {
            hits.push((!label.starts_with(&q), start, label.len(), i));
        }
    }
    hits.sort();
    hits.into_iter().map(|h| h.3).collect()
}

/// Position of the first character of `query` matched in `label`, or `None`
/// when `query` is not a subsequence of `label`. `query` must be non-empty.
fn subsequence_start(label: &[char], query: &[char]) -> Option<usize> {
    let mut qi = 0;
    let mut start = None;
    for (i, ch) in label.iter().enumerate() {
        if qi < query.len() && *ch == query[qi] {
            if qi == 0 {
                start = Some(i);
            }
            qi += 1;
        }
    }
    if qi == query.len() { start } else { None }
}

/// Key -> action pairs for the Options keyboard section. Keys match the
/// `shortcut` values in `commands()` and the keys handled in the main window.
pub fn shortcut_help() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Ctrl+N", "New download"),
        ("Ctrl+Shift+V", "Paste link from clipboard"),
        ("Ctrl+F", "Search downloads"),
        ("Ctrl+K", "Command palette"),
        ("Ctrl+A", "Select downloads"),
        ("Ctrl+P", "Pause everything, or start the queue when nothing is running"),
        ("Ctrl+R", "Retry failed downloads"),
        ("Space / Enter", "Pause or resume the selected download; Enter opens a finished file"),
        ("Del", "Delete download"),
        ("F2", "Show in folder"),
        ("Up / Down, Home / End", "Move the selection"),
        ("Esc", "Close the topmost dialog, or leave select mode"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn cmd(id: &'static str, label: &'static str) -> Command {
        Command { id, label, shortcut: None }
    }

    #[test]
    fn prefix_match_ranks_first() {
        let cmds = [cmd("a", "Show the open tab"), cmd("b", "Open file")];
        // "Open file" starts with the query, so it beats the later mid-label match.
        assert_eq!(filter("open", &cmds), vec![1, 0]);
    }

    #[test]
    fn earlier_match_position_ranks_higher() {
        let cmds = [cmd("a", "zzzab"), cmd("b", "zab")];
        // Neither starts with "ab". The one matching earlier wins.
        assert_eq!(filter("ab", &cmds), vec![1, 0]);
    }

    #[test]
    fn shorter_label_wins_on_tie() {
        let cmds = [cmd("a", "abcdef"), cmd("b", "ab")];
        assert_eq!(filter("ab", &cmds), vec![1, 0]);
    }

    #[test]
    fn match_is_case_insensitive_and_trims() {
        let cmds = [cmd("a", "Toggle Theme")];
        assert_eq!(filter("  THEME  ", &cmds), vec![0]);
    }

    #[test]
    fn empty_query_returns_all_in_order() {
        let cmds = commands();
        let all: Vec<usize> = (0..cmds.len()).collect();
        assert_eq!(filter("", &cmds), all);
        assert_eq!(filter("   ", &cmds), all);
    }

    #[test]
    fn no_match_returns_empty() {
        let cmds = commands();
        assert!(filter("qqqzzz", &cmds).is_empty());
    }

    #[test]
    fn subsequence_must_keep_order() {
        let cmds = [cmd("a", "abc")];
        assert!(filter("cba", &cmds).is_empty());
        assert_eq!(filter("ac", &cmds), vec![0]);
    }

    #[test]
    fn command_ids_are_unique() {
        let mut seen = HashSet::new();
        for c in commands() {
            assert!(seen.insert(c.id), "duplicate id {}", c.id);
        }
    }

    #[test]
    fn shortcut_help_keys_exist_in_commands() {
        let shortcuts: HashSet<&str> = commands().iter().filter_map(|c| c.shortcut).collect();
        for (key, _) in shortcut_help() {
            if key.contains('+') && !key.contains(" / ") {
                assert!(shortcuts.contains(key), "{key} missing from commands");
            }
        }
    }
}
