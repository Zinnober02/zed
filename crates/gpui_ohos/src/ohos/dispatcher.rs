use std::{
    cmp::Ordering,
    collections::{BinaryHeap, VecDeque},
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant},
};

use crate::{PlatformDispatcher, Priority, PriorityQueueSender, RunnableVariant};

struct TimerAfter {
    when: Instant,
    runnable: RunnableVariant,
}

impl Ord for TimerAfter {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse for min-heap behavior.
        other.when.cmp(&self.when)
    }
}

impl PartialOrd for TimerAfter {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for TimerAfter {
    fn eq(&self, other: &Self) -> bool {
        self.when.eq(&other.when)
    }
}

impl Eq for TimerAfter {}

type Waker = Box<dyn Fn() + Send + Sync + 'static>;

/// Main-thread task dispatcher for OHOS.
///
/// OHOS owns the UI thread, so we never block it. Foreground tasks are queued
/// and drained from OhosPlatform::tick, which the ArkTS host calls once per
/// frame. Timers run on a dedicated thread that pushes due runnables into a
/// ready queue and (optionally) nudges the host through the waker.
pub(crate) struct OhosDispatcher {
    main_thread_id: thread::ThreadId,
    main_sender: PriorityQueueSender<RunnableVariant>,
    timer_queue: Arc<(Mutex<BinaryHeap<TimerAfter>>, Condvar)>,
    ready_timers: Arc<Mutex<VecDeque<RunnableVariant>>>,
    waker: Arc<Mutex<Option<Waker>>>,
    _timer_thread: thread::JoinHandle<()>,
}

impl OhosDispatcher {
    pub(crate) fn new(main_sender: PriorityQueueSender<RunnableVariant>) -> Self {
        let timer_queue: Arc<(Mutex<BinaryHeap<TimerAfter>>, Condvar)> =
            Arc::new((Mutex::new(BinaryHeap::new()), Condvar::new()));
        let ready_timers = Arc::new(Mutex::new(VecDeque::new()));
        let waker: Arc<Mutex<Option<Waker>>> = Arc::new(Mutex::new(None));

        let timer_queue_thread = timer_queue.clone();
        let ready_timers_thread = ready_timers.clone();
        let waker_thread = waker.clone();
        let timer_thread = thread::Builder::new()
            .name("OhosTimer".to_owned())
            .spawn(move || {
                loop {
                    let (lock, cvar) = &*timer_queue_thread;
                    let mut heap = lock.lock().unwrap();

                    loop {
                        if let Some(next) = heap.peek() {
                            let now = Instant::now();
                            if next.when <= now {
                                break;
                            }
                            let timeout = next.when.saturating_duration_since(now);
                            let (new_heap, _) = cvar.wait_timeout(heap, timeout).unwrap();
                            heap = new_heap;
                        } else {
                            heap = cvar.wait(heap).unwrap();
                        }
                    }

                    let now = Instant::now();
                    let mut due = VecDeque::new();
                    while heap.peek().is_some_and(|next| next.when <= now) {
                        due.push_back(heap.pop().expect("due timer entry exists").runnable);
                    }
                    drop(heap);

                    let queued_any = !due.is_empty();
                    ready_timers_thread.lock().unwrap().append(&mut due);

                    if queued_any {
                        if let Some(waker) = waker_thread.lock().unwrap().as_ref() {
                            waker();
                        }
                    }
                }
            })
            .expect("Failed to start OHOS timer thread");

        Self {
            main_thread_id: thread::current().id(),
            main_sender,
            timer_queue,
            ready_timers,
            waker,
            _timer_thread: timer_thread,
        }
    }

    /// Install a host waker that nudges the ArkTS frame loop.
    #[allow(dead_code)]
    pub(crate) fn set_waker(&self, waker: Waker) {
        *self.waker.lock().unwrap() = Some(waker);
    }

    /// Run timers that are already due. Called from the main thread.
    pub(crate) fn run_due_timers(&self) {
        let due = std::mem::take(&mut *self.ready_timers.lock().unwrap());
        for runnable in due {
            runnable.run();
        }
    }
}

impl PlatformDispatcher for OhosDispatcher {
    fn is_main_thread(&self) -> bool {
        thread::current().id() == self.main_thread_id
    }

    fn dispatch(&self, runnable: RunnableVariant, _priority: Priority) {
        // Background work runs on its own thread.
        thread::spawn(move || runnable.run());
    }

    fn dispatch_on_main_thread(&self, runnable: RunnableVariant, priority: Priority) {
        match self.main_sender.send(priority, runnable) {
            Ok(_) => {}
            Err(runnable) => {
                // The receiver is gone (shutdown). Runnable may be !Send, so we
                // cannot drop it here; the process is exiting anyway.
                std::mem::forget(runnable);
            }
        }
    }

    fn dispatch_after(&self, duration: Duration, runnable: RunnableVariant) {
        let (lock, cvar) = &*self.timer_queue;
        let mut heap = lock.lock().unwrap();
        heap.push(TimerAfter {
            when: Instant::now() + duration,
            runnable,
        });
        cvar.notify_one();
    }

    fn spawn_realtime(&self, f: Box<dyn FnOnce() + Send>) {
        thread::spawn(f);
    }

    fn now(&self) -> Instant {
        Instant::now()
    }
}
