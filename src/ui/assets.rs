//! Icons and window artwork.
//!
//! Every icon is a hand-written 24×24 SVG in `assets/icons/`, drawn in one
//! rounded-stroke style with a single flat colour. The shell recolours them
//! from the active palette, so one file serves both themes and every state.
//!
//! The window icon is drawn here in code: a small rasteriser is less to keep
//! in sync than a PNG plus a decoder, and it scales to any size.

use crate::ui::menu::Cmd;

macro_rules! icons {
    ($($name:ident => $file:literal),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Icon { $($name),* }

        impl Icon {
            pub const ALL: &'static [Icon] = &[$(Icon::$name),*];

            /// The SVG source, embedded at build time.
            pub fn svg(self) -> &'static [u8] {
                match self {
                    $(Icon::$name => include_bytes!(concat!("../../assets/icons/", $file, ".svg"))),*
                }
            }
        }
    };
}

icons! {
    Add => "add",
    Resume => "resume",
    Pause => "pause",
    StopAll => "stop_all",
    Delete => "delete",
    DeleteCompleted => "delete_completed",
    Options => "options",
    Scheduler => "scheduler",
    StartQueue => "start_queue",
    StopQueue => "stop_queue",
    Sun => "sun",
    Moon => "moon",
    Search => "search",
    All => "all",
    Programs => "programs",
    Music => "music",
    Video => "video",
    Documents => "documents",
    Compressed => "compressed",
    Unfinished => "unfinished",
    Finished => "finished",
    Folder => "folder",
    Open => "open",
    Check => "check",
    File => "file",
    Batch => "batch",
    More => "more",
    Close => "close",
    Shield => "shield",
}

/// A toolbar button: the command it runs, its icon and its tooltip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolbarIcon {
    pub cmd: Cmd,
    pub icon: Icon,
    pub label: &'static str,
}

pub const TOOLBAR: &[ToolbarIcon] = &[
    ToolbarIcon { cmd: Cmd::AddUrl, icon: Icon::Add, label: "New download" },
    ToolbarIcon { cmd: Cmd::Resume, icon: Icon::Resume, label: "Resume" },
    ToolbarIcon { cmd: Cmd::Pause, icon: Icon::Pause, label: "Pause" },
    ToolbarIcon { cmd: Cmd::PauseAll, icon: Icon::StopAll, label: "Pause everything" },
    ToolbarIcon { cmd: Cmd::Delete, icon: Icon::Delete, label: "Delete" },
    ToolbarIcon { cmd: Cmd::DeleteCompleted, icon: Icon::DeleteCompleted, label: "Clear finished" },
    ToolbarIcon { cmd: Cmd::StartQueue, icon: Icon::StartQueue, label: "Start queue" },
    ToolbarIcon { cmd: Cmd::StopQueue, icon: Icon::StopQueue, label: "Stop queue" },
    ToolbarIcon { cmd: Cmd::Scheduler, icon: Icon::Scheduler, label: "Schedule" },
    ToolbarIcon { cmd: Cmd::Options, icon: Icon::Options, label: "Preferences" },
];

/// The icon for a category id, matching `model::Category::defaults`.
/// User-made categories share the generic file icon.
pub fn category_icon(id: Option<u32>) -> Icon {
    match id {
        None => Icon::All,
        Some(1) => Icon::Programs,
        Some(2) => Icon::Music,
        Some(3) => Icon::Video,
        Some(5) => Icon::Documents,
        Some(7) => Icon::Compressed,
        Some(_) => Icon::File,
    }
}

/// The TDM logo as straight RGBA8, for the window icon (64 px) and the header
/// mark (32 px). Generated from `assets/tdm-logo.png` by `packaging/make_logo.py`.
const APP_ICON_64: &[u8] = include_bytes!("../../assets/app-icon-64.rgba");
const APP_ICON_32: &[u8] = include_bytes!("../../assets/app-icon-32.rgba");

/// The logo at `size` px, which must be 32 or 64.
pub fn app_icon(size: u32) -> &'static [u8] {
    match size {
        32 => APP_ICON_32,
        64 => APP_ICON_64,
        _ => panic!("no app icon at {size} px; ship one with packaging/make_logo.py"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_a_24px_stroke_svg() {
        for icon in Icon::ALL {
            let src = std::str::from_utf8(icon.svg()).expect("utf-8");
            assert!(src.starts_with("<svg"), "{icon:?}");
            assert!(src.contains(r#"viewBox="0 0 24 24""#), "{icon:?}");
            assert!(src.contains(r#"stroke-linecap="round""#), "{icon:?} breaks the style");
            assert!(src.trim_end().ends_with("</svg>"), "{icon:?} is truncated");
        }
    }

    #[test]
    fn no_two_icons_share_artwork() {
        for (i, a) in Icon::ALL.iter().enumerate() {
            for b in &Icon::ALL[i + 1..] {
                assert_ne!(a.svg(), b.svg(), "{a:?} and {b:?} are identical");
            }
        }
    }

    #[test]
    fn the_known_categories_get_distinct_icons() {
        let ids = [None, Some(1), Some(2), Some(3), Some(5), Some(7), Some(99)];
        for (i, a) in ids.iter().enumerate() {
            for b in &ids[i + 1..] {
                assert_ne!(category_icon(*a), category_icon(*b), "{a:?} and {b:?}");
            }
        }
    }

    #[test]
    fn toolbar_commands_are_distinct_and_in_the_menu() {
        let cmds: std::collections::HashSet<_> = TOOLBAR.iter().map(|t| t.cmd).collect();
        assert_eq!(cmds.len(), TOOLBAR.len());
        for t in TOOLBAR {
            assert!(crate::ui::menu::contains(t.cmd), "{} is not in the menu", t.label);
            assert!(t.cmd.implemented(), "{} is a dead toolbar button", t.label);
        }
    }

    #[test]
    fn the_app_icon_is_the_logo_at_each_shipped_size() {
        for size in [32u32, 64] {
            let px = app_icon(size);
            assert_eq!(px.len(), (size * size * 4) as usize, "{size} px");
            // The logo has a transparent margin, so a visible share is the right check.
            let visible = px.chunks(4).filter(|p| p[3] > 0).count();
            assert!(visible > (size * size / 4) as usize, "{size} px: only {visible} visible pixels");
        }
    }
}
