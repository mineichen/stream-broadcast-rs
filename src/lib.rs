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
    sync::{Arc, Mutex},
    task::Poll,
};

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
    state: Arc<Mutex<Pin<Box<StreamBroadcastState<T>>>>>,
}

impl<T: Stream> Clone for StreamBroadcast<T> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
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
            pos: 0,
        }
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
        let pinned = lock.deref_mut().as_mut();

        match pinned.poll(cx, *this.pos) {
            Poll::Ready(Some((new_pos, x))) => {
                debug_assert!(
                    new_pos > *this.pos,
                    "Must always grow {} > {}",
                    new_pos,
                    *this.pos
                );
                let offset = new_pos - *this.pos - 1;
                *this.pos = new_pos;
                Poll::Ready(Some((offset, x)))
            }
            Poll::Ready(None) => {
                *this.pos += 1;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

#[pin_project]
struct StreamBroadcastState<T: Stream> {
    #[pin]
    stream: Fuse<T>,
    global_pos: u64,
    cache: Vec<T::Item>,
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
        }
    }
    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        request_pos: u64,
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
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {}
