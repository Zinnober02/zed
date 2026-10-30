//! 帧间隔分解：一帧的 66ms 到底是谁占的。
//!
//! 输入侧的探针（`input_probe`）只能证明"作业在队列里等了多久"，证明不了平台线程当时在
//! 干什么。关掉补喂、拖拽进行中的那一秒里，`queue p50` 是 38~59ms，而一帧的 vk 段只要
//! 0.8~11ms、`frame cpu` 也是 1~11ms——两者加起来解释不了 66ms 一帧。差的那 30~40ms
//! 发生在"上一帧交出去"到"下一帧开始"之间，那里一行日志都没有。
//!
//! 这里把平台线程的每一轮循环切成四段，再把"画这一帧"拆到可以指认的程度，按秒打一行：
//!
//! ```text
//! [frame] it=15 drew=14 want=120 jobs=15 moves=15 merged=60 | gap=66000/66100/66300
//!         wait=52000/52100/52200 jobt=4000/4100/4200 job1=8000/8100/8200
//!         frame=9000/9500/9800 rest=100/120/140 | gate=200 ime=300 tick=8500 draw=8000
//!         paint=5000/5100/5200 build=900 vk=1800 | acq=400 fence=60 up=300 rec=600 sub=200 pres=240 us
//!         | text n=62 us=18400 max=800 adv=0 ras=0 | at=1793000000
//! ```
//!
//! - `gap` 两次循环开始之间的间隔，倒数就是真实帧率；
//! - `wait` 阻塞在平台队列上等活的时间（没人要帧，线程就睡在这里）；
//! - `jobt` 一轮里所有活加起来的时间，`job1` 是单件活自己的时间——两者一比就是这一轮
//!   平均派发了几条；
//! - `moves` 派发出去的移动条数，`merged` 其中被后来的移动替掉、因此没有派发的条数；
//! - `frame` 画这一帧，内部按嵌套关系拆成 `gate`（抢帧锁）/ `ime`（输入法）/
//!   `tick`（应用一帧，含 `draw`）/ `draw`（请求绘制那一段，含 `paint` + `build` + `vk`）；
//! - `paint` 是 gpui 的布局与绘制，到场景交给渲染器为止；`build` 是场景转顶点；
//!   `vk` 是六段之和，六段各自也印出来：取图 / 等 fence / 上传 / 记录 / 提交 / present；
//! - `draw` 减去后三项还有一个差值，那是绘制路径的簿记（多窗口循环、`present_due`）；
//! - `rest` gap 减去前三项，也就是循环自身与本行日志的开销。它大得离谱说明还有一段
//!   没被计时覆盖，那就得再打点。
//!
//! 判读靠对比：`wait` 占满 gap 说明线程在睡（上游没人要帧，帧率不是被我们压住的）；
//! `frame` 占满 gap 说明在画（再看它内部那几段谁大）；两者都不是，看 `rest`。
//!
//! 前九列（gap/wait/jobt/job1/frame/rest 与帧内几段）带 p50/p90/max，后面这一组全是 p50：
//! 它们要回答的是"谁占大头"，分布留给 `draw` 与 `frame` 那两列。`paint` 是例外，带三个
//! 分位：它只在真画了的帧上采样，只看 p50 会看不见空转秒里那几帧慢的。
//!
//! 最后一段是文本系统：`text n` 是 `layout_line`（完整 cosmic-text 整形）被调了几次，
//! `us` 是这一秒整形花了多少时间，`adv`/`ras` 是单字步进与字形栅格化的次数。
//! `paint` 贵而 `text us` 小，说明贵在走元素树；两者接近，说明贵在整形，而整形次数
//! 与"屏幕上有几行字"相当，就说明 gpui 的行缓存在反复失效。
//!
//! 跨线程的计数有两个：帧请求与被合并掉的移动。vsync 回调、输入、投递都在别的线程上，
//! 所以它们必须是原子量。

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// 一秒最多几百条样本，存下全部分位比维护直方图简单，也不会因为桶宽把 8ms 和 66ms
/// 分到同一格里。
const MAX_SAMPLES: usize = 4096;

/// 这一段是计时器本身，不是被测代码：换算失败给 0，不影响别的样本。
fn micros(duration: Duration) -> i32 {
    let value = duration.as_micros();
    if value > i32::MAX as u128 {
        i32::MAX
    } else {
        value as i32
    }
}

fn push(samples: &mut Vec<i32>, duration: Duration) {
    if samples.len() < MAX_SAMPLES {
        samples.push(micros(duration));
    }
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

fn maximum(samples: &[i32]) -> i32 {
    // 初值 0 是这一行的语义，不是"读失败当作 0"：耗时不可能为负，而没有样本就没有最大值，
    // 报 0 与报"没有"在这一行里是同一件事。写成 fold 是为了把初值摆明面上。
    samples.iter().fold(0, |highest, &value| highest.max(value))
}

/// 一帧里可以被单独计时的那几段。嵌套是故意的，差值本身就是要看的量：
///
/// ```text
/// Tick  = 应用一帧（定时器、焦点、悬停、绘制）
/// Draw  = 其中请求绘制的那一段（`request_frames`）
/// Paint = 其中 gpui 的布局与绘制，到场景交给渲染器为止
/// Build = 其中把场景转成顶点（`build_draw_list`）
/// Vk    = 其中 vk 提交与 present（六段之和）
/// ```
///
/// 于是 `Draw - Paint - Build - Vk` 就是绘制路径里剩下的空隙（多窗口的循环、
/// `present_due` 这些簿记）。任何一项单独变大都能立刻指到该优化谁。
#[derive(Clone, Copy)]
pub(crate) enum Part {
    Gate,
    Ime,
    Tick,
    Draw,
    Paint,
    Build,
}

#[derive(Default)]
struct Window {
    iterations: u32,
    frames: u32,
    jobs: u32,
    moves: u32,
    gap: Vec<i32>,
    wait: Vec<i32>,
    jobs_took: Vec<i32>,
    each: Vec<i32>,
    frame: Vec<i32>,
    rest: Vec<i32>,
    gate: Vec<i32>,
    ime: Vec<i32>,
    tick: Vec<i32>,
    draw: Vec<i32>,
    paint: Vec<i32>,
    build: Vec<i32>,
    /// vk 六段之和。
    vk: Vec<i32>,
    /// 这一秒里 gpui 调文本系统的次数与总耗时。`layout_line` 只在 gpui 的行缓存
    /// 未命中时才会被调用，所以它一涨就说明缓存在反复失效；它与 `paint` 的比值
    /// 就是"21.9ms 里有多少是文本整形"。
    text_calls: u32,
    text_total: Duration,
    text_max: Duration,
    advance_calls: u32,
    advance_total: Duration,
    rasterize_calls: u32,
    rasterize_total: Duration,
    bounds_calls: u32,
    bounds_total: Duration,
    atlas_calls: u32,
    atlas_total: Duration,
    /// 这一秒里最大的那个场景有几个图元。`paint` 贵而场景小，说明贵在"算出要画什么"，
    /// 而不是"画的东西太多"；反过来则说明是内容真的重。
    scene_quads: u32,
    scene_paths: u32,
    scene_sprites: u32,
    scene_underlines: u32,
    /// vk 六段各自。字段分开写而不是放进数组，是为了让这一行的读法与 `vk.rs` 里的
    /// 打点顺序一一对应，改名时不会只改了一处。
    vk_acquire: Vec<i32>,
    vk_fence: Vec<i32>,
    vk_upload: Vec<i32>,
    vk_record: Vec<i32>,
    vk_submit: Vec<i32>,
    vk_present: Vec<i32>,
    started: Option<Instant>,
}

thread_local! {
    static WINDOW: RefCell<Window> = RefCell::new(Window::default());
    static LAST_ITERATION: Cell<Option<Instant>> = const { Cell::new(None) };
    /// 这一帧的绘制从哪里开始。`request_frames` 之前落下，场景交给渲染器时取走：
    /// 两者之间就是 gpui 的布局与绘制，也就是 `paint`。
    static PAINT_ORIGIN: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// 帧请求的总数。vsync 回调与输入请求都在平台线程之外调用，所以只能走原子量：
/// 写进 thread local 就写到了别人那一份里，读出来永远是 0。
static WANTS: AtomicU32 = AtomicU32::new(0);

/// 被合并掉的移动条数。合并发生在投递侧（ArkUI 线程），计数也必须在那里加。
static MERGED: AtomicU32 = AtomicU32::new(0);

/// 记一次"有人要一帧"。可以不在平台线程调用。
pub(crate) fn note_want() {
    WANTS.fetch_add(1, Ordering::Relaxed);
}

/// 记一次"这条移动被后来的那条替掉了"。同样可以不在平台线程调用。
pub(crate) fn note_merged() {
    MERGED.fetch_add(1, Ordering::Relaxed);
}

/// 循环开始时调一次，返回与上一次开始之间的间隔。
///
/// 间隔必须在循环顶端取：放在一帧画完之后取，就会把循环尾部的开销算到下一轮头上，
/// 而那个开销正是 `rest` 要量的东西。
pub(crate) fn begin_iteration() -> Duration {
    let now = Instant::now();
    LAST_ITERATION.with(|cell| {
        let previous = cell.replace(Some(now));
        previous.map_or(Duration::ZERO, |previous| {
            now.saturating_duration_since(previous)
        })
    })
}

/// 一轮循环的账。`frame` 为 None 表示这一轮没有画帧（只跑了作业或者纯粹空转）。
pub(crate) fn note_iteration(
    gap: Duration,
    wait: Duration,
    queued: u32,
    moves: u32,
    jobs_took: Duration,
    frame: Option<Duration>,
) {
    WINDOW.with(|cell| {
        let mut window = cell.borrow_mut();
        window.iterations = window.iterations.saturating_add(1);
        window.jobs = window.jobs.saturating_add(queued);
        window.moves = window.moves.saturating_add(moves);
        push(&mut window.gap, gap);
        push(&mut window.wait, wait);
        push(&mut window.jobs_took, jobs_took);
        match frame {
            Some(frame) => {
                window.frames = window.frames.saturating_add(1);
                push(&mut window.frame, frame);
                push(
                    &mut window.rest,
                    gap.saturating_sub(wait + jobs_took + frame),
                );
            }
            None => push(&mut window.rest, gap.saturating_sub(wait + jobs_took)),
        }
    });
}

/// 单件活（一个作业或一条移动）自己的耗时。与 `jobt` 的区别是分母：那个是一轮的总和，
/// 这个是一件。有它才算得出"每个输入事件花多少钱"，也才看得出合并省了多少。
pub(crate) fn note_job(took: Duration) {
    WINDOW.with(|cell| push(&mut cell.borrow_mut().each, took));
}

/// 帧内某一段的耗时。同一帧里可以按 Gate → Ime → Tick（内含 Draw → Paint → Build）的顺序
/// 各记一次。
pub(crate) fn note_part(part: Part, took: Duration) {
    WINDOW.with(|cell| {
        let mut window = cell.borrow_mut();
        let samples = match part {
            Part::Gate => &mut window.gate,
            Part::Ime => &mut window.ime,
            Part::Tick => &mut window.tick,
            Part::Draw => &mut window.draw,
            Part::Paint => &mut window.paint,
            Part::Build => &mut window.build,
        };
        push(samples, took);
    });
}

/// 这一帧的绘制从这里算起。`request_frames` 之前调一次。
pub(crate) fn begin_paint() {
    PAINT_ORIGIN.with(|cell| cell.set(Some(Instant::now())));
}

/// 场景交给渲染器的时刻：从 `begin_paint` 到现在就是 gpui 的布局与绘制。
///
/// 取走的语义是"一帧只算一次"：没脏的帧 gpui 根本不会调用 `draw`，那时这个起点
/// 就白落了，下一次 `begin_paint` 直接盖掉它，不会留下一个越来越长的假样本。
pub(crate) fn end_paint() {
    let elapsed = PAINT_ORIGIN.with(|cell| cell.take().map(|origin| origin.elapsed()));
    if let Some(elapsed) = elapsed {
        note_part(Part::Paint, elapsed);
    }
}

/// vk 的一帧六段。顺序与 `vk.rs: render_scene` 里的打点一致：
/// 取图 → 等上一帧的 fence → 上传 → 记录命令 → 提交 → present。
///
/// 以前这六段只在 hilog 里每 60 帧打一行，而 hilog 缓冲只留得住约 50 行 `gpui_ohos`，
/// 拖完之后再去看已经是空的。并进这一行之后，它们与 `draw` 落在同一秒里，能直接相减。
pub(crate) fn note_vk_frame(
    acquire: Duration,
    fence: Duration,
    upload: Duration,
    record: Duration,
    submit: Duration,
    present: Duration,
) {
    WINDOW.with(|cell| {
        let mut window = cell.borrow_mut();
        push(&mut window.vk_acquire, acquire);
        push(&mut window.vk_fence, fence);
        push(&mut window.vk_upload, upload);
        push(&mut window.vk_record, record);
        push(&mut window.vk_submit, submit);
        push(&mut window.vk_present, present);
        push(
            &mut window.vk,
            acquire + fence + upload + record + submit + present,
        );
    });
}

/// 记一次 `layout_line`：gpui 的行缓存未命中才会走到这里，所以每次调用都是一次
/// 完整的 cosmic-text 整形。只在平台线程上调用（绘制就在那条线程上），因此用
/// thread local 就够——写在别的线程上也确实不该算进这一帧的账。
pub(crate) fn note_text_layout(took: Duration) {
    WINDOW.with(|cell| {
        let mut window = cell.borrow_mut();
        window.text_calls = window.text_calls.saturating_add(1);
        window.text_total += took;
        if took > window.text_max {
            window.text_max = took;
        }
    });
}

/// 单字步进（`advance`）。它不进 `layout_line`，但每次绘制字形都可能问一次，
/// 所以单独计数计时，用来分辨"贵在整形"和"贵在逐字查询"。
pub(crate) fn note_text_advance(took: Duration) {
    WINDOW.with(|cell| {
        let mut window = cell.borrow_mut();
        window.advance_calls = window.advance_calls.saturating_add(1);
        window.advance_total += took;
    });
}

/// 一次字形栅格化 = 一次图集未命中。稳态拖拽里它应该接近 0。
pub(crate) fn note_text_rasterize(took: Duration) {
    WINDOW.with(|cell| {
        let mut window = cell.borrow_mut();
        window.rasterize_calls = window.rasterize_calls.saturating_add(1);
        window.rasterize_total += took;
    });
}

/// `glyph_raster_bounds`：每个要画的字形都会问一次它的像素包围盒，命中图集也躲不开。
/// 它落在 `paint` 里，是"每帧固定成本"的一个候选，所以单独计时。
pub(crate) fn note_text_bounds(took: Duration) {
    WINDOW.with(|cell| {
        let mut window = cell.borrow_mut();
        window.bounds_calls = window.bounds_calls.saturating_add(1);
        window.bounds_total += took;
    });
}

/// `PlatformAtlas::get_or_insert_with`：每个字形、每张图片绘制前都要过一次。
/// 命中的那一侧只是查表，但调用次数与图元数同量级，所以同样计时——
/// 它与 `ras` 的差就是查表本身的成本。
pub(crate) fn note_atlas_lookup(took: Duration) {
    WINDOW.with(|cell| {
        let mut window = cell.borrow_mut();
        window.atlas_calls = window.atlas_calls.saturating_add(1);
        window.atlas_total += took;
    });
}

/// 这一帧画出来的场景有多大。取这一秒内的最大值：要的是"最重的那一帧有多少图元"，
/// 而不是平均——平均值会被空转帧稀释。
pub(crate) fn note_scene(quads: u32, paths: u32, sprites: u32, underlines: u32) {
    WINDOW.with(|cell| {
        let mut window = cell.borrow_mut();
        window.scene_quads = window.scene_quads.max(quads);
        window.scene_paths = window.scene_paths.max(paths);
        window.scene_sprites = window.scene_sprites.max(sprites);
        window.scene_underlines = window.scene_underlines.max(underlines);
    });
}

/// 攒够一秒打一行，然后把窗口清空。这一秒里一轮循环都没跑过就什么也不打。
pub(crate) fn report_if_due() {
    WINDOW.with(|cell| {
        let mut window = cell.borrow_mut();
        let started = *window.started.get_or_insert_with(Instant::now);
        if started.elapsed().as_millis() < 1000 || window.iterations == 0 {
            return;
        }
        let line = format!(
            "[frame] it={} drew={} want={} jobs={} moves={} merged={} | \
             gap={}/{}/{} wait={}/{}/{} jobt={}/{}/{} job1={}/{}/{} frame={}/{}/{} rest={}/{}/{} | \
             gate={} ime={} tick={} draw={} paint={}/{}/{} build={} vk={} | \
             acq={} fence={} up={} rec={} sub={} pres={} us | \
             text n={} us={} max={} advn={} advus={} rasn={} rasus={} | \
             bndn={} bndus={} atln={} atlus={} | \
             scene quad={} path={} sprite={} ul={} | at={}",
            window.iterations,
            window.frames,
            WANTS.swap(0, Ordering::Relaxed),
            window.jobs,
            window.moves,
            MERGED.swap(0, Ordering::Relaxed),
            percentile(&window.gap, 50),
            percentile(&window.gap, 90),
            maximum(&window.gap),
            percentile(&window.wait, 50),
            percentile(&window.wait, 90),
            maximum(&window.wait),
            percentile(&window.jobs_took, 50),
            percentile(&window.jobs_took, 90),
            maximum(&window.jobs_took),
            percentile(&window.each, 50),
            percentile(&window.each, 90),
            maximum(&window.each),
            percentile(&window.frame, 50),
            percentile(&window.frame, 90),
            maximum(&window.frame),
            percentile(&window.rest, 50),
            percentile(&window.rest, 90),
            maximum(&window.rest),
            percentile(&window.gate, 50),
            percentile(&window.ime, 50),
            percentile(&window.tick, 50),
            percentile(&window.draw, 50),
            // 以下全是 p50：这一行已经够长了，而这几项要看的是"谁占大头"，
            // 分位分布留给 `draw` 与 `frame` 那两列。
            percentile(&window.paint, 50),
            percentile(&window.paint, 90),
            maximum(&window.paint),
            percentile(&window.build, 50),
            percentile(&window.vk, 50),
            percentile(&window.vk_acquire, 50),
            percentile(&window.vk_fence, 50),
            percentile(&window.vk_upload, 50),
            percentile(&window.vk_record, 50),
            percentile(&window.vk_submit, 50),
            percentile(&window.vk_present, 50),
            // 文本系统与墙钟：前者按总和（要的是"这一秒一共整形了多久"），
            // 后者是给事后分段用的——探针文件是追加写，A/B 两轮之间只差一段空转，
            // 有时间戳才不用靠猜。
            window.text_calls,
            window.text_total.as_micros(),
            micros(window.text_max),
            window.advance_calls,
            window.advance_total.as_micros(),
            window.rasterize_calls,
            window.rasterize_total.as_micros(),
            window.bounds_calls,
            window.bounds_total.as_micros(),
            window.atlas_calls,
            window.atlas_total.as_micros(),
            window.scene_quads,
            window.scene_paths,
            window.scene_sprites,
            window.scene_underlines,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0u64, |since| since.as_secs()),
        );
        *window = Window {
            started: Some(Instant::now()),
            ..Window::default()
        };
        drop(window);
        crate::log_line(&line);
    });
}
