use super::*;

fn sources() -> ExplorerHeaders {
    ExplorerHeaders::new(
        ExplorerHeaderConfig {
            roots: vec![
                "https://one.invalid/api".into(),
                "https://two.invalid/api".into(),
            ],
        },
        1000,
    )
    .unwrap()
}

#[tokio::test(start_paused = true)]
async fn independent_jitter_pacing_round_robin_and_canceled_attempt_budget() {
    let mut sources = sources();
    let now = Instant::now();
    assert_eq!(sources.cap, 256);
    for source in &sources.sources {
        assert!((now..=now + Duration::from_secs(1800)).contains(&source.due));
    }
    for source in &mut sources.sources {
        source.due = now;
    }
    for attempt in 0..32 {
        let index = sources.reserve().unwrap();
        assert_eq!(index, attempt % 2);
        assert!(
            (now + Duration::from_secs(900)..=now + Duration::from_secs(2701))
                .contains(&sources.sources[index].due)
        );
        assert_eq!(sources.reserve(), None);
        tokio::time::advance(Duration::from_secs(1)).await;
    }
    assert_eq!(sources.reserve(), None);
    assert_eq!(sources.sources[0].remaining, 0);
    assert_eq!(sources.sources[1].remaining, 0);
    // Missed periods create one new visit, not catch-up bursts.
    tokio::time::advance(Duration::from_secs(100_000)).await;
    assert_eq!(sources.reserve(), Some(0));
    assert_eq!(sources.sources[0].remaining, 15);
    assert!(sources.sources[0].due >= Instant::now() + Duration::from_secs(900));
}

#[tokio::test(start_paused = true)]
async fn visit_wall_window_and_source_local_backoff() {
    let mut sources = sources();
    for source in &mut sources.sources {
        source.due = Instant::now();
    }
    assert_eq!(sources.reserve(), Some(0));
    sources.sources[0].expected = Some(crate::explorer_test_support::headers()[1].block_hash());
    tokio::time::advance(Duration::from_secs(121)).await;
    assert_eq!(sources.reserve(), Some(1));
    assert!(sources.sources[0].expected.is_some());
    let before = Instant::now();
    sources.sources[1].fail(Duration::from_secs(7200), false);
    assert!(sources.sources[1].due >= before + Duration::from_secs(7200));
    for _ in 0..20 {
        sources.sources[1].fail(Duration::ZERO, true);
    }
    assert!(sources.sources[1].due <= before + Duration::from_secs(86400));
    assert_eq!(sources.sources[1].failures, 6);
    sources.sources[1].suspended = true;
    sources.sources[0].due = Instant::now();
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(sources.reserve(), Some(0));
    sources.sources[0].complete();
    assert_eq!(sources.sources[0].failures, 0);
    assert!(sources.sources[0].expected.is_none());
}

#[test]
fn disabled_sources_have_no_deadline() {
    let sources = ExplorerHeaders::new(ExplorerHeaderConfig::default(), 5).unwrap();
    assert_eq!(sources.deadline(), None);
}

#[tokio::test(start_paused = true)]
async fn last_request_requires_full_deadline_without_a_busy_wakeup_loop() {
    let mut sources = sources();
    sources.sources[0].due = Instant::now();
    sources.sources[1].suspended = true;
    assert_eq!(sources.reserve(), Some(0));
    tokio::time::advance(Duration::from_secs(117)).await;
    assert_eq!(sources.deadline(), Some(Instant::now()));
    assert_eq!(sources.reserve(), Some(0)); // exactly three seconds remain
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(sources.reserve(), None);
    assert_eq!(sources.deadline(), Some(sources.sources[0].due));
    assert!(sources.deadline().unwrap() > Instant::now());
}
