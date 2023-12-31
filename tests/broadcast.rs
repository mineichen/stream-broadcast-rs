use std::{pin::pin, sync::atomic};

use futures::{Stream, StreamExt};
use stream_broadcast::{StreamBroadcast, StreamBroadcastExt};

#[tokio::test]
async fn broadcast() {
    let stream = futures::stream::iter(0..3).fuse();
    let broadcast = StreamBroadcast::new(stream, 3);
    let broadcast2 = broadcast.clone();

    let all = broadcast.collect::<Vec<_>>().await;
    let all2 = broadcast2.collect::<Vec<_>>().await;
    assert_eq!(3, all.len());
    assert_eq!(3, all2.len());
}

#[tokio::test]
async fn new_broadcast_ignores_previous() {
    let stream = futures::stream::iter(0..3).fuse();
    let mut broadcast = StreamBroadcast::new(stream, 3);
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
    let broadcast = StreamBroadcast::new(stream, 3);
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
    let broadcast = StreamBroadcast::new(NeverStream::default().fuse(), 3);
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
    let broadcast = input.broadcast(3);
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
        .broadcast(5);
    let stream2 = stream1.clone();

    let (r1, r2) = futures::future::join(
        tokio::task::spawn(stream1.count()),
        tokio::task::spawn(stream2.count()),
    )
    .await;
    assert_eq!(r1.unwrap(), ITERATIONS);
    assert_eq!(r2.unwrap(), ITERATIONS)
}

#[tokio::test]
async fn weak_terminates_when_all_owned_are_destroyed() {
    let stream1 = futures::stream::iter(0..5).fuse().broadcast(5);
    let stream2 = stream1.clone();
    let mut weak = pin!(stream1.downgrade());
    assert_eq!(Some((0, 0)), weak.next().await);
    drop(stream1);
    assert_eq!(Some((0, 1)), weak.next().await);
    drop(stream2);
    assert_eq!(None, weak.next().await);
}
