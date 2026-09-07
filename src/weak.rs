use futures::stream::{FusedStream, Stream};
use pin_project::pin_project;
use std::{
    ops::DerefMut,
    pin::Pin,
    sync::{Mutex, Weak},
    task::Poll,
};

use super::{
    broadast_next, create_id, StreamBroadcastLossless, StreamBroadcastLossy, StreamBroadcastState,
};

/// Created by [downgrade](crate::StreamBroadcastLossy::downgrade)
#[pin_project]
pub struct WeakStreamBroadcast<T: FusedStream> {
    pos: u64,
    id: u64,
    state: Weak<Mutex<Pin<Box<StreamBroadcastState<T>>>>>,
}

impl<T: FusedStream> std::fmt::Debug for WeakStreamBroadcast<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pending = self
            .state
            .upgrade()
            .map(|x| x.lock().unwrap().global_pos.saturating_sub(self.pos))
            .unwrap_or(0);
        f.debug_struct("WeakStreamBroadcast")
            .field("pending_messages", &pending)
            .field("strong_count", &self.state.strong_count())
            .finish()
    }
}

impl<T: FusedStream> WeakStreamBroadcast<T> {
    pub(crate) fn new(state: Weak<Mutex<Pin<Box<StreamBroadcastState<T>>>>>, pos: u64) -> Self {
        Self {
            pos,
            id: create_id(),
            state,
        }
    }

    /// Upgrades a WeakBroadcast to a StreamBroadcastLossy, whose existence keeps the stream running
    #[deprecated(since = "0.3.1", note = "use `create_lossy`")]
    pub fn upgrade(&self) -> Option<StreamBroadcastLossy<T>> {
        self.create_lossy()
    }

    /// Creates a lossy subscriber on the same shared buffer, if the underlying broadcast is
    /// still alive. Its existence keeps the stream running.
    pub fn create_lossy(&self) -> Option<StreamBroadcastLossy<T>> {
        let state = self.state.upgrade()?;
        Some(StreamBroadcastLossy {
            pos: self.pos,
            id: create_id(),
            state,
        })
    }

    /// In contrast to clone, this method only shows new messages provided by the source stream
    pub fn re_subscribe(&self) -> Self {
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

impl<T: FusedStream> WeakStreamBroadcast<T>
where
    T::Item: Clone,
{
    /// Creates a lossless subscriber on the same shared buffer, if the underlying broadcast is
    /// still alive. Its existence keeps the stream running, and while it lags behind it stalls
    /// every other subscriber sharing the buffer.
    pub fn create_lossless(&self) -> Option<StreamBroadcastLossless<T>> {
        let state = self.state.upgrade()?;
        let id = create_id();
        let mut lock = state.lock().unwrap_or_else(|e| e.into_inner());
        lock.as_mut().register_lossless(id, self.pos);
        drop(lock);
        Some(StreamBroadcastLossless { id, state })
    }
}

impl<T: FusedStream> Clone for WeakStreamBroadcast<T> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            id: create_id(),
            pos: self.pos,
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
            let lock = u.lock().unwrap();
            lock.stream.is_terminated() && self.pos >= lock.global_pos
        } else {
            true
        }
    }
}
