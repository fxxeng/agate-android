//! 宿主平台能力判定（不承载具体 Android API）。
//!
//! 这里只表达 Rust Core 可稳定判断的 OS 级语义。Provider 是否真的支持某种
//! 桌面客户端导入仍由各 Provider 自己决定；本模块只阻止 Android 误走桌面固定路径。

/// `importDesktop` 表示读取当前宿主机上的桌面客户端固定路径，不等于用户显式选择
/// 文件导入。Android 没有这些桌面客户端目录语义；文件导入 / 手动粘贴仍可用。
pub fn desktop_import_platform_supported_for(os: &str) -> bool {
    matches!(os, "windows" | "macos" | "linux")
}

/// 是否应拒绝隐式桌面导入。显式文件导入优先级更高，即使 payload 同时携带
/// `importDesktop`，也不应被平台门禁误拦。
pub fn should_reject_desktop_import(
    os: &str,
    import_desktop: bool,
    explicit_file_import: bool,
) -> bool {
    import_desktop
        && !explicit_file_import
        && !desktop_import_platform_supported_for(os)
}

#[cfg(test)]
mod tests {
    use super::{desktop_import_platform_supported_for, should_reject_desktop_import};

    #[test]
    fn desktop_import_is_blocked_only_outside_supported_desktop_hosts() {
        assert!(desktop_import_platform_supported_for("windows"));
        assert!(desktop_import_platform_supported_for("macos"));
        assert!(desktop_import_platform_supported_for("linux"));
        assert!(!desktop_import_platform_supported_for("android"));
        assert!(!desktop_import_platform_supported_for("ios"));
    }

    #[test]
    fn explicit_file_import_takes_precedence_over_desktop_import_flag() {
        assert!(should_reject_desktop_import("android", true, false));
        assert!(!should_reject_desktop_import("android", true, true));
        assert!(!should_reject_desktop_import("android", false, false));
        assert!(!should_reject_desktop_import("windows", true, false));
    }
}
