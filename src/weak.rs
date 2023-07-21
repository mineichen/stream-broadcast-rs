use futures::stream::Stream;
use pin_project::pin_project;
use std::{
    ops::DerefMut,
    pin::{pin, Pin},
    sync::{Mutex, Weak},
    task::Poll,
};

use crate::create_id;

use super::{broadast_next, StreamBroadcastState};

#[pin_project]
pub struct WeakStreamBroadcast<T: Stream> {
    pos: u64,
    id: u64,
    state: Weak<Mutex<Pin<Box<StreamBroadcastState<T>>>>>,
}

impl<T: Stream> WeakStreamBroadcast<T> {
    pub(crate) fn new(state: Weak<Mutex<Pin<Box<StreamBroadcastState<T>>>>>, pos: u64) -> Self {
        Self {
            pos,
            id: create_id(),
            state,
        }
    }
}

impl<T: Stream> Clone for WeakStreamBroadcast<T> {
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

impl<T: Stream> Stream for WeakStreamBroadcast<T>
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
