// SPDX-License-Identifier: AGPL-3.0-or-later
//! Access to state owned by another thread.
//!
//! The application state (the session, and later the engine around it) lives
//! on exactly one thread: the GUI main loop, or a dedicated thread in
//! headless mode. Other threads (the MCP server, the disk I/O thread) send it
//! closures through a [`Handle`] and await the result.

use std::fmt;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use tokio::sync::oneshot;

/// Work to run on the owning thread.
pub type Job<T> = Box<dyn FnOnce(&mut T) + Send>;

/// The state's owner has shut down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the session has shut down")]
pub struct Closed;

/// A cloneable, thread-safe way to run code against state owned by another
/// thread.
pub struct Handle<T> {
    dispatch: Arc<dyn Fn(Job<T>) -> Result<(), Closed> + Send + Sync>,
}

impl<T> Clone for Handle<T> {
    fn clone(&self) -> Self {
        Self {
            dispatch: Arc::clone(&self.dispatch),
        }
    }
}

impl<T> fmt::Debug for Handle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Handle").finish_non_exhaustive()
    }
}

impl<T: 'static> Handle<T> {
    /// Builds a handle from any way of getting jobs to the owner, e.g. an
    /// event-loop channel.
    pub fn from_dispatch(
        dispatch: impl Fn(Job<T>) -> Result<(), Closed> + Send + Sync + 'static,
    ) -> Self {
        Self {
            dispatch: Arc::new(dispatch),
        }
    }

    /// Moves `state` onto a new thread that runs jobs until every handle is
    /// dropped. Used in headless mode, where there is no main loop.
    pub fn spawn_thread(state: T) -> (Self, thread::JoinHandle<T>)
    where
        T: Send,
    {
        Self::spawn_thread_with_tick(state, Duration::MAX, |_| {})
    }

    /// Like [`spawn_thread`](Self::spawn_thread), and also calls `tick` at
    /// least every `interval` while idle, for housekeeping such as delivering
    /// deferred engine updates.
    pub fn spawn_thread_with_tick(
        mut state: T,
        interval: Duration,
        mut tick: impl FnMut(&mut T) + Send + 'static,
    ) -> (Self, thread::JoinHandle<T>)
    where
        T: Send,
    {
        let (tx, rx) = mpsc::channel::<Job<T>>();
        let join = thread::Builder::new()
            .name("dk-session".into())
            .spawn(move || {
                loop {
                    match rx.recv_timeout(interval) {
                        Ok(job) => job(&mut state),
                        Err(mpsc::RecvTimeoutError::Timeout) => tick(&mut state),
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                state
            })
            .expect("failed to spawn session thread");
        let handle = Self::from_dispatch(move |job| tx.send(job).map_err(|_| Closed));
        (handle, join)
    }

    /// Like [`spawn_thread_with_tick`](Self::spawn_thread_with_tick), but
    /// builds the state on the new thread, for state that can't move between
    /// threads (plugin instances must stay on the thread that made them).
    /// Returns `init`'s error if it fails. The state is dropped on its thread
    /// once every handle is gone.
    pub fn spawn_thread_with<E>(
        init: impl FnOnce() -> Result<T, E> + Send + 'static,
        interval: Duration,
        mut tick: impl FnMut(&mut T) + Send + 'static,
    ) -> Result<(Self, thread::JoinHandle<()>), E>
    where
        E: Send + 'static,
    {
        let (tx, rx) = mpsc::channel::<Job<T>>();
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), E>>(1);
        let join = thread::Builder::new()
            .name("dk-session".into())
            .spawn(move || {
                let mut state = match init() {
                    Ok(state) => {
                        let _ = ready_tx.send(Ok(()));
                        state
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                loop {
                    match rx.recv_timeout(interval) {
                        Ok(job) => job(&mut state),
                        Err(mpsc::RecvTimeoutError::Timeout) => tick(&mut state),
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .expect("failed to spawn session thread");
        match ready_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let _ = join.join();
                return Err(e);
            }
            Err(_) => panic!("the session thread died while starting"),
        }
        let handle = Self::from_dispatch(move |job| tx.send(job).map_err(|_| Closed));
        Ok((handle, join))
    }

    /// Queues `f` to run on the owning thread, without waiting for it.
    pub fn send(&self, f: impl FnOnce(&mut T) + Send + 'static) -> Result<(), Closed> {
        (self.dispatch)(Box::new(f))
    }

    /// Runs `f` on the owning thread and returns its result.
    pub async fn with<R, F>(&self, f: F) -> Result<R, Closed>
    where
        R: Send + 'static,
        F: FnOnce(&mut T) -> R + Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        (self.dispatch)(Box::new(move |state| {
            // The caller may have stopped waiting; that's fine.
            let _ = tx.send(f(state));
        }))?;
        rx.await.map_err(|_| Closed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::AddTrack;
    use crate::session::Session;

    #[tokio::test]
    async fn runs_jobs_on_session_thread() {
        let (handle, join) = Handle::spawn_thread(Session::default());
        let outcome = handle
            .with(|s| s.execute(AddTrack::default().into()))
            .await
            .unwrap()
            .unwrap();
        assert!(outcome.track.is_some());
        let thread_name = handle
            .with(|_| thread::current().name().map(str::to_owned))
            .await
            .unwrap();
        assert_eq!(thread_name.as_deref(), Some("dk-session"));

        drop(handle);
        let session = join.join().unwrap();
        assert_eq!(session.project().tracks.len(), 1);
    }
}
