use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InstallCommand {
    pub(super) program: String,
    pub(super) args: Vec<String>,
}

impl InstallCommand {
    pub(super) fn display(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

pub(super) fn codex_home() -> PathBuf {
    if let Some(home) = env::var_os("CODEX_HOME") {
        PathBuf::from(home)
    } else if let Some(home) = env::var_os("HOME") {
        PathBuf::from(home).join(".codex")
    } else {
        PathBuf::from(".codex")
    }
}

pub(super) fn codex_install_command() -> InstallCommand {
    InstallCommand {
        program: npm_command_name().to_string(),
        args: vec!["install".into(), "-g".into(), "@openai/codex".into()],
    }
}

fn npm_command_name() -> &'static str {
    if cfg!(windows) { "npm.cmd" } else { "npm" }
}

pub(super) fn find_codex_bin() -> Option<PathBuf> {
    if let Some(env) = env::var_os("CODEX_BIN") {
        let path = PathBuf::from(env);
        if path.exists() {
            return Some(path);
        }
    }

    for candidate in codex_binary_names() {
        if let Some(path) = find_runtime_program(candidate) {
            return Some(path);
        }
    }

    if let Some(home) = env::var_os("HOME") {
        let home = PathBuf::from(home);
        for candidate in codex_home_binary_candidates(&home) {
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    npm_global_codex_bin()
}

fn codex_binary_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["codex.cmd", "codex.exe", "codex.bat", "codex"]
    } else {
        &["codex"]
    }
}

fn codex_home_binary_candidates(home: &Path) -> Vec<PathBuf> {
    if cfg!(windows) {
        vec![
            home.join("AppData")
                .join("Roaming")
                .join("npm")
                .join("codex.cmd"),
            home.join("AppData")
                .join("Roaming")
                .join("npm")
                .join("codex.exe"),
        ]
    } else {
        vec![home.join(".local").join("bin").join("codex")]
    }
}

fn npm_global_codex_bin() -> Option<PathBuf> {
    let npm = if cfg!(windows) {
        find_in_path("npm.cmd")
            .or_else(|| find_in_path("npm.exe"))
            .or_else(|| find_in_path("npm"))
    } else {
        find_runtime_program("npm")
    }?;

    let output = Command::new(npm).args(["prefix", "-g"]).output().ok()?;
    if !output.status.success() {
        return None;
    }

    let prefix = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if prefix.is_empty() {
        return None;
    }

    let prefix = PathBuf::from(prefix);
    let candidates = if cfg!(windows) {
        vec![prefix.join("codex.cmd"), prefix.join("codex.exe")]
    } else {
        vec![prefix.join("bin").join("codex")]
    };

    candidates
        .into_iter()
        .find(|path| path.exists() && runtime_program_allowed(path))
}

/// 在 PATH 中搜索 binary，只返回通过 filter 的第一个候选路径。
/// filter 为 None 时不做额外过滤（相当于原 find_in_path 行为）。
fn find_in_path_filtered<F>(binary: &str, filter: Option<F>) -> Option<PathBuf>
where
    F: Fn(&Path) -> bool,
{
    let path_var = env::var_os("PATH")?;
    for dir in env::split_paths(&path_var) {
        let candidate = dir.join(binary);
        if candidate.exists() {
            if filter.as_ref().is_none_or(|f| f(&candidate)) {
                return Some(candidate);
            }
        }
    }
    None
}

/// 在 PATH 中搜索 binary，不过滤（原 find_in_path 语义）。
fn find_in_path(binary: &str) -> Option<PathBuf> {
    find_in_path_filtered::<fn(&Path) -> bool>(binary, None)
}

/// 在 PATH 中搜索 binary，跳过不可在当前运行时直接执行的路径（如 WSL 下的 Windows 工具）。
pub(super) fn find_runtime_program(binary: &str) -> Option<PathBuf> {
    find_in_path_filtered(binary, Some(runtime_program_allowed))
}

fn runtime_program_allowed(path: &Path) -> bool {
    // WSL 可能继承 Windows PATH；这些命令不能当作 Linux runtime 工具直接执行。
    !(running_under_wsl() && is_windows_interop_path(path))
}

fn running_under_wsl() -> bool {
    wsl_detected_from_signals(
        env::var("WSL_INTEROP").ok().as_deref(),
        env::var("WSL_DISTRO_NAME").ok().as_deref(),
        fs::read_to_string("/proc/sys/kernel/osrelease")
            .ok()
            .as_deref()
            .unwrap_or(""),
        fs::read_to_string("/proc/version")
            .ok()
            .as_deref()
            .unwrap_or(""),
    )
}

fn wsl_detected_from_signals(
    wsl_interop: Option<&str>,
    wsl_distro_name: Option<&str>,
    osrelease: &str,
    version: &str,
) -> bool {
    if wsl_interop.is_some_and(|value| !value.is_empty())
        || wsl_distro_name.is_some_and(|value| !value.is_empty())
    {
        return true;
    }

    let osrelease = osrelease.to_ascii_lowercase();
    let version = version.to_ascii_lowercase();
    osrelease.contains("microsoft")
        || osrelease.contains("wsl")
        || version.contains("microsoft")
        || version.contains("wsl")
}

fn is_windows_interop_path(path: &Path) -> bool {
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "bat" | "cmd" | "exe"))
    {
        return true;
    }

    let normalized = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let Some(rest) = normalized.strip_prefix("/mnt/") else {
        return false;
    };
    let bytes = rest.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b'/'
}

/// 在 PATH 中按优先级顺序搜索多个候选名称，不过滤运行时路径。
/// 被 deploy.rs 和 repo_sync.rs 用于定位 ssh/scp/git 等系统工具。
pub(super) fn find_program(candidates: &[&str]) -> Option<PathBuf> {
    candidates
        .iter()
        .find_map(|candidate| find_in_path(candidate))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        codex_install_command, find_in_path_filtered, is_windows_interop_path,
        wsl_detected_from_signals,
    };

    #[test]
    fn install_command_uses_official_npm_package() {
        let command = codex_install_command();
        assert!(command.program == "npm" || command.program == "npm.cmd");
        assert_eq!(command.args, vec!["install", "-g", "@openai/codex"]);
    }

    #[test]
    fn wsl_detection_uses_env_or_kernel_markers() {
        assert!(wsl_detected_from_signals(
            Some("/run/WSL/123_interop"),
            None,
            "",
            ""
        ));
        assert!(wsl_detected_from_signals(None, Some("Ubuntu"), "", ""));
        assert!(wsl_detected_from_signals(
            None,
            None,
            "5.15.90.1-microsoft-standard-WSL2",
            ""
        ));
        assert!(!wsl_detected_from_signals(
            None,
            None,
            "6.8.0-generic",
            "Linux version 6.8.0"
        ));
    }

    // --- find_in_path_filtered 单元测试 ---

    /// 在 base 下创建子目录 subdir，并在其中写入 name 文件（Unix 下设置可执行位）。
    fn setup_bin(base: &std::path::Path, subdir: &str, name: &str) -> std::path::PathBuf {
        let dir = base.join(subdir);
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join(name);
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        dir
    }

    #[test]
    fn find_in_path_filtered_returns_none_when_not_found() {
        // PATH 指向一个空临时目录，搜索结果应为 None。
        let base = std::env::temp_dir().join("scodex_test_none");
        std::fs::create_dir_all(&base).unwrap();
        let old_path = std::env::var_os("PATH").unwrap_or_default();
        // SAFETY: 单线程测试，set_var 不会引起数据竞争。
        unsafe { std::env::set_var("PATH", &base) };
        let result = find_in_path_filtered::<fn(&Path) -> bool>("no_such_binary_xyz_scodex", None);
        unsafe { std::env::set_var("PATH", &old_path) };
        let _ = std::fs::remove_dir_all(&base);
        assert!(result.is_none());
    }

    #[test]
    fn find_in_path_filtered_hits_first_dir_without_filter() {
        // 两个目录都有同名文件，无 filter 时应命中第一个。
        let base = std::env::temp_dir().join("scodex_test_first");
        let dir1 = setup_bin(&base, "d1", "mybin_scodex");
        let dir2 = setup_bin(&base, "d2", "mybin_scodex");

        let path_val = std::env::join_paths([dir1.as_path(), dir2.as_path()]).unwrap();
        let old_path = std::env::var_os("PATH").unwrap_or_default();
        // SAFETY: 单线程测试，set_var 不会引起数据竞争。
        unsafe { std::env::set_var("PATH", &path_val) };
        let result = find_in_path_filtered::<fn(&Path) -> bool>("mybin_scodex", None).unwrap();
        unsafe { std::env::set_var("PATH", &old_path) };
        let _ = std::fs::remove_dir_all(&base);

        assert_eq!(result, dir1.join("mybin_scodex"));
    }

    #[test]
    fn find_in_path_filtered_skips_rejected_dir_and_hits_second() {
        // filter 拒绝第一个目录中的文件，应命中第二个目录。
        let base = std::env::temp_dir().join("scodex_test_skip");
        let dir1 = setup_bin(&base, "d1", "mybin_scodex");
        let dir2 = setup_bin(&base, "d2", "mybin_scodex");

        let first_dir = dir1.clone();
        let path_val = std::env::join_paths([dir1.as_path(), dir2.as_path()]).unwrap();
        let old_path = std::env::var_os("PATH").unwrap_or_default();
        // SAFETY: 单线程测试，set_var 不会引起数据竞争。
        unsafe { std::env::set_var("PATH", &path_val) };
        // filter: 拒绝位于 dir1 中的路径
        let result =
            find_in_path_filtered("mybin_scodex", Some(|p: &Path| !p.starts_with(&first_dir)));
        unsafe { std::env::set_var("PATH", &old_path) };
        let _ = std::fs::remove_dir_all(&base);

        assert_eq!(result.unwrap(), dir2.join("mybin_scodex"));
    }

    #[test]
    fn windows_interop_paths_are_detected() {
        assert!(is_windows_interop_path(Path::new(
            "/mnt/c/Users/me/AppData/Roaming/npm/codex"
        )));
        assert!(is_windows_interop_path(Path::new(
            "/mnt/c/Users/me/AppData/Roaming/npm/codex.cmd"
        )));
        assert!(is_windows_interop_path(Path::new("codex.exe")));
        assert!(!is_windows_interop_path(Path::new("/usr/local/bin/codex")));
        assert!(!is_windows_interop_path(Path::new(
            "/home/me/.local/bin/codex"
        )));
    }
}
