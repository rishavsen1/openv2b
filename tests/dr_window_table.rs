//! R1-11: the (start, end] DR window convention gets an independent anchor.
//! This table of covered slot indices was written BY HAND from the
//! one_month event list (wall-clock arithmetic on paper), not derived from
//! `DrEvent::contains`. The Python referee carries its own copy. If engine,
//! referee, and this table ever disagree, the convention drifted.
//!
//! Slot arithmetic: slot s covers the 15-min interval starting at s*15 min;
//! day d hour h -> slot 96*d + 4*h. Under (start, end], an event written as
//! start=17:00, end=19:00 covers the intervals STARTING 17:15 through 19:00.

use openv2b::scenario::Scenario;
use std::path::PathBuf;

/// (start_slot, end_slot, first_covered, last_covered, n_covered)
const HAND_TABLE: &[(usize, usize, usize, usize, usize)] = &[
    // Standard Tue/Thu 17:00-19:00 events: days 1,3,8,10,15,17,22,24,29.
    (164, 172, 165, 172, 8),     // day 1:  96+68 .. 96+76
    (356, 364, 357, 364, 8),     // day 3:  288+68
    (836, 844, 837, 844, 8),     // day 8:  768+68
    (1028, 1036, 1029, 1036, 8), // day 10: 960+68
    (1508, 1516, 1509, 1516, 8), // day 15: 1440+68
    (1700, 1708, 1701, 1708, 8), // day 17: 1632+68
    (2180, 2188, 2181, 2188, 8), // day 22: 2112+68
    (2372, 2380, 2373, 2380, 8), // day 24: 2304+68
    (2852, 2860, 2853, 2860, 8), // day 29: 2784+68
    // Boundary events.
    (0, 4, 1, 4, 4),              // day 0 00:00-01:00: covers 00:15..01:00
    (844, 848, 845, 848, 4),      // day 8 19:00-20:00 (back-to-back)
    (2556, 2566, 2557, 2566, 10), // day 26 15:00-17:30 (TOU straddle)
    (2876, 2879, 2877, 2879, 3),  // day 29 23:00-23:45 (ends at final slot)
];

#[test]
fn month_dr_events_match_hand_written_slot_table() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/one_month");
    let scenario = Scenario::load(&dir).expect("month scenario loads");
    assert_eq!(
        scenario.dr_events.len(),
        HAND_TABLE.len(),
        "event count drifted from the hand table"
    );
    let mut events: Vec<_> = scenario.dr_events.clone();
    events.sort_by_key(|e| e.start_slot);
    let mut table = HAND_TABLE.to_vec();
    table.sort_by_key(|t| t.0);
    for (e, &(start, end, first, last, n)) in events.iter().zip(&table) {
        assert_eq!((e.start_slot, e.end_slot), (start, end), "event boundaries");
        let covered: Vec<usize> = (0..2880).filter(|&s| e.contains(s)).collect();
        assert_eq!(
            covered.first(),
            Some(&first),
            "first covered slot of ({start},{end}]"
        );
        assert_eq!(
            covered.last(),
            Some(&last),
            "last covered slot of ({start},{end}]"
        );
        assert_eq!(covered.len(), n, "covered count of ({start},{end}]");
    }
}
