//! 网关启动失败的分类与描述（尤其是端口冲突）。
//!
//! ── 为什么需要它 ────────────────────────────────────────────
//! 网关默认监听 127.0.0.1:8787（可在设置页改），启动时 bind 失败是用户最容易
//! 撞上的故障。但「失败」有两种性质完全不同的原因，处置方式正好相反：
//!   · 端口被**另一个进程**占着（Linux `EADDRINUSE` → `ErrorKind::AddrInUse`）
//!     —— 换端口，或结束占用该端口的程序；
//!   · 端口被**系统拒绝**（`EACCES` → `ErrorKind::PermissionDenied`）：监听
//!     特权端口（<1024）或被 SELinux 策略拦下——本机有没有进程监听都一样，
//!     结束进程无从谈起，只能换端口。
//! 界面若只给一句「端口被占用」，第二种情况的用户会去找一个并不存在的进程；
//! 反过来把第一种说成「系统保留」，又会让人白白放弃一条有效的出路。
//!
//! ── 判据为什么是错误码 ──────────────────────────────────────
//! `ErrorKind` 已把两类失败映射成不同的值，bind 出错的那一刻就能拿到；
//! 扫描 /proc/net/tcp 反推监听进程既不可靠（多网络命名空间、受限读法随时
//! 会变），也没有必要——Android 壳侧的处置只有「换端口」，占用者是谁不影响
//! 用户该做的事。
//!
//! ── 本模块只描述，不动作 ────────────────────────────────────
//! 桌面版的「查监听进程 / 结束进程树」（Win32）及其数据结构（Occupant 等）
//! 已随 Android-only 主线删除；Android 壳侧从不查占用者，文案一律按
//! 「查不到占用者」生成。把事实翻译成「性质 + 处置建议」的职责不变：
//! 界面不能自己拼文案，同一件事在通知、卡片、日志里说法不一致时，
//! 用户无从判断哪个是真的。

use serde::Serialize;

/// 端口冲突的性质。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConflictKind {
    /// 有另一个进程在监听：结束那个进程就能把端口拿回来。
    Occupied,
    /// 被系统保留 / 被安全策略拒绝：本机没有任何进程在监听，只能换端口。
    Reserved,
    /// 其它 bind 失败原因（地址不可用、权限不足等）：没有确定的处置手段。
    Other,
}

impl ConflictKind {
    /// 机器可读标签（JNI → Kotlin `StartResult.kind` 用；与 serde kebab-case 一致）。
    pub fn as_str(&self) -> &'static str {
        match self {
            ConflictKind::Occupied => "occupied",
            ConflictKind::Reserved => "reserved",
            ConflictKind::Other => "other",
        }
    }
}

/// 一次端口冲突的完整描述。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortConflict {
    pub kind: ConflictKind,
    pub port: u16,
    /// 给人看的完整说明（含处置建议）
    pub message: String,
}

impl PortConflict {
    /// 从 bind 失败的错误构造冲突描述。
    ///
    /// Android 壳侧从不查占用进程：`Reserved` 本来就查不到，`Occupied`
    /// 的文案也一律按「查不到占用者」生成 —— 处置建议不依赖占用者是谁。
    pub fn from_bind_error(port: u16, error: &std::io::Error) -> Self {
        let kind = match error.kind() {
            std::io::ErrorKind::AddrInUse => ConflictKind::Occupied,
            std::io::ErrorKind::PermissionDenied => ConflictKind::Reserved,
            _ => ConflictKind::Other,
        };
        Self::new(kind, port, &error.to_string())
    }

    /// 组装冲突描述（文案集中在这里，避免界面与后端各说各话）。
    pub fn new(kind: ConflictKind, port: u16, detail: &str) -> Self {
        Self { kind, port, message: describe(kind, port, detail) }
    }

    /// 侧栏那一行用的短标签。
    ///
    /// 与 `message` 分开是因为长度约束完全不同：状态条只有一行、还要带端口号，
    /// 完整说明（可能上百字）只能进 title 与弹窗。两处措辞都由后端给，
    /// 是为了避免界面自己拼一套说法、和后端日志里的说法对不上。
    pub fn label(&self) -> &'static str {
        match self.kind {
            ConflictKind::Occupied => "端口被占用",
            ConflictKind::Reserved => "端口被系统保留",
            ConflictKind::Other => "网关启动失败",
        }
    }
}

/// 按性质生成说明文案（Android 侧从不填占用者，Occupied 只有这一种口径）
fn describe(kind: ConflictKind, port: u16, detail: &str) -> String {
    match kind {
        ConflictKind::Occupied => {
            format!(
                "端口 {port} 已被其他应用占用。可结束占用该端口的程序，或改用其它端口（设置页）。"
            )
        }
        ConflictKind::Reserved => format!(
            "端口 {port} 被系统拒绝（保留段或权限不足，例如 1024 以下特权端口），\
             结束进程也无法解决。请改用其它端口（设置页）。"
        ),
        ConflictKind::Other => format!("端口 {port} 无法监听：{detail}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_as_str_matches_serde_kebab_case() {
        assert_eq!(ConflictKind::Occupied.as_str(), "occupied");
        assert_eq!(ConflictKind::Reserved.as_str(), "reserved");
        assert_eq!(ConflictKind::Other.as_str(), "other");
    }

    #[test]
    fn android_copy_for_occupied_names_the_two_way_out() {
        let conflict = PortConflict::new(ConflictKind::Occupied, 8787, "os error");
        assert!(conflict.message.contains("8787"));
        assert!(conflict.message.contains("端口"));
        assert!(conflict.message.contains("其它端口") || conflict.message.contains("其他端口"));
        // 桌面版「查不到进程（可能是权限不足）」的误导文案不再出现
        assert!(!conflict.message.contains("权限不足"));
    }

    #[test]
    fn android_copy_for_reserved_never_mentions_windows_reservation() {
        let conflict = PortConflict::new(ConflictKind::Reserved, 80, "os error 13");
        assert!(conflict.message.contains("其它端口"));
        assert!(!conflict.message.contains("Hyper-V"));
        assert!(!conflict.message.contains("winnat"));
        assert!(!conflict.message.contains("WSL2"));
    }
}
