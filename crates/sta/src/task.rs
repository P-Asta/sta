//! Posting closures to the CEF UI thread [skeleton, frozen].
//!
//! Responsibility: the only way shell code defers work. Use it to escape re-entrancy (inside CEF
//! callbacks, IPC handlers holding the router lock, `do_close`, ...) and for timers.
//!
//! Public API:
//! - `pub fn post_ui(f: impl FnOnce() + 'static)` — UI thread → UI thread (closure may hold `Rc`).
//! - `pub fn post_ui_delayed(delay_ms: i64, f: impl FnOnce() + 'static)` — same, after a delay.
//! - `pub fn post_ui_from_any_thread(f: impl FnOnce() + Send + 'static)` — from worker threads.
//!
//! `post_ui`/`post_ui_delayed` must be called on the UI thread (debug-asserted): the closure is
//! `!Send`, and CEF destroys a task that fails to post on the *calling* thread.

use cef::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

type LocalJob = Rc<RefCell<Option<Box<dyn FnOnce()>>>>;
#[allow(dead_code)] // stage-2 API
type SendJob = Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>;

wrap_task! {
    struct LocalTask {
        job: LocalJob,
    }

    impl Task {
        fn execute(&self) {
            // Take the closure first so the RefCell borrow ends before it runs.
            let f = self.job.borrow_mut().take();
            if let Some(f) = f {
                f();
            }
        }
    }
}

wrap_task! {
    struct SendTask {
        job: SendJob,
    }

    impl Task {
        fn execute(&self) {
            let f = self.job.lock().ok().and_then(|mut g| g.take());
            if let Some(f) = f {
                f();
            }
        }
    }
}

/// Runs `f` on the UI thread as a new task (never synchronously).
pub fn post_ui(f: impl FnOnce() + 'static) {
    debug_assert!(currently_on(ThreadId::UI) != 0, "post_ui called off the UI thread");
    let mut task = LocalTask::new(Rc::new(RefCell::new(Some(Box::new(f)))));
    post_task(ThreadId::UI, Some(&mut task));
}

/// Runs `f` on the UI thread after `delay_ms` milliseconds (0 = next task).
pub fn post_ui_delayed(delay_ms: i64, f: impl FnOnce() + 'static) {
    debug_assert!(currently_on(ThreadId::UI) != 0, "post_ui_delayed called off the UI thread");
    let mut task = LocalTask::new(Rc::new(RefCell::new(Some(Box::new(f)))));
    if delay_ms <= 0 {
        post_task(ThreadId::UI, Some(&mut task));
    } else {
        post_delayed_task(ThreadId::UI, Some(&mut task), delay_ms);
    }
}

/// Runs `f` on the UI thread; callable from any browser-process thread.
#[allow(dead_code)] // stage-2 API
pub fn post_ui_from_any_thread(f: impl FnOnce() + Send + 'static) {
    let mut task = SendTask::new(Arc::new(Mutex::new(Some(Box::new(f)))));
    post_task(ThreadId::UI, Some(&mut task));
}
