use std::{
    future::Future,
    pin::{pin, Pin},
    sync::{atomic, Arc},
    task::{Context, Wake, Waker},
    time::Duration,
};

use futures::{stream::FusedStream, FutureExt, Stream, StreamExt};
use stream_broadcast::{StreamBroadcastExt, StreamBroadcastLossy};
use tokio::{task, time::timeout};

/// Always uses the same short timeout, so no test has to pick (and justify) its own duration.
async fn timeout_fast<F: Future>(future: F) -> Result<F::Output, tokio::time::error::Elapsed> {
    timeout(Duration::from_millis(10), future).await
}

#[tokio::test]
async fn broadcast() {
    let stream = futures::stream::iter(0..3).fuse();
    let broadcast = StreamBroadcastLossy::new(stream, 3);
    let broadcast2 = broadcast.clone();

    let all = broadcast.collect::<Vec<_>>().await;
    let all2 = broadcast2.collect::<Vec<_>>().await;
    assert_eq!(3, all.len());
    assert_eq!(3, all2.len());
}

#[tokio::test]
async fn new_broadcast_ignores_previous() {
    let stream = futures::stream::iter(0..3).fuse();
    let mut broadcast = StreamBroadcastLossy::new(stream, 3);
    broadcast.next().await.expect("Should be here");
    let broadcast2 = broadcast.clone();

    let all = broadcast.collect::<Vec<_>>().await;
    let all2 = broadcast2.collect::<Vec<_>>().await;
    assert_eq!(2, all.len());
    assert_eq!(2, all2.len());
}

#[tokio::test]
async fn indicates_skipped_entries() {
    let stream = futures::stream::iter(0..4).fuse();
    let broadcast = StreamBroadcastLossy::new(stream, 3);
    let mut broadcast2 = broadcast.clone();
    broadcast2.next().await.unwrap(); // fetch before running into cachemiss

    assert_eq!(4, broadcast.count().await);
    assert_eq!(
        (1..4).sum::<i32>(),
        broadcast2
            .zip(futures::stream::iter([0, 0, 0]))
            .fold(0, |acc, ((offset, x), expected_offset)| async move {
                assert_eq!(offset, expected_offset);
                acc + x
            })
            .await
    );
}

#[tokio::test]
async fn input_stream_is_never_called_after_first_none() {
    let broadcast = StreamBroadcastLossy::new(NeverStream::default().fuse(), 3);
    let broadcast2 = broadcast.clone();
    assert_eq!(0, broadcast.count().await);
    assert_eq!(0, broadcast2.count().await);

    #[derive(Default)]
    struct NeverStream(atomic::AtomicBool);

    impl Stream for NeverStream {
        type Item = ();

        fn poll_next(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            if self.0.load(atomic::Ordering::SeqCst) {
                panic!("Polled multiple times")
            }

            self.0.store(true, atomic::Ordering::SeqCst);
            std::task::Poll::Ready(None)
        }
    }
}

#[tokio::test]
async fn use_with_not_pin() {
    let input = futures::stream::iter(0..4)
        .then(|x| async move { x })
        .fuse();
    let broadcast = input.broadcast_lossy(3);
    assert_eq!(4, broadcast.count().await);
}

#[tokio::test]
async fn test_parallel() {
    const ITERATIONS: usize = 50;
    let stream1 = futures::stream::iter(0..ITERATIONS)
        .fuse()
        .then(|x| async move {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            x
        })
        .broadcast_lossy(5);
    let stream2 = stream1.clone();

    let (r1, r2) =
        futures::future::join(task::spawn(stream1.count()), task::spawn(stream2.count())).await;
    assert_eq!(r1.unwrap(), ITERATIONS);
    assert_eq!(r2.unwrap(), ITERATIONS)
}

#[tokio::test]
async fn weak_terminates_when_all_owned_are_destroyed() {
    let stream1 = futures::stream::iter(0..5).fuse().broadcast_lossy(5);
    let stream2 = stream1.clone();
    let mut weak = pin!(stream1.downgrade());
    assert_eq!(Some((0, 0)), weak.next().await);
    drop(stream1);
    assert_eq!(Some((0, 1)), weak.next().await);
    drop(stream2);
    assert_eq!(None, weak.next().await);
}

#[tokio::test]
async fn zst_items_respect_buffer_size() {
    // Vec::<()>::with_capacity(n).capacity() == usize::MAX, so reading the buffer size
    // via `cache.capacity()` never limits the cache for a zero-sized item type.
    let stream = futures::stream::iter(std::iter::repeat_n((), 4)).fuse();
    let mut broadcast = StreamBroadcastLossy::new(stream, 1);
    let broadcast2 = broadcast.clone();

    while broadcast.next().await.is_some() {}
    let all2 = broadcast2.collect::<Vec<_>>().await;
    assert_eq!(vec![(3, ())], all2);
}

#[tokio::test]
async fn terminated_stream_wakes_pending_subscribers() {
    // Subscriber `a` parks on the inner stream (Pending), registering its waker. Subscriber
    // `b` then observes `Ready(None)`. Without draining/waking `a`'s waker there, `a`'s task
    // would never be rescheduled -- even though a later re-poll would immediately see `None`.
    // Assert the waker itself gets invoked, rather than relying on tokio's scheduling to
    // happen to re-poll `a` anyway (which it does, independent of this bug).

    struct CountingWaker(atomic::AtomicUsize);
    impl Wake for CountingWaker {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref()
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, atomic::Ordering::SeqCst);
        }
    }

    let stream = PendingThenNone::default().fuse();
    let mut a = StreamBroadcastLossy::new(stream, 3);
    let mut b = a.clone();

    let counter = Arc::new(CountingWaker(atomic::AtomicUsize::new(0)));
    let waker = Waker::from(counter.clone());
    let mut cx = Context::from_waker(&waker);

    let mut a_next = a.next();
    assert!(Pin::new(&mut a_next).poll(&mut cx).is_pending());

    assert_eq!(None, b.next().await);

    assert_eq!(
        1,
        counter.0.load(atomic::Ordering::SeqCst),
        "subscriber `a`'s waker was never invoked after the stream terminated"
    );

    #[derive(Default)]
    struct PendingThenNone(atomic::AtomicBool);

    impl Stream for PendingThenNone {
        type Item = ();

        fn poll_next(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            if self.0.swap(true, atomic::Ordering::SeqCst) {
                std::task::Poll::Ready(None)
            } else {
                std::task::Poll::Pending
            }
        }
    }
}

#[tokio::test]
async fn debug_after_termination_does_not_panic() {
    let mut broadcast = StreamBroadcastLossy::new(futures::stream::iter(0..1).fuse(), 3);
    assert_eq!(Some((0, 0)), broadcast.next().await);
    assert_eq!(None, broadcast.next().await);
    // `pos` is now `global_pos + 1`; must not underflow-panic when computing `pending_messages`.
    assert!(format!("{broadcast:?}").contains("StreamBroadcastLossy"));
}

#[tokio::test]
async fn is_terminated_is_false_while_items_are_pending() {
    let stream = futures::stream::iter(0..2).fuse();
    let broadcast = StreamBroadcastLossy::new(stream, 3);
    let lagging = broadcast.clone();

    assert_eq!(2, broadcast.count().await); // drains the inner stream to termination
    assert!(
        !lagging.is_terminated(),
        "lagging subscriber still has buffered items to deliver"
    );
}

#[tokio::test]
async fn lossless_yields_every_item() {
    let stream = futures::stream::iter(0..10).fuse();
    let lossless = stream.broadcast_lossless(2);
    assert_eq!(
        (0..10).collect::<Vec<_>>(),
        lossless.collect::<Vec<_>>().await
    );
}

#[tokio::test]
async fn two_lossless_clones_see_all_items() {
    let stream = futures::stream::iter(0..10).fuse();
    let a = stream.broadcast_lossless(2);
    let b = a.clone();

    let (all_a, all_b) = futures::future::join(a.collect::<Vec<_>>(), b.collect::<Vec<_>>()).await;
    assert_eq!((0..10).collect::<Vec<_>>(), all_a);
    assert_eq!((0..10).collect::<Vec<_>>(), all_b);
}

#[tokio::test]
async fn lossless_and_lossy_share_the_buffer() {
    // `futures::stream::iter` never returns `Pending`, so `lossless` (the unconstrained min
    // holder) drains the whole source synchronously the first time it is polled, before `lossy`
    // ever gets a chance to run. `lossy` therefore legitimately skips ahead to the tail, exactly
    // like a lossy subscriber sharing the buffer with a lossless one that isn't kept in lock-step.
    let stream = futures::stream::iter(0..10).fuse();
    let lossless = stream.broadcast_lossless(2);
    let lossy = lossless.create_lossy();

    let (all_lossless, all_lossy) =
        futures::future::join(lossless.collect::<Vec<_>>(), lossy.collect::<Vec<_>>()).await;
    assert_eq!((0..10).collect::<Vec<_>>(), all_lossless);
    assert_eq!(vec![(8, 8), (0, 9)], all_lossy);
}

#[tokio::test]
async fn input_stream_is_not_polled_while_full() {
    struct CountingStream {
        polls: std::sync::Arc<atomic::AtomicUsize>,
        next: u32,
        max: u32,
    }

    impl Stream for CountingStream {
        type Item = u32;

        fn poll_next(
            mut self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            self.polls.fetch_add(1, atomic::Ordering::SeqCst);
            if self.next >= self.max {
                return std::task::Poll::Ready(None);
            }
            let item = self.next;
            self.next += 1;
            std::task::Poll::Ready(Some(item))
        }
    }

    let polls = std::sync::Arc::new(atomic::AtomicUsize::new(0));
    let stream = CountingStream {
        polls: polls.clone(),
        next: 0,
        max: 100,
    }
    .fuse();

    let mut lossless = stream.broadcast_lossless(2);
    let mut lossy = lossless.create_lossy();

    assert_eq!(Some((0, 0)), lossy.next().await);
    assert_eq!(Some((0, 1)), lossy.next().await);
    assert_eq!(2, polls.load(atomic::Ordering::SeqCst));

    // The buffer (size 2) is now full of unread items; the input stream must not be polled
    // again until `lossless` reads one of them.
    assert!(timeout_fast(lossy.next()).await.is_err());
    assert_eq!(2, polls.load(atomic::Ordering::SeqCst));

    assert_eq!(Some(0), lossless.next().await);
    assert_eq!(Some((0, 2)), lossy.next().await);
    assert_eq!(3, polls.load(atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn dropping_lagging_lossless_unblocks_others() {
    let stream = futures::stream::iter(0..10).fuse();
    let lossless = stream.broadcast_lossless(2);
    let mut lossy = lossless.create_lossy();

    assert_eq!(Some((0, 0)), lossy.next().await);
    assert_eq!(Some((0, 1)), lossy.next().await);
    assert!(timeout_fast(lossy.next()).await.is_err());

    drop(lossless);

    let rest = timeout_fast(lossy.collect::<Vec<_>>())
        .await
        .expect("dropping the lagging lossless subscriber should unblock the lossy one");
    assert_eq!(8, rest.len());
}

#[tokio::test]
async fn dropping_lagging_lossless_wakes_a_truly_parked_poller() {
    // `dropping_lagging_lossless_unblocks_others` above doesn't actually exercise the wake: its
    // `timeout_fast(lossy.next())` call drops the pending future once it elapses (leaving a
    // stale, harmless waker in `wakable`), and the later `lossy.collect()` succeeds via a
    // *fresh* poll that re-checks the gate on its own -- not via being woken from a suspended
    // state. This test isolates the actual mechanism: a poller that returned `Pending` and is
    // never polled again except via its stored waker being invoked.
    struct CountingWaker(atomic::AtomicUsize);
    impl Wake for CountingWaker {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref()
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, atomic::Ordering::SeqCst);
        }
    }

    let stream = futures::stream::iter(0..10).fuse();
    let lossless = stream.broadcast_lossless(2);
    let mut lossy = lossless.create_lossy();

    assert_eq!(Some((0, 0)), lossy.next().await);
    assert_eq!(Some((0, 1)), lossy.next().await);

    let counter = Arc::new(CountingWaker(atomic::AtomicUsize::new(0)));
    let waker = Waker::from(counter.clone());
    let mut cx = Context::from_waker(&waker);

    let mut lossy_next = lossy.next();
    assert!(
        Pin::new(&mut lossy_next).poll(&mut cx).is_pending(),
        "the buffer is full and `lossless` hasn't read anything yet, so the gate is closed"
    );

    drop(lossless);

    assert_eq!(
        1,
        counter.0.load(atomic::Ordering::SeqCst),
        "dropping the lagging lossless subscriber must wake pollers parked on the now-open gate"
    );
}

#[tokio::test]
async fn lossless_reading_from_cache_wakes_a_truly_parked_poller() {
    // A lagging lossless subscriber reading a backlogged item takes the early-return cache
    // branch in `StreamBroadcastState::poll`, which never touches `wakable` itself -- only
    // `set_lossless_pos` (called afterward, once its position is updated) does. This isolates
    // that specific wake, as opposed to the "produced a fresh item" path, which already wakes
    // via `poll()`'s own `Ready(Some)` branch before `set_lossless_pos` ever runs.
    struct CountingWaker(atomic::AtomicUsize);
    impl Wake for CountingWaker {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref()
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, atomic::Ordering::SeqCst);
        }
    }

    let stream = futures::stream::iter(0..10).fuse();
    let mut lossless = stream.broadcast_lossless(2);
    let mut lossy = lossless.create_lossy();

    // Fill the buffer (cap=2) via `lossy`, without `lossless` reading anything -- `lossless`
    // stays at pos 0, lagging behind global_pos=2.
    assert_eq!(Some((0, 0)), lossy.next().await);
    assert_eq!(Some((0, 1)), lossy.next().await);

    let counter = Arc::new(CountingWaker(atomic::AtomicUsize::new(0)));
    let waker = Waker::from(counter.clone());
    let mut cx = Context::from_waker(&waker);

    let mut lossy_next = lossy.next();
    assert!(
        Pin::new(&mut lossy_next).poll(&mut cx).is_pending(),
        "the buffer is full and `lossless` hasn't read anything yet, so the gate is closed"
    );

    // `lossless` reads its oldest buffered item (0), advancing its position from 0 to 1 via
    // the cache branch -- this frees the buffer slot and should reopen the gate.
    assert_eq!(Some(0), lossless.next().await);

    assert_eq!(
        1,
        counter.0.load(atomic::Ordering::SeqCst),
        "advancing a lagging lossless subscriber's position via a cache read must wake pollers \
         parked on the now-open gate"
    );
}

#[tokio::test]
async fn lossless_is_terminated_after_natural_termination_does_not_panic() {
    // Once a lossless subscriber observes `Ready(None)`, it unregisters itself. Everything that
    // reads its position afterward (`is_terminated`, `create_lossy`, `Clone`, `Debug`, or a
    // second poll) must not blow up just because the registry no longer has an entry for it.
    let stream = futures::stream::iter(0..1).fuse();
    let mut lossless = stream.broadcast_lossless(1);
    assert_eq!(Some(0), lossless.next().await);
    assert_eq!(None, lossless.next().await); // unregisters itself here
    assert!(lossless.is_terminated());
}

#[tokio::test]
async fn create_lossless_from_lagging_lossy_clamps_to_oldest_cached_item() {
    let stream = futures::stream::iter(0..5).fuse();
    let lossy = stream.broadcast_lossy(2);
    let lossy2 = lossy.clone();

    // Drive `lossy2` far ahead so it falls behind the cache window.
    assert_eq!(5, lossy2.count().await);

    let lossless = lossy.create_lossless();
    let items = lossless.collect::<Vec<_>>().await;
    // Only the last 2 items (cache size) are recoverable; nothing after them is skipped.
    assert_eq!(vec![3, 4], items);
}

#[tokio::test]
async fn create_lossy_from_lossless_does_not_backpressure() {
    let stream = futures::stream::iter(0..10).fuse();
    let lossless = stream.broadcast_lossless(2);
    let lossy = lossless.create_lossy();
    drop(lossless);

    // With no lossless subscriber left, `lossy` behaves like a plain lossy broadcast.
    let all = timeout_fast(lossy.collect::<Vec<_>>())
        .await
        .expect("lossy subscriber should not be blocked once the lossless sibling is gone");
    assert_eq!(10, all.len());
}

#[tokio::test]
async fn lossless_drains_buffer_after_input_terminated() {
    // `lossy` alone would stall permanently trying to discover termination: with a cap-2
    // buffer already full of two items `lossless` hasn't read, even the poll that would just
    // observe `None` is gated. Drive both concurrently, as the documented caveat requires.
    let stream = futures::stream::iter(0..2).fuse();
    let lossless = stream.broadcast_lossless(2);
    let lossy = lossless.create_lossy();

    let (lossy_count, remaining) = timeout_fast(futures::future::join(
        lossy.count(),
        lossless.collect::<Vec<_>>(),
    ))
    .await
    .expect("both subscribers should drain fully");
    assert_eq!(2, lossy_count);
    assert_eq!(vec![0, 1], remaining);
}

#[tokio::test]
async fn weak_create_lossy_and_lossless() {
    let stream = futures::stream::iter(0..5).fuse();
    let lossless = stream.broadcast_lossless(5);
    let weak = lossless.create_lossy().downgrade();

    let mut upgraded_lossy = weak.create_lossy().expect("broadcast is still alive");
    assert_eq!(Some((0, 0)), upgraded_lossy.next().await);

    let mut upgraded_lossless = weak.create_lossless().expect("broadcast is still alive");
    assert_eq!(Some(0), upgraded_lossless.next().await);

    drop(lossless);
    drop(upgraded_lossy);
    drop(upgraded_lossless);
    assert!(weak.create_lossy().is_none());
    assert!(weak.create_lossless().is_none());
}

#[tokio::test]
async fn parallel_lossless() {
    const ITERATIONS: usize = 50;
    let stream1 = futures::stream::iter(0..ITERATIONS)
        .fuse()
        .broadcast_lossless(5);
    let stream2 = stream1.clone();

    let mut seed1 = 37u8;
    let mut seed2 = 13u8;

    let count1 = stream1
        .then(move |x| {
            seed1 = seed1.wrapping_mul(43);
            tokio::time::sleep(Duration::from_micros(seed1 as _)).map(move |_| x)
        })
        .count();
    let count2 = stream2
        .then(move |x| {
            seed2 = seed2.wrapping_mul(43);
            tokio::time::sleep(Duration::from_micros(seed2 as _)).map(move |_| x)
        })
        .count();

    let (r1, r2) = futures::future::join(task::spawn(count1), task::spawn(count2)).await;
    assert_eq!(r1.unwrap(), ITERATIONS);
    assert_eq!(r2.unwrap(), ITERATIONS)
}

#[tokio::test]
#[allow(deprecated)]
async fn deprecated_api_still_compiles() {
    use stream_broadcast::StreamBroadcast;

    let stream = futures::stream::iter(0..3).fuse();
    let broadcast = StreamBroadcast::new(stream, 3);
    let broadcast2 = broadcast.clone();
    let weak = broadcast.downgrade();

    assert_eq!(3, broadcast.count().await);
    assert_eq!(3, broadcast2.count().await);
    assert!(weak.upgrade().is_none());

    let via_ext = futures::stream::iter(0..3).fuse().broadcast(3);
    assert_eq!(3, via_ext.count().await);
}
