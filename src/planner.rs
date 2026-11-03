//! Pure segment planning: which segments look slow, and where to split.
//!
//! Works on plain data so it does not depend on engine types. No IO.

/// Throughput snapshot for one segment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SegmentRate {
    pub id: u64,
    pub bytes_per_sec: f64,
    pub age_secs: f64,
}

/// Alignment for split points, in bytes.
const ALIGN: u64 = 4096;

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let n = values.len();
    Some(if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    })
}

/// Segments at least `min_age_secs` old whose rate is below
/// `median_rate / factor`. The median is taken over all given rates.
///
/// Returns ids sorted ascending. Returns empty for empty input or when
/// `factor` is not greater than 1 (or is NaN).
pub fn slow_segments(rates: &[SegmentRate], factor: f64, min_age_secs: f64) -> Vec<u64> {
    if !(factor > 1.0) {
        return Vec::new();
    }
    let mut speeds: Vec<f64> = rates.iter().map(|r| r.bytes_per_sec).collect();
    let Some(med) = median(&mut speeds) else {
        return Vec::new();
    };
    let threshold = med / factor;
    let mut ids: Vec<u64> = rates
        .iter()
        .filter(|r| r.age_secs >= min_age_secs && r.bytes_per_sec < threshold)
        .map(|r| r.id)
        .collect();
    ids.sort_unstable();
    ids
}

/// Index of the largest remaining range, measured as `end - start`.
/// Ties go to the lower `start`. `None` when `ranges` is empty.
///
/// Each tuple is `(segment_id, start, end)`.
pub fn pick_split(ranges: &[(u64, u64, u64)]) -> Option<usize> {
    let mut best: Option<(usize, u64, u64)> = None;
    for (i, &(_, start, end)) in ranges.iter().enumerate() {
        let len = end.saturating_sub(start);
        best = match best {
            None => Some((i, len, start)),
            Some((bi, bl, bs)) => {
                if len > bl || (len == bl && start < bs) {
                    Some((i, len, start))
                } else {
                    Some((bi, bl, bs))
                }
            }
        };
    }
    best.map(|(i, _, _)| i)
}

/// Split point for the remaining range `[start, end)`.
///
/// The midpoint rounded up to a 4096-byte boundary. Returns `None` unless
/// the range is at least `2 * 4096` bytes. The result is always strictly
/// inside `(start, end)`. The right-hand part may be shorter than 4096
/// bytes when `start` is not aligned.
pub fn split_point(start: u64, end: u64) -> Option<u64> {
    let len = end.checked_sub(start)?;
    if len < 2 * ALIGN {
        return None;
    }
    let mid = start + len / 2;
    let aligned = mid.div_ceil(ALIGN).checked_mul(ALIGN)?;
    if aligned > start && aligned < end {
        Some(aligned)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(id: u64, bps: f64, age: f64) -> SegmentRate {
        SegmentRate {
            id,
            bytes_per_sec: bps,
            age_secs: age,
        }
    }

    #[test]
    fn slow_segments_empty_input() {
        assert!(slow_segments(&[], 3.0, 1.0).is_empty());
    }

    #[test]
    fn slow_segments_bad_factor_returns_empty() {
        let rates = [r(1, 0.0, 10.0), r(2, 100.0, 10.0)];
        assert!(slow_segments(&rates, 1.0, 1.0).is_empty());
        assert!(slow_segments(&rates, f64::NAN, 1.0).is_empty());
    }

    #[test]
    fn slow_segments_uses_median_and_sorts_ids() {
        // Rates 100, 100, 100, 10, 10 -> median 100, threshold 100/3 = 33.3.
        let rates = [
            r(9, 100.0, 10.0),
            r(4, 10.0, 10.0),
            r(2, 100.0, 10.0),
            r(7, 10.0, 10.0),
            r(5, 100.0, 10.0),
        ];
        assert_eq!(slow_segments(&rates, 3.0, 5.0), vec![4, 7]);
    }

    #[test]
    fn median_with_even_count_averages_middle_pair() {
        // Rates 10, 20, 60, 100 -> median 40, threshold 40/2 = 20.
        // 10 is below 20; 20 is not strictly below 20.
        let rates = [
            r(1, 10.0, 10.0),
            r(2, 20.0, 10.0),
            r(3, 60.0, 10.0),
            r(4, 100.0, 10.0),
        ];
        assert_eq!(slow_segments(&rates, 2.0, 1.0), vec![1]);
    }

    #[test]
    fn young_segment_is_not_preempted() {
        let rates = [
            r(1, 100.0, 30.0),
            r(2, 100.0, 30.0),
            r(3, 1.0, 2.0),
            r(4, 1.0, 30.0),
        ];
        // Segment 3 is slow but only 2 s old, below min_age 5 s.
        assert_eq!(slow_segments(&rates, 3.0, 5.0), vec![4]);
    }

    #[test]
    fn all_zero_rates_flag_nothing() {
        let rates = [r(1, 0.0, 10.0), r(2, 0.0, 10.0)];
        assert!(slow_segments(&rates, 3.0, 1.0).is_empty());
    }

    #[test]
    fn pick_split_empty_is_none() {
        assert_eq!(pick_split(&[]), None);
    }

    #[test]
    fn pick_split_returns_largest_range() {
        let ranges = [(1, 0, 100), (2, 200, 900), (3, 1000, 1050)];
        assert_eq!(pick_split(&ranges), Some(1));
    }

    #[test]
    fn pick_split_tie_goes_to_lower_start() {
        let ranges = [(1, 5000, 6000), (2, 0, 1000), (3, 9000, 10000)];
        assert_eq!(pick_split(&ranges), Some(1));
    }

    #[test]
    fn split_point_needs_two_aligned_blocks() {
        assert_eq!(split_point(0, 8191), None);
        assert!(split_point(0, 8192).is_some());
        assert_eq!(split_point(10, 10), None);
        assert_eq!(split_point(10, 5), None);
    }

    #[test]
    fn split_point_is_aligned_and_inside() {
        // Midpoint 50_000 rounds up to 53_248 (13 * 4096).
        assert_eq!(split_point(0, 100_000), Some(53_248));
        assert_eq!(split_point(4096, 4096 + 8192), Some(4096 + 4096));
        for (s, e) in [(100u64, 8292u64), (1, 20_000), (4095, 50_001)] {
            let p = split_point(s, e).unwrap();
            assert_eq!(p % ALIGN, 0);
            assert!(p > s && p < e);
        }
    }
}
