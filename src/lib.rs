#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

use futures::stream::{FusedStream, Stream};
use pin_project::pin_project;
use std::{
    collections::BTreeMap,
    ops::DerefMut,
    pin::Pin,
    sync::{atomic::AtomicU64, Arc, Mutex},
    task::Poll,
};

mod weak;

pub use weak::*;

pub trait StreamBroadcastExt: FusedStream + Sized {
    #[deprecated(since = "0.3.1", note = "renamed to `broadcast_lossy`")]
    fn broadcast(self, size: usize) -> StreamBroadcastLossy<Self>;

    /// Broadcasts the stream, allowing subscribers to fall behind. A subscriber that falls
    /// more than `size` items behind skips ahead and reports the number of skipped items as
    /// the first element of the `(missed, item)` tuple it yields.
    fn broadcast_lossy(self, size: usize) -> StreamBroadcastLossy<Self>;

    /// Broadcasts the stream without ever skipping items: while it still needs the oldest
    /// buffered item, the input stream is not polled, so producers feeding it stall. Yields
    /// `T::Item` directly -- there is no `missed` counter, because nothing is ever missed.
    ///
    /// Because a lossless subscriber can hold the whole broadcast's progress hostage, an alive
    /// but never-polled (or leaked) instance stalls every other subscriber sharing the same
    /// buffer, including lossy ones and [WeakStreamBroadcast] handles.
    ///
    /// ```
    /// # #[tokio::main]
    /// # async fn main() {
    /// use futures::{SinkExt, StreamExt};
    /// use std::time::Duration;
    /// use stream_broadcast::StreamBroadcastExt;
    ///
    /// // `mpsc::channel(1)` holds exactly one unread item at a time.
    /// let (mut tx, rx) = futures::channel::mpsc::channel(1);
    /// let mut lossless = rx.broadcast_lossless(1);
    /// let mut lossy = lossless.create_lossy();
    ///
    /// tx.send('a').await.unwrap();
    /// // Move 'a' into the broadcast buffer (also size 1), freeing the channel again.
    /// assert_eq!(Some((0, 'a')), lossy.next().await);
    /// tx.send('b').await.unwrap();
    ///
    /// // The broadcast buffer now holds 'a', which `lossless` hasn't read yet, so it is full.
    /// // A further `send` cannot complete: the input stream would need to be polled for 'b',
    /// // which would overwrite 'a'. Pass `&mut send_c` to `timeout` so only the borrow is
    /// // dropped once it elapses, keeping `send_c` itself intact to complete below.
    /// let mut send_c = tx.send('c');
    /// assert!(tokio::time::timeout(Duration::from_millis(50), &mut send_c)
    ///     .await
    ///     .is_err());
    ///
    /// // Consuming from `lossless` frees the buffer slot, so the broadcast can advance again and
    /// // read 'b' out of the channel, which lets `send_c` complete too.
    /// // Note the plain item -- no `(missed, item)` tuple.
    /// assert_eq!(Some('a'), lossless.next().await);
    /// assert_eq!(Some((0, 'b')), lossy.next().await);
    /// send_c.await.unwrap();
    /// # }
    /// ```
    fn broadcast_lossless(self, size: usize) -> StreamBroadcastLossless<Self>;
}

#[allow(deprecated)]
impl<T: FusedStream + Sized> StreamBroadcastExt for T
where
    T::Item: Clone,
{
    fn broadcast(self, size: usize) -> StreamBroadcastLossy<Self> {
        self.broadcast_lossy(size)
    }

    fn broadcast_lossy(self, size: usize) -> StreamBroadcastLossy<Self> {
        StreamBroadcastLossy::new(self, size)
    }

    fn broadcast_lossless(self, size: usize) -> StreamBroadcastLossless<Self> {
        StreamBroadcastLossless::new(self, size)
    }
}

/// Renamed to [StreamBroadcastLossy].
#[deprecated(since = "0.3.1", note = "renamed to `StreamBroadcastLossy`")]
pub type StreamBroadcast<T> = StreamBroadcastLossy<T>;

#[pin_project]
pub struct StreamBroadcastLossy<T: FusedStream> {
    pos: u64,
    id: u64,
    state: Arc<Mutex<Pin<Box<StreamBroadcastState<T>>>>>,
}

impl<T: FusedStream> std::fmt::Debug for StreamBroadcastLossy<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pending = self
            .state
            .lock()
            .unwrap()
            .global_pos
            .saturating_sub(self.pos);
        f.debug_struct("StreamBroadcastLossy")
            .field("pending_messages", &pending)
            .field("strong_count", &Arc::strong_count(&self.state))
            .finish()
    }
}

impl<T: FusedStream> Clone for StreamBroadcastLossy<T> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            id: create_id(),
            pos: self.pos,
        }
    }
}

impl<T: FusedStream> StreamBroadcastLossy<T>
where
    T::Item: Clone,
{
    pub fn new(outer: T, size: usize) -> Self {
        Self {
            state: Arc::new(Mutex::new(Box::pin(StreamBroadcastState::new(outer, size)))),
            id: create_id(),
            pos: 0,
        }
    }

    /// Creates a weak broadcast which terminates its stream, if all 'strong' [StreamBroadcastLossy] went out of scope
    ///
    /// ```
    /// # #[tokio::main]
    /// # async fn main() {
    /// use futures::StreamExt;
    /// use stream_broadcast::StreamBroadcastExt;
    ///
    /// let stream = futures::stream::iter(0..).fuse().broadcast_lossy(5);
    /// let mut weak = std::pin::pin!(stream.downgrade());
    /// assert_eq!(Some((0, 0)), weak.next().await);
    /// drop(stream);
    /// assert_eq!(None, weak.next().await);
    /// # }
    /// ```
    pub fn downgrade(&self) -> WeakStreamBroadcast<T> {
        WeakStreamBroadcast::new(Arc::downgrade(&self.state), self.pos)
    }

    /// In contrast to clone, this method only shows new messages provided by the source stream
    pub fn re_subscribe(&self) -> Self {
        Self {
            state: self.state.clone(),
            id: create_id(),
            pos: self.state.lock().unwrap().global_pos,
        }
    }

    /// Creates a lossless subscriber on the same shared buffer, starting at this subscriber's
    /// current position. Items already evicted from the buffer cannot be recovered: if this
    /// subscriber has fallen behind, the new lossless subscriber's start position is clamped to
    /// the oldest item still cached.
    pub fn create_lossless(&self) -> StreamBroadcastLossless<T> {
        let id = create_id();
        let mut lock = self.state.lock().unwrap_or_else(|e| e.into_inner());
        lock.as_mut().register_lossless(id, self.pos);
        StreamBroadcastLossless {
            id,
            state: self.state.clone(),
        }
    }
}

impl<T: FusedStream> Stream for StreamBroadcastLossy<T>
where
    T::Item: Clone,
{
    type Item = (u64, T::Item);

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let this = self.project();
        let mut lock = this.state.lock().unwrap();
        broadast_next(lock.deref_mut().as_mut(), cx, this.pos, *this.id)
    }
}
fn create_id() -> u64 {
    static ID_COUNTER: AtomicU64 = AtomicU64::new(0);
    ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}
fn broadast_next<T: FusedStream>(
    pinned: Pin<&mut StreamBroadcastState<T>>,
    cx: &mut std::task::Context<'_>,
    pos: &mut u64,
    id: u64,
) -> Poll<Option<(u64, T::Item)>>
where
    T::Item: Clone,
{
    match pinned.poll(cx, *pos, id) {
        Poll::Ready(Some((new_pos, x))) => {
            debug_assert!(new_pos > *pos, "Must always grow {} > {}", new_pos, *pos);
            let offset = new_pos - *pos - 1;
            *pos = new_pos;
            Poll::Ready(Some((offset, x)))
        }
        Poll::Ready(None) => {
            *pos += 1;
            Poll::Ready(None)
        }
        Poll::Pending => Poll::Pending,
    }
}

impl<T: FusedStream> FusedStream for StreamBroadcastLossy<T>
where
    T::Item: Clone,
{
    fn is_terminated(&self) -> bool {
        let lock = self.state.lock().unwrap();
        lock.stream.is_terminated() && self.pos >= lock.global_pos
    }
}

/// Created by [broadcast_lossless](StreamBroadcastExt::broadcast_lossless) or
/// [create_lossless](StreamBroadcastLossy::create_lossless). See
/// [broadcast_lossless](StreamBroadcastExt::broadcast_lossless) for the full documentation and
/// a demonstration of the backpressure using a bounded channel.
pub struct StreamBroadcastLossless<T: FusedStream> {
    id: u64,
    state: Arc<Mutex<Pin<Box<StreamBroadcastState<T>>>>>,
}

impl<T: FusedStream> std::fmt::Debug for StreamBroadcastLossless<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let lock = self.state.lock().unwrap();
        let pending = lock.global_pos.saturating_sub(lock.lossless_pos(self.id));
        f.debug_struct("StreamBroadcastLossless")
            .field("pending_messages", &pending)
            .field("strong_count", &Arc::strong_count(&self.state))
            .finish()
    }
}

impl<T: FusedStream> StreamBroadcastLossless<T>
where
    T::Item: Clone,
{
    /// # Panics
    /// Panics if `size` is 0.
    pub fn new(outer: T, size: usize) -> Self {
        let id = create_id();
        let state = Arc::new(Mutex::new(Box::pin(StreamBroadcastState::new(outer, size))));
        state.lock().unwrap().as_mut().register_lossless(id, 0);
        Self { id, state }
    }

    /// In contrast to clone, this method only shows new messages provided by the source stream
    pub fn re_subscribe(&self) -> Self {
        let id = create_id();
        let mut lock = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let global_pos = lock.global_pos;
        lock.as_mut().register_lossless(id, global_pos);
        Self {
            state: self.state.clone(),
            id,
        }
    }

    /// Creates a lossy subscriber on the same shared buffer, starting at this subscriber's
    /// current position.
    pub fn create_lossy(&self) -> StreamBroadcastLossy<T> {
        let lock = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let pos = lock.lossless_pos(self.id);
        StreamBroadcastLossy {
            state: self.state.clone(),
            id: create_id(),
            pos,
        }
    }
}

impl<T: FusedStream> Clone for StreamBroadcastLossless<T>
where
    T::Item: Clone,
{
    fn clone(&self) -> Self {
        let id = create_id();
        let mut lock = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let pos = lock.lossless_pos(self.id);
        lock.as_mut().register_lossless(id, pos);
        Self {
            state: self.state.clone(),
            id,
        }
    }
}

impl<T: FusedStream> Drop for StreamBroadcastLossless<T> {
    fn drop(&mut self) {
        let mut lock = self.state.lock().unwrap_or_else(|e| e.into_inner());
        lock.as_mut().unregister_lossless(self.id);
    }
}

impl<T: FusedStream> Stream for StreamBroadcastLossless<T>
where
    T::Item: Clone,
{
    type Item = T::Item;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let mut lock = this.state.lock().unwrap();
        let pos = lock.lossless_pos(this.id);
        broadcast_next_lossless(lock.deref_mut().as_mut(), cx, pos, this.id)
    }
}

impl<T: FusedStream> FusedStream for StreamBroadcastLossless<T>
where
    T::Item: Clone,
{
    fn is_terminated(&self) -> bool {
        let lock = self.state.lock().unwrap();
        lock.stream.is_terminated() && lock.lossless_pos(self.id) >= lock.global_pos
    }
}

fn broadcast_next_lossless<T: FusedStream>(
    mut state: Pin<&mut StreamBroadcastState<T>>,
    cx: &mut std::task::Context<'_>,
    pos: u64,
    id: u64,
) -> Poll<Option<T::Item>>
where
    T::Item: Clone,
{
    match state.as_mut().poll(cx, pos, id) {
        Poll::Ready(Some((new_pos, x))) => {
            debug_assert_eq!(
                new_pos,
                pos + 1,
                "a lossless subscriber must never skip items"
            );
            state.set_lossless_pos(id, new_pos);
            Poll::Ready(Some(x))
        }
        Poll::Ready(None) => {
            state.unregister_lossless(id);
            Poll::Ready(None)
        }
        Poll::Pending => Poll::Pending,
    }
}

#[pin_project]
struct StreamBroadcastState<T: FusedStream> {
    #[pin]
    stream: T,
    global_pos: u64,
    cap: u64,
    cache: Vec<T::Item>,
    /// subscriber id -> next pos to read, for every live lossless subscriber.
    lossless: BTreeMap<u64, u64>,
    wakable: Vec<(u64, std::task::Waker)>,
}

/// Whether the input stream may be polled again without overwriting an item that a lossless
/// subscriber still needs. An empty registry means "unconstrained" -- today's lossy behaviour.
fn can_advance(global_pos: u64, cap: u64, lossless: &BTreeMap<u64, u64>) -> bool {
    lossless
        .values()
        .min()
        .is_none_or(|&min| global_pos.saturating_sub(min) < cap)
}

/// Registers `waker` for `id`, replacing any waker already stored for it rather than
/// accumulating duplicates.
fn register_waker(wakable: &mut Vec<(u64, std::task::Waker)>, id: u64, waker: &std::task::Waker) {
    wakable.push((id, waker.clone()));
}

fn wake_all(wakable: &mut Vec<(u64, std::task::Waker)>, except: u64) {
    wakable.drain(..).for_each(|(k, w)| {
        if k != except {
            w.wake();
        }
    });
}

impl<T: FusedStream> StreamBroadcastState<T> {
    fn new(outer: T, size: usize) -> Self {
        let size = size.max(1);
        Self {
            stream: outer,
            cache: Vec::with_capacity(size), // Could be improved with  Box<[MaybeUninit<T::Item>]>
            cap: size as u64,
            global_pos: Default::default(),
            lossless: Default::default(),
            wakable: Default::default(),
        }
    }

    /// Registers a new lossless subscriber at `requested_pos`, clamped up to the oldest item
    /// still cached if it is already too far behind (those items cannot be recovered). Returns
    /// the clamped starting position.
    fn register_lossless(self: Pin<&mut Self>, id: u64, requested_pos: u64) -> u64 {
        let this = self.project();
        let pos = requested_pos.max(this.global_pos.saturating_sub(*this.cap));
        this.lossless.insert(id, pos);
        pos
    }

    /// The current read position of a live lossless subscriber.
    /// An id no longer in the registry has already read everything there is to read, so its
    /// position is exactly `global_pos` (nothing pending).
    fn lossless_pos(&self, id: u64) -> u64 {
        self.lossless.get(&id).copied().unwrap_or(self.global_pos)
    }

    fn set_lossless_pos(self: Pin<&mut Self>, id: u64, pos: u64) {
        let this = self.project();
        if let Some(entry) = this.lossless.get_mut(&id) {
            *entry = pos;
        }
        wake_all(this.wakable, id);
    }

    fn unregister_lossless(self: Pin<&mut Self>, id: u64) {
        let this = self.project();
        if this.lossless.remove(&id).is_some() {
            this.wakable.drain(..).for_each(|(_, w)| {
                w.wake();
            });
        }
    }
}

impl<T: FusedStream> StreamBroadcastState<T>
where
    T::Item: Clone,
{
    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        request_pos: u64,
        id: u64,
    ) -> Poll<Option<(u64, T::Item)>> {
        let this = self.project();
        if *this.global_pos > request_pos {
            let cap = *this.cap;
            let return_pos = if *this.global_pos - request_pos > cap {
                *this.global_pos - cap
            } else {
                request_pos
            };

            let result = this.cache[(return_pos % cap) as usize].clone();
            return Poll::Ready(Some((return_pos + 1, result)));
        }

        if !this.stream.as_ref().get_ref().is_terminated()
            && !can_advance(*this.global_pos, *this.cap, this.lossless)
        {
            register_waker(this.wakable, id, cx.waker());
            return Poll::Pending;
        }

        match this.stream.poll_next(cx) {
            Poll::Ready(Some(x)) => {
                wake_all(this.wakable, id);

                let cap = *this.cap;
                if (this.cache.len() as u64) < cap {
                    this.cache.push(x.clone());
                } else {
                    this.cache[(*this.global_pos % cap) as usize] = x.clone();
                }
                *this.global_pos += 1;
                let result = (*this.global_pos, x);
                Poll::Ready(Some(result))
            }
            Poll::Ready(None) => {
                wake_all(this.wakable, id);
                Poll::Ready(None)
            }
            Poll::Pending => {
                register_waker(this.wakable, id, cx.waker());
                Poll::Pending
            }
        }
    }
}
