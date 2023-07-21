#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

use futures::{
    stream::{Fuse, Stream},
    StreamExt,
};
use pin_project::pin_project;
use std::{
    ops::DerefMut,
    pin::{pin, Pin},
    sync::{atomic::AtomicU64, Arc, Mutex},
    task::Poll,
};

mod weak;

pub use weak::*;

pub trait StreamBroadcastExt: Stream + Sized {
    fn broadcast(self, size: usize) -> StreamBroadcast<Self>;
}

impl<T: Stream + Sized> StreamBroadcastExt for T
where
    T::Item: Clone,
{
    fn broadcast(self, size: usize) -> StreamBroadcast<Self> {
        StreamBroadcast::new(self, size)
    }
}

#[pin_project]
pub struct StreamBroadcast<T: Stream> {
    pos: u64,
    id: u64,
    state: Arc<Mutex<Pin<Box<StreamBroadcastState<T>>>>>,
}

impl<T: Stream> Clone for StreamBroadcast<T> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            id: create_id(),
            pos: self.state.lock().unwrap().global_pos,
        }
    }
}

impl<T: Stream> StreamBroadcast<T>
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

    pub fn weak(&self) -> WeakStreamBroadcast<T> {
        WeakStreamBroadcast::new(Arc::downgrade(&self.state), self.pos)
    }
}

impl<T: Stream> Stream for StreamBroadcast<T>
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
fn broadast_next<T: Stream>(
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

#[pin_project]
struct StreamBroadcastState<T: Stream> {
    #[pin]
    stream: Fuse<T>,
    global_pos: u64,
    cache: Vec<T::Item>,
    wakable: Vec<(u64, std::task::Waker)>,
}

impl<T: Stream> StreamBroadcastState<T>
where
    T::Item: Clone,
{
    fn new(outer: T, size: usize) -> Self {
        Self {
            stream: outer.fuse(),
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
