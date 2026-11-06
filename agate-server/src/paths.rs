//! 配置目录与相关路径的**唯一事实来源**。
//!
//! 拆分前这段实现住在桌面 crate 的 `gateway.rs`（`crate::gateway::config_dir`
//! 是唯一事实来源，`server::config` 只做转发）。网关拆成独立 crate 后事实
//! 来源随迁到这里：`paths::config_dir` 是唯一实现，桌面侧 `gateway::config_dir`
//! 反过来转发本模块 —— 「两条路径、一个实现」的约定不变，只是方向反了。
//!
//! ── 环境变量 ────────────────────────────────────────────────
//! `AGATE_PROXY_HOME` 优先，旧名 `WORKBUDDY_PROXY_HOME` 兼容读（1.x 的
//! 启动脚本/快捷方式里可能还留着旧名）。Docker 部署把它指到挂载卷
//! （compose 示例：`AGATE_PROXY_HOME=/data`），数据即可留在容器外。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 平台路径能力的最小边界。
///
/// 当前 Desktop 与 headless 都遵循环境变量约定；Android 不伪造 HOME，
/// 而是在 Core bootstrap 前通过 JNI 设置 config_dir override。
/// Provider 只负责读取宿主环境，不创建目录、不写环境变量，也不缓存可变状态。
pub(crate) trait PathProvider: Send + Sync {
    fn config_dir_override(&self) -> Option<PathBuf>;
    fn user_home_dir(&self) -> Option<PathBuf>;
}

/// 当前共享 Core 的环境路径适配器。
///
/// Windows 优先 USERPROFILE，其他宿主也兼容 USERPROFILE 后回退 HOME，
/// 保持既有桌面/ headless 路径顺序。后续若 Desktop Host 需要显式注入
/// 路径，可在宿主边界设置 config_dir override，而不是改写进程环境。
struct EnvironmentPathProvider;

impl PathProvider for EnvironmentPathProvider {
    fn config_dir_override(&self) -> Option<PathBuf> {
        resolve_config_dir_override(
            std::env::var("AGATE_PROXY_HOME").ok().as_deref(),
            std::env::var("WORKBUDDY_PROXY_HOME").ok().as_deref(),
        )
    }

    fn user_home_dir(&self) -> Option<PathBuf> {
        resolve_user_home(
            std::env::var("USERPROFILE").ok().as_deref(),
            std::env::var("HOME").ok().as_deref(),
        )
    }
}

static ENVIRONMENT_PATH_PROVIDER: EnvironmentPathProvider = EnvironmentPathProvider;

fn path_provider() -> &'static dyn PathProvider {
    &ENVIRONMENT_PATH_PROVIDER
}

/// Android 由 JNI 在 Core bootstrap 前注入应用私有目录。
///
/// 不再通过 `std::env::set_var` 修改进程全局环境：Rust 运行时可能已经有
/// Tokio worker、日志线程或其他请求线程并发读取环境变量；进程级 env 写入
/// 会把 Android 生命周期配置变成全局可变状态。该覆盖只设置一次，桌面端
/// 不调用它，仍沿用环境变量和默认目录语义。
static CONFIG_DIR_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

/// 设置进程级配置目录覆盖。重复设置相同路径视为幂等；不同路径拒绝覆盖，
/// 因为 Core 是进程单例，运行中切换数据目录会让不同子系统读写两套数据。
pub fn set_config_dir_override(path: impl Into<PathBuf>) -> Result<(), String> {
    let path = path.into();
    validate_config_dir_path(&path)?;
    match CONFIG_DIR_OVERRIDE.set(path.clone()) {
        Ok(()) => Ok(()),
        Err(_) if CONFIG_DIR_OVERRIDE.get() == Some(&path) => Ok(()),
        Err(_) => Err(format!(
            "配置目录已初始化为 {}，不能切换到 {}",
            CONFIG_DIR_OVERRIDE.get().map(|value| value.display().to_string()).unwrap_or_default(),
            path.display(),
        )),
    }
}

/// JNI 注入路径的基本校验。不要在这里 canonicalize：首次启动时目录可能
/// 尚不存在，且 canonicalize 会把“目录创建”职责错误地推到宿主边界之外。
fn validate_config_dir_path(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty() {
        return Err("配置目录不能为空".into());
    }
    if !path.is_absolute() {
        return Err(format!("配置目录必须是绝对路径: {}", path.display()));
    }
    if path.to_string_lossy().contains('\0') {
        return Err("配置目录不能包含 NUL 字符".into());
    }
    Ok(())
}

/// 配置目录：Android 显式覆盖优先，其次环境变量，最后 `{用户主目录}/.agate`。
pub fn config_dir() -> PathBuf {
    CONFIG_DIR_OVERRIDE
        .get()
        .cloned()
        .or_else(|| path_provider().config_dir_override())
        .unwrap_or_else(default_config_dir)
}

/// 返回可用于定位旧桌面数据的用户主目录。
///
/// 与 `default_config_dir()` 有意不同：这里**不回退到当前工作目录 `.`**。
/// Android 服务的工作目录不是用户主目录；把 `.` 拼成 `~/.xxx` 会让启动期
/// 导入器误扫应用工作目录，并可能把「没有找到旧文件」永久记成已迁移。
pub fn user_home_dir() -> Option<PathBuf> {
    path_provider().user_home_dir()
}

/// 旧版 `.workbuddy-proxy` 配置目录。
///
/// 没有有效的绝对用户主目录时返回 None；Android 应用工作目录不是桌面用户
/// 主目录，不能回退到 `.` 扫描或迁移旧桌面数据。
pub(crate) fn legacy_config_dir() -> Option<PathBuf> {
    legacy_config_dir_from_home(user_home_dir())
}

fn legacy_config_dir_from_home(home: Option<PathBuf>) -> Option<PathBuf> {
    home.map(|home| home.join(".workbuddy-proxy"))
}

fn resolve_user_home(userprofile: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    [userprofile, home]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .find(|path| path.is_absolute() && !path.to_string_lossy().contains('\0'))
}

/// 是否存在显式配置目录覆盖（Android JNI 注入或桌面/headless 环境变量）。
/// 配置迁移需要据此判断用户是否已经指定独立数据目录。
pub fn has_config_dir_override() -> bool {
    CONFIG_DIR_OVERRIDE.get().is_some() || path_provider().config_dir_override().is_some()
}

/// 解析配置目录覆盖：新变量优先，旧变量只作为兼容回退；空白视为未设置。
fn resolve_config_dir_override(primary: Option<&str>, legacy: Option<&str>) -> Option<PathBuf> {
    [primary, legacy]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// 默认配置目录：`{用户主目录}/.agate`
fn default_config_dir() -> PathBuf {
    path_provider()
        .user_home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".agate")
}

/// 旧桌面设置文件的路径（`{config_dir}/desktop-settings.json`）。
///
/// 拆分前由桌面 crate 的 `settings::legacy_file_path()` 提供；本 crate 的
/// 一次性迁移（`db::migrate::settings`）要定位这个文件，而 headless 下
/// 桌面 crate 根本不参与 —— 所以路径事实来源随 `config_dir` 一起迁到本
/// 模块，桌面侧的 `settings::legacy_file_path()` 反过来转发这里。
///
/// 文件名是历史事实：这个文件已不再是设置的真相来源（进了 SQLite 的
/// `kv` 表），名字只被迁移与回落读路径引用，不会随代码演进变化。
pub fn legacy_desktop_settings_file() -> PathBuf {
    config_dir().join("desktop-settings.json")
}

#[cfg(test)]
mod tests {
    use super::{legacy_config_dir_from_home, resolve_config_dir_override, resolve_user_home, validate_config_dir_path};
    use std::path::{Path, PathBuf};

    #[test]
    fn config_dir_requires_absolute_non_empty_path() {
        assert!(validate_config_dir_path(Path::new("/data/user/0/dev.agate/files/agate")).is_ok());
        assert!(validate_config_dir_path(Path::new("")).is_err());
        assert!(validate_config_dir_path(Path::new("relative/agate")).is_err());
    }

    #[test]
    fn legacy_home_requires_an_absolute_non_empty_path() {
        assert_eq!(
            resolve_user_home(Some("/data/user/0/dev.agate"), None),
            Some(PathBuf::from("/data/user/0/dev.agate")),
        );
        assert_eq!(
            resolve_user_home(None, Some("/home/agate")),
            Some(PathBuf::from("/home/agate")),
        );
        assert_eq!(resolve_user_home(None, None), None);
        assert_eq!(resolve_user_home(Some("  "), None), None);
        assert_eq!(resolve_user_home(None, Some("relative/home")), None);
        assert_eq!(
            resolve_user_home(Some("relative/profile"), Some("/home/fallback")),
            Some(PathBuf::from("/home/fallback")),
        );
    }

    #[test]
    fn user_home_prefers_userprofile_then_home_and_rejects_relative_values() {
        assert_eq!(
            resolve_user_home(Some("/profile/agate"), Some("/home/agate")),
            Some(PathBuf::from("/profile/agate")),
        );
        assert_eq!(
            resolve_user_home(Some("relative/profile"), Some("/home/agate")),
            Some(PathBuf::from("/home/agate")),
        );
        assert_eq!(resolve_user_home(Some("relative/profile"), None), None);
    }

    #[test]
    fn legacy_config_dir_requires_a_valid_user_home() {
        assert_eq!(
            legacy_config_dir_from_home(Some(PathBuf::from("/home/agate"))),
            Some(PathBuf::from("/home/agate/.workbuddy-proxy")),
        );
        assert_eq!(legacy_config_dir_from_home(None), None);
    }

    #[test]
    fn config_dir_override_prefers_new_name_and_falls_back_to_legacy() {
        assert_eq!(
            resolve_config_dir_override(Some("/data/agate"), Some("/old/agate")),
            Some(PathBuf::from("/data/agate")),
        );
        assert_eq!(
            resolve_config_dir_override(Some("  "), Some("/old/agate")),
            Some(PathBuf::from("/old/agate")),
        );
        assert_eq!(resolve_config_dir_override(Some(" "), Some("")), None);
    }

    #[test]
    fn config_dir_rejects_nul_character() {
        assert!(validate_config_dir_path(Path::new("/data/agate\0broken")).is_err());
    }
}
