//! Pure logic for the download list: speed smoothing, ETA text and sorting.

use std::cmp::Ordering;

/// What the list is ordered by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    /// The order downloads were added in (the engine's own order).
    Added,
    Name,
    Size,
    Progress,
    Speed,
}

impl SortKey {
    pub fn label(self) -> &'static str {
        match self {
            SortKey::Added => "Added",
            SortKey::Name => "Name",
            SortKey::Size => "Size",
            SortKey::Progress => "Progress",
            SortKey::Speed => "Speed",
        }
    }

    /// The next key when the sort button is clicked.
    pub fn next(self) -> SortKey {
        match self {
            SortKey::Added => SortKey::Name,
            SortKey::Name => SortKey::Size,
            SortKey::Size => SortKey::Progress,
            SortKey::Progress => SortKey::Speed,
            SortKey::Speed => SortKey::Added,
        }
    }
}

/// The fields of a row that sorting looks at.
#[derive(Clone, Copy, Debug)]
pub struct SortRow<'a> {
    pub id: u64,
    pub name: &'a str,
    pub total: Option<u64>,
    pub done: u64,
    pub speed: f64,
}

fn progress(r: &SortRow) -> f64 {
    match r.total {
        Some(t) if t > 0 => r.done as f64 / t as f64,
        _ => 0.0,
    }
}

/// Order two rows by `key`, ascending. Ties fall back to the add order, so a
/// sort is stable and rows do not jump about when speeds tie.
pub fn compare(key: SortKey, a: &SortRow, b: &SortRow) -> Ordering {
    let primary = match key {
        SortKey::Added => Ordering::Equal,
        SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        SortKey::Size => a.total.unwrap_or(0).cmp(&b.total.unwrap_or(0)),
        SortKey::Progress => progress(a).total_cmp(&progress(b)),
        SortKey::Speed => a.speed.total_cmp(&b.speed),
    };
    primary.then(a.id.cmp(&b.id))
}

/// Exponential moving average of a speed sample. The first sample (previous
/// of zero) is taken as is, so a download shows a speed at once.
pub fn smooth(prev: f64, sample: f64) -> f64 {
    const ALPHA: f64 = 0.3;
    let sample = if sample.is_finite() && sample > 0.0 { sample } else { 0.0 };
    if prev <= 0.0 { sample } else { prev + ALPHA * (sample - prev) }
}

/// Time left as "45s", "2m 05s" or "1h 03m", or "~3d 4h" for very long waits.
pub fn fmt_eta(secs: u64) -> String {
    if secs >= 86_400 {
        format!("~{}d {}h", secs / 86_400, (secs % 86_400) / 3600)
    } else if secs >= 3600 {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// Turn per-connection ranges into ordered bar spans of (portion, filled
/// fraction). `parts` are (start, end inclusive, done). Gaps between parts
/// become empty spans, so the spans always cover the whole file in order.
pub fn part_spans(parts: &[(u64, u64, u64)], total: u64) -> Vec<(u16, f32)> {
    if total == 0 {
        return Vec::new();
    }
    let scale = |bytes: u64| -> u16 { ((bytes as u128 * 10_000) / total as u128).clamp(0, 10_000) as u16 };
    let mut spans = Vec::new();
    let mut at = 0u64;
    for &(start, end, done) in parts {
        let start = start.max(at).min(total);
        let end = end.saturating_add(1).min(total);
        if end <= start {
            continue;
        }
        if start > at {
            spans.push((scale(start - at).max(1), 0.0));
        }
        let len = end - start;
        spans.push((scale(len).max(1), (done.min(len) as f32 / len as f32).clamp(0.0, 1.0)));
        at = end;
    }
    if at < total {
        spans.push((scale(total - at).max(1), 0.0));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: u64, name: &str, total: Option<u64>, done: u64, speed: f64) -> SortRow<'_> {
        SortRow { id, name, total, done, speed }
    }

    #[test]
    fn eta_formats() {
        assert_eq!(fmt_eta(5), "5s");
        assert_eq!(fmt_eta(125), "2m 05s");
        assert_eq!(fmt_eta(3780), "1h 03m");
        assert_eq!(fmt_eta(90_000), "~1d 1h");
    }

    #[test]
    fn smoothing_damps_spikes() {
        assert_eq!(smooth(0.0, 100.0), 100.0);
        let s = smooth(100.0, 1000.0);
        assert!(s > 100.0 && s < 1000.0);
        assert_eq!(smooth(100.0, f64::NAN), 70.0);
        assert_eq!(smooth(100.0, -5.0), 70.0);
    }

    #[test]
    fn sorting_by_each_key() {
        let a = row(1, "beta", Some(100), 50, 5.0);
        let b = row(2, "Alpha", Some(300), 30, 1.0);
        let c = row(3, "gamma", None, 0, 9.0);
        let order = |k| {
            let mut v = vec![a, b, c];
            v.sort_by(|x, y| compare(k, x, y));
            v.iter().map(|r| r.id).collect::<Vec<_>>()
        };
        assert_eq!(order(SortKey::Added), vec![1, 2, 3]);
        assert_eq!(order(SortKey::Name), vec![2, 1, 3]);
        assert_eq!(order(SortKey::Size), vec![3, 1, 2]);
        assert_eq!(order(SortKey::Progress), vec![3, 2, 1]);
        assert_eq!(order(SortKey::Speed), vec![2, 1, 3]);
    }

    #[test]
    fn keys_cycle_back_to_start() {
        let mut k = SortKey::Added;
        for _ in 0..5 {
            k = k.next();
        }
        assert_eq!(k, SortKey::Added);
    }

    #[test]
    fn spans_cover_file_in_order() {
        let spans = part_spans(&[(0, 49, 50), (50, 99, 10)], 100);
        assert_eq!(spans, vec![(5000, 1.0), (5000, 0.2)]);
    }

    #[test]
    fn spans_fill_gaps_and_clamp() {
        let spans = part_spans(&[(25, 49, 99)], 100);
        assert_eq!(spans, vec![(2500, 0.0), (2500, 1.0), (5000, 0.0)]);
        assert!(part_spans(&[], 0).is_empty());
    }
}
