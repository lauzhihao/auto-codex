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

pub(super) fn find_in_path(binary: &str) -> Option<PathBuf> {
    let path_var = env::var_os("PATH")?;
    for dir in env::split_paths(&path_var) {
        let candidate = dir.join(binary);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

pub(super) fn find_runtime_program(binary: &str) -> Option<PathBuf> {
    let path_var = env::var_os("PATH")?;
    for dir in env::split_paths(&path_var) {
        let candidate = dir.join(binary);
        if candidate.exists() && runtime_program_allowed(&candidate) {
            return Some(candidate);
        }
    }
    None
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

pub(super) fn find_program(candidates: &[&str]) -> Option<PathBuf> {
    candidates
        .iter()
        .find_map(|candidate| find_in_path(candidate))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{codex_install_command, is_windows_interop_path, wsl_detected_from_signals};

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
