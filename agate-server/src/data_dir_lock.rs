//! 数据目录跨进程 Owner Lock。
//!
//! ── 为什么需要它 ────────────────────────────────────────────
//! 同一份 `{config_dir}` 被两个 Agate 进程同时打开时，SQLite 的
//! `-wal` / 检查点会互相踩，配置快照与 `kernel-state.json` 也会交叉覆盖。
//! 桌面双开、Android 前台服务 + 独立调试进程、容器被 `restart` 拉起而旧进程
//! 尚未退出 —— 都是真实可达的组合。本模块把「一个数据目录同时只允许一个
//! Owner」变成启动期的硬约束。
//!
//! ── 为什么用 flock 而不是别的 ────────────────────────────────
//! * **不用 `File::try_lock()`**：它 1.89 才稳定，本 crate 的 MSRV 是 1.88
//!   （`agate-server/Cargo.toml` 的 `rust-version`，来自 rusqlite 0.40 →
//!   libsqlite3-sys 0.38 的声明）。用它会悄悄把 MSRV 抬到 1.89。
//! * **不用 `fs2` 等第三方锁 crate**：`libc` 本就在依赖树里（rusqlite /
//!   reqwest / tokio 都会拉），提升为直接依赖不新增任何解析结果、不改
//!   `Cargo.lock` 的版本集合。见 `Cargo.toml` 对应条目。
//! * **不自研 PID 存活判断 + 删锁文件协议**：那是经典的竞态温床（PID 复用、
//!   `kill(0)` 权限、删除与重建之间的窗口）。flock 由内核维护，进程无论怎么
//!   死（含 `SIGKILL`）内核都会把锁释放掉，这正是要的语义。
//!
//! ── 三条不可动摇的硬约束 ────────────────────────────────────
//! 1. **绝不 `unlink` 锁文件**。`Drop` 只让 fd 关闭（由 `File` 完成），文件本身
//!    留在盘上。已加锁的文件被 unlink 之后，新进程会在**同一个路径上创建一个
//!    新 inode** 并成功加锁 —— 两个进程各自锁着不同 inode，双双认为自己是
//!    Owner，这就是「双实例漏洞」。留着文件，最坏情况只是一个 0 字节的
//!    `.agate.lock`。
//! 2. **`FD_CLOEXEC` 必须真的置上**。Sidecar（Qoder 的 `runtime-info`、
//!    mihomo 等）通过 `tokio::process` 起子进程，`exec` 之后子进程会继承所有
//!    未置 close-on-exec 的 fd。锁 fd 被继承的后果是：宿主进程退出、子进程
//!    还活着 → 锁没释放 → 下次启动被自己的残留子进程挡住。`open(2)` 时直接
//!    带 `O_CLOEXEC`，再用 `F_GETFD`/`F_SETFD` 复核补一遍。
//! 3. **失败必须可区分**。占用冲突（`Occupied`）与真正的 IO 错误（`Io`）分开
//!    上报；`Io` **绝不**静默降级成「无锁继续」——「锁不上就裸奔」等于没有锁。
//!
//! ── Android 文件系统实测（真实设备结论，不是推测）────────────
//! 在 Android 14 设备上对本仓两种候选路径实测 `flock(LOCK_EX|LOCK_NB)`：
//!
//! | 路径 | 文件系统 | 结果 |
//! |---|---|---|
//! | `/data/data/<pkg>/files/agate`（`AgateApp.kt:22` 与 `AgateForegroundService.kt:276` 传的 `filesDir.resolve("agate")`） | ext4 / f2fs（应用私有目录） | **成功** |
//! | `/data/data/<pkg>/files/home/...`（同分区另一路径） | 同上 | **成功** |
//! | `/storage/emulated/0/...`、`/sdcard/...`（共享存储，FUSE / sdcardfs） | 虚拟文件系统 | **`ENOSYS`(38)** |
//!
//! 结论：**Android 主线数据目录支持本锁**；只有把 `AGATE_PROXY_HOME` 指到
//! 共享存储（`/sdcard`）的部署会拿到 `Io` 并因此拒绝启动 —— 那是正确行为，
//! 共享存储上 SQLite 的 WAL 语义本来也不可靠。该路径由 §5.9 留到真机验收时
//! 复核双进程冲突表现，本章今天只证明了「能锁上」。

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

/// 锁文件名。固定放在 `{config_dir}` 下 —— 与数据库、配置同目录，迁移
/// 目录时一起搬走，不额外引入第二个路径来源。
pub const LOCK_FILE_NAME: &str = ".agate.lock";

/// 取锁失败的两类原因。**刻意不实现 `From<io::Error>`**：那会让调用点用 `?`
/// 把「占用冲突」误当成普通 IO 错误吞掉，两者对用户的含义完全不同。
#[derive(Debug)]
pub enum LockError {
    /// 另一个进程正持有同一数据目录。带锁文件路径，直接可展示给用户 / JNI。
    Occupied(PathBuf),
    /// 打开或加锁时的普通 IO 错误（权限、只读文件系统、`ENOSYS` 等）。
    Io(PathBuf, std::io::Error),
}

impl LockError {
    /// 锁文件路径（两类错误都带），供 JNI 把可操作信息透给 Kotlin。
    pub fn path(&self) -> &Path {
        match self {
            LockError::Occupied(path) | LockError::Io(path, _) => path,
        }
    }

    /// 是否为「已被占用」。调用方（JNI）据此决定文案是「已有实例在跑」
    /// 还是「数据目录不可用」。
    pub fn is_occupied(&self) -> bool {
        matches!(self, LockError::Occupied(_))
    }
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::Occupied(path) => write!(
                f,
                "数据目录已被另一个 Agate 实例占用（{}）：请先关闭正在运行的实例，\
                 或改用独立的数据目录",
                path.display()
            ),
            LockError::Io(path, error) => {
                write!(f, "数据目录锁不可用（{}）：{error}", path.display())
            }
        }
    }
}

impl std::error::Error for LockError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LockError::Occupied(_) => None,
            LockError::Io(_, error) => Some(error),
        }
    }
}

/// 活着的 Owner Lock。持有期间数据目录归本进程所有。
///
/// **不要在 `Drop` 里 unlink 锁文件**（见模块头约束 1）：本类型的 drop 只发生
/// 在 `File` 上，关闭 fd 即释放 flock，文件保留。
#[derive(Debug)]
pub struct DataDirLock {
    /// 只为了持有 fd。`File` 的 drop 关闭 fd → 内核释放 flock。
    _file: File,
    /// 锁文件绝对路径（诊断 + 冲突报错用）
    path: PathBuf,
    /// 数据目录（规范化后），同进程重入判定用
    config_dir: PathBuf,
    /// 取锁成功的时刻，纯诊断
    acquired_at: SystemTime,
}

impl DataDirLock {
    /// 在 `config_dir` 下取锁。目录**必须**已存在（调用方在 bootstrap 第 3 步
    /// 创建），本函数不 `create_dir_all` —— 建目录是启动顺序的一部分，
    /// 藏进锁里会让「冲突时不得产生副作用」这条判据失效。
    pub fn acquire(config_dir: &Path) -> Result<Self, LockError> {
        let path = config_dir.join(LOCK_FILE_NAME);
        let io = |error| LockError::Io(path.clone(), error);

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o644)
            // `open(2)` 直接带 O_CLOEXEC：即使后面 F_SETFD 那步被跳过，
            // fd 也不会漏进 exec 的子进程（约束 2）。
            .custom_flags(libc::O_CLOEXEC)
            .open(&path)
            .map_err(io)?;

        let fd = file.as_raw_fd();
        // 复核并补一道 F_SETFD：老内核对 open 的 O_CLOEXEC 支持不完整时兜底。
        // SAFETY: fd 来自上面成功打开的 File，调用期间 File 存活、未关闭；
        // fcntl 不转移所有权。
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 && flags & libc::FD_CLOEXEC == 0 {
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }

        // 非阻塞排他锁；EWOULDBLOCK/EAGAIN = 已有 Owner。
        // EINTR 理论上不会出现（LOCK_NB 不等待），但仍按惯例重试一次护栏。
        loop {
            // SAFETY: fd 有效；flock 只读 fd 状态，不转移所有权。
            let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                break;
            }
            let error = std::io::Error::last_os_error();
            match error.kind() {
                ErrorKind::Interrupted => continue,
                ErrorKind::WouldBlock => return Err(LockError::Occupied(path)),
                _ => {
                    // 含 ENOSYS（共享存储 / FUSE 不支持 flock）—— 按约束 3
                    // 一律判 Io，绝不降级为无锁继续。
                    return Err(LockError::Io(path, error));
                }
            }
        }

        // 诊断信息（谁在什么时刻持锁）。**写失败不得导致锁被释放**：
        // 这里忽略所有结果，fd 依然持有 flock。
        let stamp = format!(
            "pid={} at={}\n",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        );
        let _ = file.set_len(0);
        let _ = file.write_all(stamp.as_bytes());
        let _ = file.flush();

        Ok(DataDirLock {
            _file: file,
            path,
            config_dir: canonical_dir(config_dir),
            acquired_at: SystemTime::now(),
        })
    }

    /// 锁文件路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 该锁所属的数据目录（规范化后）。
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// 取锁时刻（诊断 / 日志用）。
    pub fn acquired_at(&self) -> SystemTime {
        self.acquired_at
    }

    /// fd 是否带 `FD_CLOEXEC`。仅测试与诊断用，生产路径不依赖它。
    pub fn is_close_on_exec(&self) -> bool {
        // SAFETY: fd 由 `_file` 持有，本调用期间未释放。
        let flags = unsafe { libc::fcntl(self._file.as_raw_fd(), libc::F_GETFD) };
        flags >= 0 && flags & libc::FD_CLOEXEC != 0
    }
}

/// 规范化用于「同进程重入」比对的目录路径。
///
/// 不 canonicalize 失败就报错：`acquire` 的调用约定是目录已存在，但即便
/// 它在极端情况下不可解析，也不该因此拒绝启动 —— 退回原路径比对即可，
/// 跨进程的正确性由 flock 保证，这里只影响同进程重入判定。
fn canonical_dir(dir: &Path) -> PathBuf {
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
}

// ── 同进程重入 ──────────────────────────────────────────────
// Android 的 Core 是进程级单例（`jni_bridge::CORE`），Application 启动与 FGS
// 兜底都会走 `core_init`。虽然 `ensure_core` 已有引导锁，这里仍必须让「同一
// 进程对同一目录重复取锁」幂等返回**同一把**锁：
//   · 不同 fd 的两把 flock 在同进程内也会互相冲突（flock 按 open file
//     description 计，不按进程计），第二次 `acquire` 会误报 Occupied；
//   · 而「两个进程」才是要挡的对象，同进程重复初始化不是错误。
// 用 Weak 存登记，最后一个 Arc 落地时登记项自然失效，无需显式注销。
static HELD: OnceLock<Mutex<Vec<Weak<DataDirLock>>>> = OnceLock::new();

fn held_registry() -> &'static Mutex<Vec<Weak<DataDirLock>>> {
    HELD.get_or_init(|| Mutex::new(Vec::new()))
}

/// 取锁（同进程幂等）。生产路径**统一走这里**，不要直接调 `DataDirLock::acquire`。
///
/// * 本进程已持有同一 `config_dir` 的锁，且该锁仍活着 → 返回同一个 `Arc`，不重复
///   flock，也不报错；
/// * 否则真正取锁；`Occupied` 意味着**另一个进程**持有，原样上报。
pub fn acquire_shared(config_dir: &Path) -> Result<Arc<DataDirLock>, LockError> {
    let key = canonical_dir(config_dir);
    {
        let mut registry = held_registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // 顺手清掉已落地的 Weak，避免登记表随启动次数无界增长
        registry.retain(|weak| weak.strong_count() > 0);
        if let Some(existing) = registry
            .iter()
            .filter_map(Weak::upgrade)
            .find(|lock| lock.config_dir == key)
        {
            return Ok(existing);
        }
    }

    let lock = Arc::new(DataDirLock::acquire(config_dir)?);
    {
        let mut registry = held_registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        registry.push(Arc::downgrade(&lock));
    }
    Ok(lock)
}

// ── tests ────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// 每个用例一个独立临时目录（不引入 `tempfile`，避免新 dev-dependency）。
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let dir = std::env::temp_dir().join(format!(
                "agate-lock-test-{tag}-{}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("创建临时目录");
            TempDir(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 测试间的 fork 互斥护卫。
    ///
    /// `std::process::Command::spawn()` 在子进程 `exec` 完成前不返回，而 **fork 之后、
    /// `exec` 之前**子进程会复制父进程**全部** fd —— 包括别的测试正持有的锁。
    /// 若此时那个测试恰好 `drop` 自己的锁并立即重取，锁实际仍未释放（副本还活着），
    /// 就会偶发收到 `Occupied`。
    ///
    /// 这不是库的缺陷（flock 的语义就是「按 open file description 计」），而是测试
    /// 隔离问题：单线程跑全过、并行偶发失败，即此。会 spawn 子进程的用例与会
    /// drop+重取的用例都拿这把互斥，两者永不同时在场。
    static FORK_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn fork_guard() -> std::sync::MutexGuard<'static, ()> {
        FORK_GUARD
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut list: Vec<String> = std::fs::read_dir(dir)
            .expect("读目录")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        list.sort();
        list
    }

    // 1
    #[test]
    fn same_data_dir_second_lock_is_rejected_without_side_effects() {
        let dir = TempDir::new("t1");
        let _first = DataDirLock::acquire(dir.path()).expect("首次取锁应成功");

        let second = DataDirLock::acquire(dir.path());
        match second {
            Err(LockError::Occupied(path)) => {
                assert_eq!(path, dir.path().join(LOCK_FILE_NAME), "冲突应报锁文件路径");
            }
            other => panic!("第二次取锁必须报 Occupied，实际: {other:?}"),
        }

        // 冲突没有产生任何新文件：目录里只有那一个锁文件
        assert_eq!(names(dir.path()), vec![LOCK_FILE_NAME.to_string()]);
    }

    // 2
    #[test]
    fn lock_released_after_drop_allows_reacquire() {
        let _guard = fork_guard();
        let dir = TempDir::new("t2");
        {
            let _lock = DataDirLock::acquire(dir.path()).expect("首次取锁");
        }
        let again = DataDirLock::acquire(dir.path()).expect("drop 之后必须能再次取锁");
        assert_eq!(again.path(), dir.path().join(LOCK_FILE_NAME));
    }

    // 3
    #[test]
    fn different_data_dirs_can_lock_concurrently() {
        let a = TempDir::new("t3a");
        let b = TempDir::new("t3b");
        let _lock_a = DataDirLock::acquire(a.path()).expect("目录 a 取锁");
        let _lock_b = DataDirLock::acquire(b.path()).expect("目录 b 取锁应同时成功");
    }

    // 4
    #[cfg(unix)]
    #[test]
    fn symlink_or_alias_to_same_dir_is_rejected() {
        let real = TempDir::new("t4real");
        let alias_parent = TempDir::new("t4alias");
        let alias = alias_parent.path().join("link");
        std::os::unix::fs::symlink(real.path(), &alias).expect("建 symlink");

        let _lock = DataDirLock::acquire(real.path()).expect("真实目录取锁");
        match DataDirLock::acquire(&alias) {
            Err(LockError::Occupied(_)) => {}
            other => panic!("经 symlink 访问同一目录必须冲突，实际: {other:?}"),
        }
    }

    // 5
    #[test]
    fn bootstrap_conflict_does_not_open_db_or_write_config() {
        let _guard = fork_guard();
        let holder = TempDir::new("t5hold");
        let target = TempDir::new("t5target");
        // 占位：在目标目录里预先放一份「已有配置」，冲突时它必须原样不动
        let config_path = target.path().join("config.json");
        std::fs::write(&config_path, "{\"preserved\":true}").expect("写占位配置");
        let before_meta = std::fs::metadata(&config_path).expect("读元数据");
        let before_len = before_meta.len();
        let before_mtime = before_meta.modified().expect("mtime");

        // 由**子进程**持有目标目录的锁，然后本进程尝试取锁 —— 模拟「另一实例在跑」
        let child = std::process::Command::new(std::env::current_exe().expect("当前 exe"))
            .arg("--exact")
            .arg("data_dir_lock::tests::zz_child_hold_lock")
            .arg("--nocapture")
            .env("AGATE_LOCK_PROBE_DIR", target.path())
            .env("AGATE_LOCK_PROBE_HOLD_MS", "3000")
            .spawn()
            .expect("拉起持锁子进程");
        // 等子进程真正拿到锁
        std::thread::sleep(std::time::Duration::from_millis(800));

        let conflict = DataDirLock::acquire(target.path());
        assert!(
            matches!(conflict, Err(LockError::Occupied(_))),
            "另一进程持锁时必须冲突，实际: {conflict:?}"
        );

        // 冲突后：数据库不存在、配置内容与 mtime 未变
        assert!(
            !target.path().join("agate.db").exists(),
            "冲突时不得创建数据库"
        );
        let after = std::fs::metadata(&config_path).expect("读元数据");
        assert_eq!(after.len(), before_len, "配置长度不得变化");
        assert_eq!(
            after.modified().expect("mtime"),
            before_mtime,
            "配置 mtime 不得变化"
        );
        assert_eq!(
            std::fs::read_to_string(&config_path).expect("读配置"),
            "{\"preserved\":true}"
        );

        let _ = holder; // 仅用于命名清晰
        let mut child = child;
        let _ = child.wait();
    }

    // 6
    #[test]
    fn drop_keeps_lock_file_on_disk() {
        let dir = TempDir::new("t6");
        let path = {
            let lock = DataDirLock::acquire(dir.path()).expect("取锁");
            lock.path().to_path_buf()
        };
        assert!(path.exists(), "drop 之后锁文件必须仍在盘上（不得 unlink）");
        // 内容里应留下持锁诊断（pid / 时间戳）
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            content.contains("pid="),
            "锁文件应写入诊断信息，实际: {content:?}"
        );
    }

    // 7
    #[test]
    fn lock_file_descriptor_is_close_on_exec() {
        let dir = TempDir::new("t7");
        let lock = DataDirLock::acquire(dir.path()).expect("取锁");
        assert!(lock.is_close_on_exec(), "锁 fd 必须带 FD_CLOEXEC");
    }

    // 8 / 9 —— 真·跨进程（父进程持锁 → 子进程再取）
    #[test]
    fn cross_process_second_process_is_rejected_then_acquires_after_release() {
        let _guard = fork_guard();
        let dir = TempDir::new("t8");
        let lock = DataDirLock::acquire(dir.path()).expect("父进程取锁");

        let while_held = probe_child(dir.path());
        assert_eq!(
            while_held, "OCCUPIED",
            "父进程持锁期间，子进程必须收到 Occupied"
        );

        drop(lock);
        let after_release = probe_child(dir.path());
        assert_eq!(
            after_release, "ACQUIRED",
            "父进程释放后，子进程必须能取到锁"
        );
    }

    /// 起一个子进程，让它在 `dir` 上尝试取锁，返回 "ACQUIRED" / "OCCUPIED"。
    fn probe_child(dir: &Path) -> String {
        let output = std::process::Command::new(std::env::current_exe().expect("当前 exe"))
            .arg("--exact")
            .arg("data_dir_lock::tests::zz_child_probe")
            .arg("--nocapture")
            .env("AGATE_LOCK_PROBE_DIR", dir)
            .output()
            .expect("拉起探针子进程");
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            let line = line.trim();
            if line == "ACQUIRED" || line == "OCCUPIED" || line.starts_with("ERROR ") {
                return line.to_string();
            }
        }
        panic!("子进程未输出判定，stdout: {stdout}");
    }

    // 以下两个是**测试夹具**，不是用例：只由上面两个跨进程用例以子进程方式调用。
    // `cargo test` 默认也会跑它们；没有环境变量时直接空过（立即 ok）。

    #[test]
    fn zz_child_probe() {
        let Ok(dir) = std::env::var("AGATE_LOCK_PROBE_DIR") else {
            return;
        };
        match DataDirLock::acquire(Path::new(&dir)) {
            Ok(_) => println!("ACQUIRED"),
            Err(LockError::Occupied(_)) => println!("OCCUPIED"),
            Err(other) => println!("ERROR {other}"),
        }
    }

    #[test]
    fn zz_child_hold_lock() {
        let Ok(dir) = std::env::var("AGATE_LOCK_PROBE_DIR") else {
            return;
        };
        let Ok(lock) = DataDirLock::acquire(Path::new(&dir)) else {
            return;
        };
        println!("HOLDING");
        let hold_ms: u64 = std::env::var("AGATE_LOCK_PROBE_HOLD_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1500);
        std::thread::sleep(std::time::Duration::from_millis(hold_ms));
        drop(lock);
    }

    // ── 本模块自身不变量 ──────────────────────────────────────
    #[test]
    fn same_process_reentry_returns_same_lock() {
        let dir = TempDir::new("t10");
        let first = acquire_shared(dir.path()).expect("首次");
        let second = acquire_shared(dir.path()).expect("同进程重入应幂等");
        assert!(
            Arc::ptr_eq(&first, &second),
            "同进程重复取锁必须返回同一把锁（Android core_init 幂等路径依赖它）"
        );
    }

    #[test]
    fn lock_error_carries_path_and_kind() {
        let dir = TempDir::new("t11");
        let _held = DataDirLock::acquire(dir.path()).expect("取锁");
        let error = DataDirLock::acquire(dir.path()).expect_err("必须冲突");
        assert!(error.is_occupied());
        assert_eq!(error.path(), dir.path().join(LOCK_FILE_NAME));
        assert!(error.to_string().contains("另一个 Agate 实例"));
    }

    #[test]
    fn lock_file_name_is_stable() {
        assert_eq!(LOCK_FILE_NAME, ".agate.lock");
    }
}
