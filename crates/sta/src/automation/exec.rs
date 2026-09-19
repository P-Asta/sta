//! A tiny single-threaded executor on the CEF UI thread [owner: automation].
//!
//! Automation recipes are `async fn`s that await CDP results, timers and channel messages. Tasks
//! live in a `thread_local` map and are polled from posted UI tasks, never inside a CEF callback:
//! a waker posts a poll task (`task::post_ui_from_any_thread`), so waking from inside an observer
//! callback or from another thread is safe, and no borrow is ever held across a poll.
//!
//! Public API:
//! - `pub fn spawn(fut: impl Future<Output = ()> + 'static)`
//! - `pub fn oneshot<T>() -> (Sender<T>, Receiver<T>)` — `Receiver` resolves to `Option<T>`
//!   (`None` when the sender was dropped without a value)
//! - `pub fn sleep(ms: i64) -> Sleep`, `pub async fn timeout<F: Future>(ms, fut) -> Option<F::Output>`
//! - `pub fn clear()` — drops every task (teardown)

use crate::task;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, Wake, Waker};

type BoxedTask = Pin<Box<dyn Future<Output = ()>>>;

thread_local! {
    static TASKS: RefCell<HashMap<u64, BoxedTask>> = RefCell::new(HashMap::new());
    static NEXT_TASK: Cell<u64> = const { Cell::new(1) };
    /// Set while `clear` runs: wakes are ignored.
    static CLOSED: Cell<bool> = const { Cell::new(false) };
}

struct TaskWaker {
    id: u64,
    scheduled: AtomicBool,
}

impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if self.scheduled.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = self.clone();
        task::post_ui_from_any_thread(move || {
            me.scheduled.store(false, Ordering::SeqCst);
            poll_task(me.id, &me);
        });
    }
}

/// Starts `fut` on the UI thread (first poll in a posted task).
pub fn spawn(fut: impl Future<Output = ()> + 'static) {
    if CLOSED.get() {
        return;
    }
    let id = NEXT_TASK.replace(NEXT_TASK.get() + 1);
    TASKS.with(|t| t.borrow_mut().insert(id, Box::pin(fut)));
    Waker::from(Arc::new(TaskWaker { id, scheduled: AtomicBool::new(false) })).wake();
}

fn poll_task(id: u64, handle: &Arc<TaskWaker>) {
    if CLOSED.get() {
        return;
    }
    // The future leaves the map while it is polled: a nested spawn or wake never sees a borrow.
    let Some(mut fut) = TASKS.with(|t| t.borrow_mut().remove(&id)) else { return };
    let waker = Waker::from(handle.clone());
    let mut cx = Context::from_waker(&waker);
    let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fut.as_mut().poll(&mut cx)));
    match done {
        Ok(Poll::Pending) => {
            TASKS.with(|t| t.borrow_mut().insert(id, fut));
        }
        Ok(Poll::Ready(())) => drop(fut),
        Err(_) => {
            log_error!("automation task {id} panicked; dropped");
            drop(fut);
        }
    }
}

/// Number of live tasks (debug snapshot).
pub fn task_count() -> usize {
    TASKS.with(|t| t.borrow().len())
}

/// Drops every task (before `cef::shutdown`).
pub fn clear() {
    CLOSED.set(true);
    let tasks = TASKS.with(|t| std::mem::take(&mut *t.borrow_mut()));
    drop(tasks);
}

// ----------------------------------------------------------------------------------- oneshot

struct Slot<T> {
    value: Option<T>,
    closed: bool,
    waker: Option<Waker>,
}

pub struct Sender<T> {
    slot: Rc<RefCell<Slot<T>>>,
}

pub struct Receiver<T> {
    slot: Rc<RefCell<Slot<T>>>,
}

/// A single-value channel for UI-thread code.
pub fn oneshot<T>() -> (Sender<T>, Receiver<T>) {
    let slot = Rc::new(RefCell::new(Slot { value: None, closed: false, waker: None }));
    (Sender { slot: slot.clone() }, Receiver { slot })
}

impl<T> Sender<T> {
    pub fn send(self, value: T) {
        let waker = {
            let mut s = self.slot.borrow_mut();
            s.value = Some(value);
            s.waker.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
        // `Drop` runs next and finds the value delivered.
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let waker = {
            let Ok(mut s) = self.slot.try_borrow_mut() else { return };
            s.closed = true;
            s.waker.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
    }
}

impl<T> Future for Receiver<T> {
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
        let mut s = self.slot.borrow_mut();
        if let Some(v) = s.value.take() {
            return Poll::Ready(Some(v));
        }
        if s.closed {
            return Poll::Ready(None);
        }
        s.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

// ----------------------------------------------------------------------------------- timers

pub struct Sleep {
    rx: Receiver<()>,
}

/// Resolves after `ms` milliseconds (a delayed UI task).
pub fn sleep(ms: i64) -> Sleep {
    let (tx, rx) = oneshot();
    task::post_ui_delayed(ms.max(0), move || tx.send(()));
    Sleep { rx }
}

impl Future for Sleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        match Pin::new(&mut self.rx).poll(cx) {
            Poll::Ready(_) => Poll::Ready(()),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// `Some(output)` if `fut` finishes within `ms`, else `None` (the future is dropped).
pub async fn timeout<F: Future>(ms: i64, fut: F) -> Option<F::Output> {
    let mut fut = std::pin::pin!(fut);
    let mut timer = std::pin::pin!(sleep(ms));
    std::future::poll_fn(move |cx| {
        if let Poll::Ready(v) = fut.as_mut().poll(cx) {
            return Poll::Ready(Some(v));
        }
        match timer.as_mut().poll(cx) {
            Poll::Ready(()) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    })
    .await
}
