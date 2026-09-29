//! 导入进度与导入互斥：进程内一张表，同时回答「这条源有没有导入在跑」与「跑到哪了」。
//!
//! # 为什么两件事共用一张表
//!
//! 进度必须**与导入同生共灭**。导入的超时不是「把任务取消」，而是丢弃 handler 的
//! future（`http.request_timeout_seconds`）：future 被丢掉 → [`ImportGuard`] 被 drop
//! → 条目消失。进度若另有一份自己的登记，超时后它会永远停在「写入中」，前端就一直
//! 转圈；而占用与进度共用 [`ImportGuard::drop`] 这一个清理点，就不会有这种半死不活的
//! 假条目。
//!
//! # 三条硬规矩（谁破了谁负责）
//!
//! 1. **条目存在 ⟺ 本进程上这条源有一次导入在跑**。一张表、一条 `Drop`。把导入挪进
//!    `tokio::spawn`（future 不再随请求超时被丢弃）或把 `let _guard = …` 写成
//!    `let _ = …`（当场释放），这条保证立刻失效：前者留下没人清的假条目，后者让互斥
//!    形同虚设。
//! 2. **进程内、单实例前提**。本应用按单实例部署。多实例（蓝绿）下轮询可能落到另一色，
//!    **「查不到 ≠ 没在跑」**；那时要做的是 Redis 分布式锁 + 心跳，这段注释就是升级说明。
//! 3. **上报在条目缺失时静默忽略**：绝不 panic，也绝不凭空造条目。造出来的条目没有
//!    owner、永远不会被清掉，比没有进度更坏。
//!
//! 锁中毒继续可用：恢复内部数据，不让一次 panic 把导入永久锁死。

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

use schemars::JsonSchema;
use serde::Serialize;

/// 导入阶段。
///
/// 线上名字由 `rename_all = "snake_case"` 给出，前端按这些字符串分支——
/// 改名就是改契约。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ImportStage {
    /// 没有导入在跑（查不到条目时的兜底）。
    Idle,
    /// 读上传文件、校验表头。**上传阶段也报它**：multipart body 在 transport 层读完才
    /// dispatch，服务端拿不到上传字节的进度，那一段只能落在最近的这个档位上。
    Loading,
    /// 解析数据行。
    Parsing,
    /// 逐绑定落库。
    Writing,
}

/// 一条导入的进度快照。字段全是 `Option`：`None` = 该阶段还没报过。
///
/// **不要给字段加 `skip_serializing_if`**：键集是前后端各写一份、逐字对账的契约
/// （`feishu-projections.json` 的 `get_import_progress.result.emitted`，由 Action 里的
/// `assert_keys` 钉住），某个阶段突然少一个键就是一次静默的契约漂移。
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(crate) struct ImportProgress {
    stage: ImportStage,
    /// 本轮一共几个文件（`MultipartSpec::max_files(32)` 界定）。
    files_total: Option<u32>,
    /// 已读完**并校验过表头**的文件数。
    files_done: Option<u32>,
    /// 正在解析第几个文件（1-based）。
    file_index: Option<u32>,
    /// 该文件已越过的**物理**行序号（含空行）。
    ///
    /// 与回执里的 `rows_read` 刻意不同口径：后者是**非空**数据行数。别顺手统一——
    /// 这个数要在解析途中单调推进，而「读进来几行」要等到行收口时才知道。
    rows_done: Option<u64>,
    /// 该文件的数据行总数（calamine 的 `<dimension>`）；读不到 = `None`。
    rows_total: Option<u64>,
    /// 本轮要处理几条绑定。
    bindings_total: Option<u32>,
    /// **已开始处理**的绑定数（开始处理第 i 条时是 i，0-based）。
    bindings_done: Option<u32>,
}

impl ImportProgress {
    /// 查不到条目时的兜底：`Idle` + 计数全空。
    pub(crate) fn idle() -> Self {
        Self::at(ImportStage::Idle)
    }

    fn at(stage: ImportStage) -> Self {
        Self {
            stage,
            files_total: None,
            files_done: None,
            file_index: None,
            rows_done: None,
            rows_total: None,
            bindings_total: None,
            bindings_done: None,
        }
    }
}

/// 进程内的导入登记表：`datasource_id` → 进度（条目存在即「有导入在跑」）。
static IMPORTING: OnceLock<Mutex<HashMap<i64, ImportProgress>>> = OnceLock::new();

/// 取锁。中毒的锁不该让导入永久不可用：恢复内部数据继续用。
fn lock() -> MutexGuard<'static, HashMap<i64, ImportProgress>> {
    IMPORTING
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 一条数据源的导入占用凭证。存活期间即持有占用，条目（连同进度）在 `Drop` 时消失。
///
/// **不持有 `MutexGuard`**（`acquire` 里那个在函数返回前就落了）：std 的 `MutexGuard`
/// 不是 `Send`，把它揣在结构里会让 `handle` 的 future 变成非 `Send`，而它要交给 axum。
/// 占用的事实记在 [`IMPORTING`] 这张表里，凭证只记着自己占的是哪一条。这样一来
/// `Drop` 也覆盖了所有返回路径——`?` 提前返回与 panic 展开都会走到它。
pub(crate) struct ImportGuard {
    datasource_id: i64,
}

/// 该数据源已有一次导入在跑。
pub(crate) struct ImportBusy;

impl ImportGuard {
    /// 占用这条数据源的导入位，并立刻放一份「加载中、计数全空」的进度进去。
    pub(crate) fn acquire(datasource_id: i64) -> Result<Self, ImportBusy> {
        let mut importing = lock();
        // **先判再插**，不要「插了再看返回值」：那样在已在跑时会把对方正在写的进度
        // 覆盖成刚构造的空快照——占用没拿到，对方的进度却被抹了。
        if importing.contains_key(&datasource_id) {
            return Err(ImportBusy);
        }
        importing.insert(datasource_id, ImportProgress::at(ImportStage::Loading));
        Ok(Self { datasource_id })
    }

    /// 测试专用别名。
    ///
    /// 生产侧只该从 `handle` 拿锁，「测试能拿到一把锁」与「端点会拿锁」是两件事，
    /// 名字分开是为了让测试读起来不像在生产路径上。
    #[cfg(test)]
    pub(crate) fn acquire_for_test(datasource_id: i64) -> Result<Self, ImportBusy> {
        Self::acquire(datasource_id)
    }
}

impl Drop for ImportGuard {
    fn drop(&mut self) {
        lock().remove(&self.datasource_id);
    }
}

/// 查这条源的进度快照。`None` = 本进程上没有它的条目（含「从来没导过」与「已跑完」）。
pub(crate) fn snapshot(datasource_id: i64) -> Option<ImportProgress> {
    // 只读路径不初始化登记表：一个从没跑过导入的进程不该因为查进度而建表。
    let importing = IMPORTING.get()?;
    importing
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&datasource_id)
        .cloned()
}

/// 上报的唯一入口：**条目缺失时静默忽略**，绝不 panic、绝不凭空造条目。
///
/// 这里**不做**「表还没建就先返回」的短路（`snapshot` 那条只读路径才做）：上报走哪条
/// 分支取决于进程里跑过什么别的测试，会让「造不造条目」这条不变量变得看运气。
fn update(datasource_id: i64, apply: impl FnOnce(&mut ImportProgress)) {
    let mut importing = lock();
    if let Some(progress) = importing.get_mut(&datasource_id) {
        apply(progress);
    }
}

/// 上报「已读完并校验过表头的文件数」。
pub(crate) fn report_loading(datasource_id: i64, files_total: u32, files_done: u32) {
    update(datasource_id, |progress| {
        progress.stage = ImportStage::Loading;
        progress.files_total = Some(files_total);
        progress.files_done = Some(files_done);
    });
}

/// 上报解析进度。
///
/// `rows_done` 是**物理**行序号（含空行），`rows_total` 来自 `<dimension>`
/// （读不到 = `None`，前端此时只报计数、不报分母）。
pub(crate) fn report_parsing(
    datasource_id: i64,
    file_index: u32,
    files_total: u32,
    rows_done: u64,
    rows_total: Option<u64>,
) {
    update(datasource_id, |progress| {
        progress.stage = ImportStage::Parsing;
        progress.file_index = Some(file_index);
        progress.files_total = Some(files_total);
        progress.rows_done = Some(rows_done);
        progress.rows_total = rows_total;
    });
}

/// 上报「已开始处理的绑定数」。
///
/// 调用点是绑定循环的**循环体首行**，所以 `bindings_done` 到不了 `bindings_total`
/// （最后一条正在写）——前端按「已完成 k/n」显示，差的那一条是正在跑的那条。
pub(crate) fn report_writing(datasource_id: i64, bindings_total: u32, bindings_done: u32) {
    update(datasource_id, |progress| {
        progress.stage = ImportStage::Writing;
        progress.bindings_total = Some(bindings_total);
        progress.bindings_done = Some(bindings_done);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // **每条用例的 id 全部错开**：登记表是进程级静态量，而 `#[test]` 默认跑在并行
    // 线程上。两条用例共用一个 id 的话，谁先拿到、谁后拿到是调度决定的——随机失败。

    #[test]
    fn the_entry_exists_only_while_the_guard_lives() {
        let guard = ImportGuard::acquire_for_test(11).unwrap_or_else(|_| panic!("第一次应拿到"));
        let running = snapshot(11).unwrap_or_else(|| panic!("持锁期间必须查得到进度"));
        assert_eq!(running.stage, ImportStage::Loading);
        drop(guard);
        assert!(snapshot(11).is_none(), "`Drop` 是唯一的清理点");
    }

    #[test]
    fn a_finished_run_does_not_leak_counts_into_the_next_one() {
        let guard = ImportGuard::acquire_for_test(12).unwrap_or_else(|_| panic!("第一次应拿到"));
        report_parsing(12, 1, 1, 7, Some(9));
        drop(guard);

        let _second =
            ImportGuard::acquire_for_test(12).unwrap_or_else(|_| panic!("跑完应能再拿到"));
        let fresh = snapshot(12).unwrap_or_else(|| panic!("新一轮必须查得到"));
        assert_eq!(fresh.stage, ImportStage::Loading, "新一轮从 Loading 开始");
        assert_eq!(fresh.rows_done, None, "上一轮的计数不能继承");
        assert_eq!(fresh.rows_total, None);
    }

    #[test]
    fn a_second_acquire_for_the_same_source_is_busy() {
        let held = ImportGuard::acquire_for_test(7);
        assert!(held.is_ok(), "第一次应拿到");
        report_parsing(7, 1, 1, 3, Some(9));

        let second = ImportGuard::acquire_for_test(7);
        assert!(
            matches!(second, Err(ImportBusy)),
            "同一数据源的第二次并发导入必须被拒"
        );
        let kept = snapshot(7).unwrap_or_else(|| panic!("在跑的那次必须还在"));
        assert_eq!(
            kept.rows_done,
            Some(3),
            "被拒的那次不能把在跑那次的进度抹成空快照"
        );
    }

    #[test]
    fn a_different_source_is_not_blocked() {
        // **id 与上一条错开**：登记表是进程级静态量，而 `#[test]` 默认跑在并行线程上。
        // 两条测试都用 7 的话，谁先拿到、谁后拿到是调度决定的——互相抢同一把锁会让
        // 这条测试随机失败。
        let _held = ImportGuard::acquire_for_test(107).unwrap_or_else(|_| panic!("第一次应拿到"));
        assert!(
            ImportGuard::acquire_for_test(108).is_ok(),
            "不同数据源互不影响"
        );
    }

    #[test]
    fn reports_without_an_entry_are_ignored() {
        // 条目缺失（从没跑过 / 已经跑完）时的上报必须**静默忽略**：造条目会留下一个
        // 没有 owner、永远不会被清掉的假进度。
        report_loading(13, 2, 1);
        report_parsing(13, 1, 2, 5, Some(9));
        report_writing(13, 3, 0);
        assert!(snapshot(13).is_none(), "上报绝不凭空造条目");
    }

    #[test]
    fn every_stage_serializes_to_its_wire_name() {
        for (stage, name) in [
            (ImportStage::Idle, "idle"),
            (ImportStage::Loading, "loading"),
            (ImportStage::Parsing, "parsing"),
            (ImportStage::Writing, "writing"),
        ] {
            let mut progress = ImportProgress::idle();
            progress.stage = stage;
            let value = serde_json::to_value(&progress).unwrap_or_else(|error| panic!("{error}"));
            assert_eq!(
                value["stage"],
                serde_json::json!(name),
                "阶段名就是线上契约：改名等于前端认不出，会静默降级成 idle"
            );
        }
    }
}
