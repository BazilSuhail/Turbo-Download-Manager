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

/// The application mark, `size`×`size` RGBA8: a bold orange T with three
/// forward chevrons on a charcoal rounded tile. Same geometry as
/// `packaging/gen_art.py`; supersampled 4×4 for smooth edges.
pub fn app_icon(size: u32) -> Vec<u8> {
    const SS: u32 = 4;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let mut sum = [0f32; 3];
            let mut hits = 0u32;
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = (px as f32 + (sx as f32 + 0.5) / SS as f32) / size as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / SS as f32) / size as f32;
                    if let Some(c) = sample(x, y) {
                        for (s, v) in sum.iter_mut().zip(c) {
                            *s += v;
                        }
                        hits += 1;
                    }
                }
            }
            if hits == 0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            for s in sum {
                out.push((s / hits as f32).round() as u8);
            }
            out.push((255 * hits / (SS * SS)) as u8);
        }
    }
    out
}

const TILE_TOP: [f32; 3] = [46.0, 40.0, 35.0];
const TILE_BOTTOM: [f32; 3] = [22.0, 19.0, 17.0];
const MARK_TOP: [f32; 3] = [255.0, 184.0, 77.0];
const MARK_BOTTOM: [f32; 3] = [255.0, 106.0, 26.0];
const CHEVRON_TIPS: [f32; 3] = [0.70, 0.79, 0.88];
const CHEVRON_ARM: f32 = 0.10;
const CHEVRON_RISE: f32 = 0.17;
const CHEVRON_HALF: f32 = 0.034;

/// Colour at a unit-square point (y down), or `None` outside the tile.
fn sample(x: f32, y: f32) -> Option<[f32; 3]> {
    let dx = ((x - 0.5).abs() - 0.28).max(0.0);
    let dy = ((y - 0.5).abs() - 0.28).max(0.0);
    if dx * dx + dy * dy > 0.22 * 0.22 {
        return None;
    }
    if in_mark(x, y) {
        Some(lerp(MARK_TOP, MARK_BOTTOM, ((y - 0.30) / 0.42).clamp(0.0, 1.0)))
    } else {
        Some(lerp(TILE_TOP, TILE_BOTTOM, y))
    }
}

fn lerp(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t)
}

fn in_mark(x: f32, y: f32) -> bool {
    let bar = (0.12..=0.56).contains(&x) && (0.30..=0.41).contains(&y);
    let stem = (0.29..=0.39).contains(&x) && (0.30..=0.72).contains(&y);
    bar || stem || CHEVRON_TIPS.iter().any(|&tip| in_chevron(x, y, tip))
}

fn in_chevron(x: f32, y: f32, tip: f32) -> bool {
    let back = tip - CHEVRON_ARM;
    let up = seg_dist(x, y, back, 0.5 - CHEVRON_RISE, tip, 0.5);
    let down = seg_dist(x, y, tip, 0.5, back, 0.5 + CHEVRON_RISE);
    up.min(down) <= CHEVRON_HALF
}

fn seg_dist(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let (dx, dy) = (bx - ax, by - ay);
    let t = (((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    (px - ax - t * dx).hypot(py - ay - t * dy)
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
    fn the_app_icon_is_a_charcoal_tile_with_an_orange_t() {
        let size = 64;
        let px = app_icon(size);
        assert_eq!(px.len(), (size * size * 4) as usize);
        let at = |x: u32, y: u32| &px[((y * size + x) * 4) as usize..][..4];
        assert_eq!(at(0, 0)[3], 0, "corners are transparent");
        let opaque = px.chunks(4).filter(|p| p[3] == 255).count();
        assert!(opaque > (size * size / 3) as usize, "only {opaque} opaque pixels");
        // The T's stem is orange and opaque; the tile under the mark is dark charcoal.
        let stem = at(22, 38);
        assert!(stem[3] == 255 && stem[0] > 200 && stem[2] < 80, "stem is {stem:?}");
        let tile = at(32, 60);
        assert!(tile[3] == 255 && tile[0] < 60, "tile is {tile:?}");
    }
}
