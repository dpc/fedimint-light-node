use super::*;

#[test]
fn degradation_warnings_stay_bounded_across_recovery_and_failure() {
    let mut progress = SyncProgress::default();
    let start = Instant::now();
    assert!(!progress.warning_due(1, false, start));
    assert!(progress.warning_due(0, false, start));
    for seconds in 1..60 {
        let now = start + Duration::from_secs(seconds);
        assert!(!progress.warning_due(1, false, now));
        assert!(!progress.warning_due(0, false, now));
        assert!(!progress.warning_due(1, true, now));
    }
    assert!(progress.warning_due(1, true, start + Duration::from_secs(60)));
    assert!(!progress.warning_due(0, true, start + Duration::from_secs(61)));
}

#[test]
fn summaries_follow_selection_or_minute_not_each_visit() {
    let mut progress = SyncProgress::default();
    let start = Instant::now();
    assert!(progress.summary_due(1, start));
    for seconds in 1..60 {
        assert!(!progress.summary_due(1, start + Duration::from_secs(seconds)));
    }
    assert!(progress.summary_due(1, start + Duration::from_secs(60)));
    assert!(progress.summary_due(2, start + Duration::from_secs(61)));
    assert!(!progress.summary_due(2, start + Duration::from_secs(62)));
}
