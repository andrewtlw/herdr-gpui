use super::*;
use std::sync::atomic::AtomicUsize;

fn wait_for(loads: &AtomicUsize, expected: usize) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while loads.load(Ordering::Acquire) < expected {
        assert!(Instant::now() < deadline, "the worker was never woken");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// The worker blocks until something wakes it, so it reads the settings at
/// start and again for each reload, though no sound is asked for, and does
/// nothing in between.
#[test]
fn an_idle_worker_reads_settings_at_start_and_for_each_reload() {
    let loads = Arc::new(AtomicUsize::new(0));
    let counted = loads.clone();
    let service = Service::start(
        move || {
            counted.fetch_add(1, Ordering::AcqRel);
            Ok(Settings::default())
        },
        |_, _, _, _, _| Ok(()),
    );
    wait_for(&loads, 1);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        loads.load(Ordering::Acquire),
        1,
        "idle, nothing is read again"
    );
    service.reload();
    wait_for(&loads, 2);
    service.reload();
    wait_for(&loads, 3);
}

/// Reloads made before the worker wakes queue one wake between them, so a
/// burst of them never fills the queue sounds use.
#[test]
fn a_burst_of_reloads_queues_one_wake() {
    let (sender, receiver) = mpsc::sync_channel(4);
    let service = Service {
        sender: Some(sender),
        settings: Default::default(),
        reload: Default::default(),
        stop: Default::default(),
        volume: Default::default(),
    };
    for _ in 0..3 {
        service.reload();
    }
    assert!(matches!(receiver.try_recv(), Ok(Wake::Reload)));
    assert!(receiver.try_recv().is_err());
    assert!(
        service.request(PlaybackRequest::Preview),
        "the queue still has room for a sound"
    );
}
