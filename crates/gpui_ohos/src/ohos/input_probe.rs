//! 输入到画面的后两段延迟：排队等平台线程，以及从平台线程接手到 present 返回。
//!
//! 宿主那侧量的是"设备采样 → 应用收到"（inkstone 的 input_probe.cpp）。这一段
//! 应用改不了，也是唯一可能大的；而它后面还有两段属于我们自己：投进队列到平台线程
//! 真正接手，以及接手到这一帧交出去。三段首尾相接，加起来就是一次移动从上屏前的
//! 全部耗时，缺任何一段都会把问题归错地方。
//!
//! 两端读同一个时钟：CLOCK_MONOTONIC。宿主的 steady_clock 在 OHOS 上就是它，设备
//! 侧的时间戳来自 MMI，也落在同一基准。基准不同源不会算出一个像样的数，而是算出一
//! 个十几位数；这类样本丢掉并计数，不让它污染分位——"全部被丢"本身就是结论。

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Instant;

/// 最近一条输入在设备侧发生的时刻。
static LAST_DEVICE_NS: AtomicI64 = AtomicI64::new(0);
/// 有输入还没被某一帧带走。
static PENDING: AtomicBool = AtomicBool::new(false);

#[repr(C)]
struct Timespec {
    seconds: i64,
    nanos: i64,
}

/// Linux 与 OHOS 上同一个值；这里不引 libc，因为整段只需要这一个调用。
const CLOCK_MONOTONIC: i32 = 1;

unsafe extern "C" {
    fn clock_gettime(clock: i32, time: *mut Timespec) -> i32;
}

pub(crate) fn now_ns() -> i64 {
    let mut now = Timespec {
        seconds: 0,
        nanos: 0,
    };
    // SAFETY: clock_gettime 只往这块由我们持有、对齐正确的 timespec 里写。
    let code = unsafe { clock_gettime(CLOCK_MONOTONIC, &mut now) };
    if code != 0 {
        return 0;
    }
    now.seconds * 1_000_000_000 + now.nanos
}

fn micros(nanos: i64) -> i32 {
    let value = nanos / 1000;
    if value > i32::MAX as i64 {
        i32::MAX
    } else {
        value as i32
    }
}

struct Window {
    started: Instant,
    /// 投进队列到平台线程接手。这一项就是"我们自己的批"：它大，说明事件在队列里
    /// 等平台线程画完上一帧，而不是系统没交给我们。
    queue: Vec<i32>,
    /// 设备采样到 present 返回。上屏还要再等扫描出，应用侧看不见，只能另算。
    present: Vec<i32>,
    queue_max: i32,
    present_max: i32,
    /// 时刻算不出来（负值或零）的样本数。
    dropped: u32,
}

thread_local! {
    static WINDOW: RefCell<Window> = RefCell::new(Window {
        started: Instant::now(),
        queue: Vec::new(),
        present: Vec::new(),
        queue_max: 0,
        present_max: 0,
        dropped: 0,
    });
}

/// 输入到了（宿主那边刚收到）。可以不在平台线程调用：这里只写两个原子量。
pub(crate) fn note_input(device_ns: i64) {
    if device_ns <= 0 {
        return;
    }
    LAST_DEVICE_NS.store(device_ns, Ordering::Relaxed);
    PENDING.store(true, Ordering::Relaxed);
}

/// 平台线程接手的时刻。`arrival_ns` 是宿主收到这条事件的时刻，同一个时钟。
pub(crate) fn note_dispatch(arrival_ns: i64) {
    if arrival_ns <= 0 {
        return;
    }
    let wait = now_ns() - arrival_ns;
    if wait < 0 {
        WINDOW.with(|window| window.borrow_mut().dropped += 1);
        return;
    }
    let value = micros(wait);
    WINDOW.with(|window| {
        let mut window = window.borrow_mut();
        if value > window.queue_max {
            window.queue_max = value;
        }
        window.queue.push(value);
    });
}

/// 一帧交出去了。只统计"这一帧之前确实有新输入"的情况，否则空闲帧会把分位冲淡。
pub(crate) fn note_present() {
    if !PENDING.swap(false, Ordering::Relaxed) {
        return;
    }
    let device = LAST_DEVICE_NS.load(Ordering::Relaxed);
    if device <= 0 {
        return;
    }
    let age = now_ns() - device;
    if age < 0 {
        WINDOW.with(|window| window.borrow_mut().dropped += 1);
        return;
    }
    let value = micros(age);
    WINDOW.with(|window| {
        let mut window = window.borrow_mut();
        if value > window.present_max {
            window.present_max = value;
        }
        window.present.push(value);
    });
}

/// 第 percent 分位。样本为空给 0。
fn percentile(samples: &[i32], percent: usize) -> i32 {
    if samples.is_empty() {
        return 0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    sorted[(sorted.len() * percent / 100).min(sorted.len() - 1)]
}

/// 攒够一秒打一行，然后把窗口清空。没有样本时什么也不打。
pub(crate) fn report_if_due() {
    WINDOW.with(|window| {
        let mut window = window.borrow_mut();
        if window.started.elapsed().as_millis() < 1000 {
            return;
        }
        let queue_len = window.queue.len();
        let present_len = window.present.len();
        if queue_len == 0 && present_len == 0 && window.dropped == 0 {
            window.started = Instant::now();
            return;
        }
        let line = format!(
            "[input] queue n={} p50={} p90={} max={}us | present n={} p50={} p90={} max={}us dropped={}",
            queue_len,
            percentile(&window.queue, 50),
            percentile(&window.queue, 90),
            window.queue_max,
            present_len,
            percentile(&window.present, 50),
            percentile(&window.present, 90),
            window.present_max,
            window.dropped
        );
        window.queue.clear();
        window.present.clear();
        window.queue_max = 0;
        window.present_max = 0;
        window.dropped = 0;
        window.started = Instant::now();
        drop(window);
        crate::log_line(&line);
    });
}
