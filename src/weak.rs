use futures::stream::{FusedStream, Stream};
use pin_project::pin_project;
use std::{
    ops::DerefMut,
    pin::{pin, Pin},
    sync::{Mutex, Weak},
    task::Poll,
};

use super::{broadast_next, create_id, StreamBroadcast, StreamBroadcastState};

/// Created by [weak](crate::StreamBroadcast::weak)
#[pin_project]
pub struct WeakStreamBroadcast<T: FusedStream> {
    pos: u64,
    id: u64,
    state: Weak<Mutex<Pin<Box<StreamBroadcastState<T>>>>>,
}

impl<T: FusedStream> WeakStreamBroadcast<T> {
    pub(crate) fn new(state: Weak<Mutex<Pin<Box<StreamBroadcastState<T>>>>>, pos: u64) -> Self {
        Self {
            pos,
            id: create_id(),
            state,
        }
    }

    /// Upgrades a WeakBroadcast to a StreamBroadcast, whose existence keeps the stream running
    pub fn upgrade(&self) -> Option<StreamBroadcast<T>> {
        let state = self.state.upgrade()?;
        Some(StreamBroadcast {
            pos: self.pos,
            id: create_id(),
            state,
        })
    }
}

impl<T: FusedStream> Clone for WeakStreamBroadcast<T> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            id: create_id(),
            pos: self
                .state
                .upgrade()
                .map(|s| s.lock().unwrap().global_pos)
                .unwrap_or(0), // State is never polled anyways
        }
    }
}

impl<T: FusedStream> Stream for WeakStreamBroadcast<T>
where
    T::Item: Clone,
{
    type Item = (u64, T::Item);

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let this = self.project();
        let Some(state) = this.state.upgrade() else {
            return Poll::Ready(None);
        };
        let mut lock = state.lock().unwrap();
        broadast_next(lock.deref_mut().as_mut(), cx, this.pos, *this.id)
    }
}

impl<T: FusedStream> FusedStream for WeakStreamBroadcast<T>
where
    T::Item: Clone,
{
    fn is_terminated(&self) -> bool {
        if let Some(u) = self.state.upgrade() {
            u.lock().unwrap().stream.is_terminated()
        } else {
            true
        }
    }
}
