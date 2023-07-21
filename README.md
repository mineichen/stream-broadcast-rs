Runtime independent broadcast, which only polls it's underlying stream if no pending data is available.
```rust
use futures::StreamExt;
use stream_broadcast::StreamBroadcastExt;

#[tokio::main]
async fn main() {
    let broadcast = futures::stream::iter('a'..='d').broadcast(3);
    let broadcast2 = broadcast.clone();
    assert_eq!(4, broadcast.count().await);
    // Letter 'a' wasn't available anymore due to `broadcast(3)`, which limits the buffer to 3 items
    // Left side of tuple represents number of missed items
    assert_eq!(vec![(1, 'b'), (0, 'c'), (0, 'd')], broadcast2.collect::<Vec<_>>().await);
}
```
Uses `#![forbid(unsafe_code)]`
# Difference to other libraries:
[shared_stream](https://docs.rs/shared_stream/0.2.1/shared_stream/index.html):
- Caches the entire stream from start, which is not practical for big datasets.
  This crate streams from the same position where the clone-origin is currently at
- [shared_stream](https://docs.rs/shared_stream/0.2.1/shared_stream/index.html) never skips an entry. This library only provides information about missing data
- High risk of leaking memory


[tokio::sync::broadcast](https://docs.rs/tokio/latest/tokio/sync/broadcast/index.html):
- Broadcasts don't implement Stream directly, but [tokio_stream](https://docs.rs/tokio-stream/latest/tokio_stream/wrappers/struct.BroadcastStream.html) provides a wrapper.
- Entries are pushed actively to the sender (No Lazy evaluation when stream is paused). This requires a subroutine, which has to be managed somehow.
- Instead of returning missing frames in the ErrorVariant ([tokio_stream](https://docs.rs/tokio-stream/latest/tokio_stream/wrappers/struct.BroadcastStream.html)), this library returns a tuple (missing_frames_since_last_frame, TData) to mitigate errors when doing stuff like `stream.count()`
