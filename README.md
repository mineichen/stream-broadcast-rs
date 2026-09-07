Runtime independent broadcast, which only polls it's underlying stream if no pending data is available.
```rust
use futures::StreamExt;
use stream_broadcast::StreamBroadcastExt;

#[tokio::main]
async fn main() {
    let broadcast = futures::stream::iter('a'..='d').fuse().broadcast_lossy(3);
    let broadcast2 = broadcast.clone();
    assert_eq!(4, broadcast.count().await);
    // Letter 'a' wasn't available anymore due to `broadcast_lossy(3)`, which limits the buffer to 3 items
    // Left side of tuple represents number of missed items
    assert_eq!(vec![(1, 'b'), (0, 'c'), (0, 'd')], broadcast2.collect::<Vec<_>>().await);
}
```
Uses `#![forbid(unsafe_code)]`

# Lossless subscribers
`broadcast_lossy` allows subscribers to fall behind and skip items. If every item matters,
`broadcast_lossless` instead applies backpressure: while a lossless subscriber still needs the
oldest buffered item, the input stream is not polled, so producers feeding it stall. Lossless
subscribers yield plain items -- there is no `missed` counter, because nothing is ever missed.
```rust
use futures::StreamExt;
use stream_broadcast::StreamBroadcastExt;

#[tokio::main]
async fn main() {
    let lossless = futures::stream::iter('a'..='d').fuse().broadcast_lossless(3);
    // Yields every item, in order, with no `missed` counter.
    assert_eq!(vec!['a', 'b', 'c', 'd'], lossless.collect::<Vec<_>>().await);
}
```
A lossless subscriber that is never polled (or leaked) stalls every other subscriber sharing
its buffer, including lossy ones -- see [StreamBroadcastLossless] for the full caveats and a
demonstration of the backpressure using a bounded channel.

`broadcast()` and `StreamBroadcast` still work but are deprecated in favor of `broadcast_lossy()`
and `StreamBroadcastLossy`.
# Difference to other libraries:
[shared_stream](https://docs.rs/shared_stream/0.2.1/shared_stream/index.html):
- Caches the entire stream from start, which is not practical for big datasets.
  This crate streams from the same position where the clone-origin is currently at
- [shared_stream](https://docs.rs/shared_stream/0.2.1/shared_stream/index.html) never skips an entry.
  - `stream_broadcast::StreamBroadcastLossy` provides information about missing data before a item
  - `stream_broadcast::StreamBroadcastLossless` stalls source polls until all StreamBroadcastLossless have space for it
- High risk of leaking memory


[tokio::sync::broadcast](https://docs.rs/tokio/latest/tokio/sync/broadcast/index.html):
- Broadcasts don't implement Stream directly, but [tokio_stream](https://docs.rs/tokio-stream/latest/tokio_stream/wrappers/struct.BroadcastStream.html) provides a wrapper.
- Entries are pushed actively to the sender (No Lazy evaluation when stream is paused). This requires a subroutine, which has to be managed somehow.
  - This can be emulated with `stream_broadcast::StreamBroadcastLossy<futures::channel::mpsc::Receiver<T>>`
- Instead of returning missing frames in the ErrorVariant ([tokio_stream](https://docs.rs/tokio-stream/latest/tokio_stream/wrappers/struct.BroadcastStream.html)), 
  `stream_broadcast::StreamBroadcastLossy` returns a tuple (missing_frames_since_last_frame, TData) to avoid silly mistakes on operations like `stream.count()`
