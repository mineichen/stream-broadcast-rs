#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

use futures::stream::{FusedStream, Stream};
use pin_project::pin_project;
use std::{
    ops::DerefMut,
    pin::{pin, Pin},
    sync::{atomic::AtomicU64, Arc, Mutex},
    task::Poll,
};

mod weak;

pub use weak::*;

pub trait StreamBroadcastExt: FusedStream + Sized {
    fn broadcast(self, size: usize) -> StreamBroadcast<Self>;
}

impl<T: FusedStream + Sized> StreamBroadcastExt for T
where
    T::Item: Clone,
{
    fn broadcast(self, size: usize) -> StreamBroadcast<Self> {
        StreamBroadcast::new(self, size)
    }
}

#[pin_project]
pub struct StreamBroadcast<T: FusedStream> {
    pos: u64,
    id: u64,
    state: Arc<Mutex<Pin<Box<StreamBroadcastState<T>>>>>,
}

impl<T: FusedStream> Clone for StreamBroadcast<T> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            id: create_id(),
            pos: self.state.lock().unwrap().global_pos,
        }
    }
}

impl<T: FusedStream> StreamBroadcast<T>
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

    /// Creates a weak broadcast which terminates its stream, if all 'strong' [StreamBroadcast] went out of scope
    ///
    /// ```
    /// # #[tokio::main]
    /// # async fn main() {
    /// use futures::StreamExt;
    /// use stream_broadcast::StreamBroadcastExt;
    ///
    /// let stream = futures::stream::iter(0..).fuse().broadcast(5);
    /// let mut weak = std::pin::pin!(stream.weak());
    /// assert_eq!(Some((0, 0)), weak.next().await);
    /// drop(stream);
    /// assert_eq!(None, weak.next().await);
    /// # }
    /// ```
    pub fn downgrade(&self) -> WeakStreamBroadcast<T> {
        WeakStreamBroadcast::new(Arc::downgrade(&self.state), self.pos)
    }

    #[deprecated(since = "0.2.2", note = "please use `downgrade` instead")]
    pub fn weak(&self) -> WeakStreamBroadcast<T> {
        WeakStreamBroadcast::new(Arc::downgrade(&self.state), self.pos)
    }
}

impl<T: FusedStream> Stream for StreamBroadcast<T>
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

impl<T: FusedStream> FusedStream for StreamBroadcast<T>
where
    T::Item: Clone,
{
    fn is_terminated(&self) -> bool {
        self.state.lock().unwrap().stream.is_terminated()
    }
}

#[pin_project]
struct StreamBroadcastState<T: FusedStream> {
    #[pin]
    stream: T,
    global_pos: u64,
    cache: Vec<T::Item>,
    wakable: Vec<(u64, std::task::Waker)>,
}

impl<T: FusedStream> StreamBroadcastState<T>
where
    T::Item: Clone,
{
    fn new(outer: T, size: usize) -> Self {
        Self {
            stream: outer,
            cache: Vec::with_capacity(size), // Could be improved with  Box<[MaybeUninit<T::Item>]>
            global_pos: Default::default(),
            wakable: Default::default(),
        }
    }
    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        request_pos: u64,
        id: u64,
    ) -> Poll<Option<(u64, T::Item)>> {
        let this = self.project();
        if *this.global_pos > request_pos {
            let cap = this.cache.capacity();
            let return_pos = if *this.global_pos - request_pos > cap as u64 {
                *this.global_pos - cap as u64
            } else {
                request_pos
            };

            let result = this.cache[(return_pos % cap as u64) as usize].clone();
            return Poll::Ready(Some((return_pos + 1, result)));
        }

        match this.stream.poll_next(cx) {
            Poll::Ready(Some(x)) => {
                this.wakable.drain(..).for_each(|(k, w)| {
                    if k != id {
                        w.wake();
                    }
                });

                let cap = this.cache.capacity();
                if this.cache.len() < cap {
                    this.cache.push(x.clone());
                } else {
                    this.cache[(*this.global_pos % cap as u64) as usize] = x.clone();
                }
                *this.global_pos += 1;
                let result = (*this.global_pos, x);
                Poll::Ready(Some(result))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => {
                this.wakable.push((id, cx.waker().clone()));
                Poll::Pending
            }
        }
    }
}
