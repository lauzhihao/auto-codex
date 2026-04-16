# scodex

[English](./README.md) | [简体中文](./README.zh-CN.md)

`scodex` 会选择当前“立刻可用额度”最合适的 Codex 账号，切换 `~/.codex/auth.json`，然后启动或恢复 Codex。

这个仓库只包含代码，不包含账号池数据、额度缓存、本地配置或虚拟环境文件。

现在只有 Rust 二进制发行线仍在维护。仓库里保留的 Python 实现只作为迁移参考，不再作为安装路径，也不再继续维护。

如果你不喜欢或不习惯使用命令行，可以体验功能更丰富的 GUI 版本：<https://github.com/murongg/ai-accounts-hub>

## 安装

```bash
curl -fsSL https://raw.githubusercontent.com/lauzhihao/scodex/main/install.sh | bash
```

Windows PowerShell：

```powershell
irm https://raw.githubusercontent.com/lauzhihao/scodex/main/install.ps1 | iex
```

当前预编译发行目标：

- Linux：`x86_64-unknown-linux-musl`
- macOS：`x86_64-apple-darwin`、`aarch64-apple-darwin`
- Windows：`x86_64-pc-windows-msvc`

安装脚本会：

- 从 GitHub Releases 下载预编译的 Rust 二进制
- 安装 `scodex` 作为主命令
- 保留 `auto-codex` 作为兼容入口
- 安装 `scodex-original` 作为到底层 `codex` 的透传辅助命令
- 在存在 `~/.codex/auth.json` 时导入到本地状态
- 在额度接口可用时刷新使用量缓存

## 依赖

- `codex`
- Unix 安装器：`bash`、`curl`、`tar`
- Windows 安装器：PowerShell 5+ 或 PowerShell 7+

源码构建：

```bash
cargo build --release
```

## 入口命令

- `scodex`：主命令
- `auto-codex`：历史兼容 wrapper
- `scodex-original`：到底层官方 Codex CLI 的透传辅助命令
- `codex`：官方 Codex CLI 命令，安装器不会接管

## 命令总览

默认使用 `scodex`。保留 `auto-codex` 只是为了兼容旧习惯，不再作为文档主名。

| 命令 | 作用 |
| --- | --- |
| `scodex` | 刷新额度；如果当前账号的 5h 剩余额度至少还有 20% 就继续使用它，否则切换到最佳账号，然后启动或恢复 Codex |
| `scodex launch` | 默认行为的显式写法 |
| `scodex auto` | 刷新额度；如果当前账号的 5h 剩余额度至少还有 20% 就继续使用它，否则切换到最佳账号，但不启动 Codex |
| `scodex login` | 通过 `codex login --device-auth` 添加一个账号 |
| `scodex use <email>` | 按邮箱直接切换到一个已知账号 |
| `scodex list` | 先刷新实时额度，再显示最新账号额度 |
| `scodex refresh` | 刷新所有已知账号的实时额度，并直接打印最新结果 |
| `scodex import-auth <path>` | 导入一个 `auth.json` 文件，或包含 `auth.json` 的目录 |
| `scodex import-known` | 导入 `~/.codex/auth.json`；可选导入 AI Accounts Hub 管理的账号 |
| `scodex update` | 从 GitHub Releases 下载当前平台匹配的 Rust 发行资产并替换已安装二进制（`upgrade` 为兼容别名） |

## 支持的参数

### 全局参数

- `--state-dir <path>`：覆盖本地状态目录
- `-h`、`--help`：显示帮助

### `launch`

```bash
scodex launch [--no-import-known] [--no-login] [--dry-run] [--no-resume] [--no-launch] [<codex 参数...>]
```

- `--no-import-known`：跳过自动导入 `~/.codex/auth.json`
- `--no-login`：当没有可用账号时，不自动发起设备登录
- `--dry-run`：只打印会选哪个账号，不执行切换和启动
- `--no-resume`：总是新开会话，不执行 `resume --last`
- `--no-launch`：只切换账号，不启动 Codex
- 命令后面的额外参数会继续传给 Codex
- 刷新后，如果当前账号的 5h 剩余额度仍然不少于 20%，`launch` 会直接继续使用它，不再重新对所有账号打分选号

### `auto`

```bash
scodex auto [--no-import-known] [--no-login] [--dry-run]
```

- 会刷新额度；如果当前账号的 5h 剩余额度至少还有 20% 就继续使用它，否则切换到最佳账号
- 不会启动 Codex

### `login`

```bash
scodex login [--switch]
```

- `--switch`：登录完成后立即切换到新账号

### `use`

```bash
scodex use <email>
```

- 会按邮箱大小写不敏感精确匹配已知账号，并直接切换过去
- 示例：`scodex use lauzhihao@qq.com`

### `list`

```bash
scodex list
```

- 会先调用实时额度接口，再打印最新的账号额度快照

### `refresh`

```bash
scodex refresh
```

- 会对所有已知账号调用实时额度接口
- 刷新完成后会立刻打印最新账号列表
- 当前 Rust 发行版仍按顺序刷新账号额度

### `import-auth`

```bash
scodex import-auth <path>
```

- `<path>` 可以是 `auth.json` 文件，也可以是其父目录

### `import-known`

```bash
scodex import-known
```

- 默认导入 `~/.codex/auth.json`
- 如果还想导入 AI Accounts Hub 管理的 Codex homes，可以这样执行：

```bash
AUTO_CODEX_IMPORT_ACCOUNTS_HUB=1 scodex import-known
```

### `update`

```bash
scodex update [-f|--force]
scodex upgrade [-f|--force]
```

- 会从 GitHub Releases 下载当前平台匹配的最新发行资产并替换已安装二进制
- `update` 仍然是主命令，用来兼容历史 Python 版本用户
- `upgrade` 是等价别名，给更偏好这个命名的用户使用
- `-f`、`--force`：即使当前版本已经等于最新 tag，也强制重新安装一次

## 透传行为

如果第一个非全局参数不是文档里列出的子命令，`scodex` 会在完成账号选择后，把它当成官方 Codex CLI 命令继续执行。

例如：

```bash
scodex resume --last
scodex exec "fix failing test"
```

这也是为什么 `scodex resume --last` 可以工作，尽管 `resume` 并不是 `scodex` 自己声明的子命令。

## 选号说明

- 刷新额度时会调用实时 usage API，不只是读本地缓存
- 选号时优先看 `5h` 窗口剩余额度，再看 weekly 额度
- 目标是优先挑出“下一次会话最可能立刻可用”的账号

## 发布检查清单

推送前：

1. 执行 `rg -n 'access_token|refresh_token|id_token|OPENAI_API_KEY|account_id|@qq\\.com|/Users/ncds|/Users/liuzhihao' .`
2. 确认 `git status --short` 里只有代码和文档改动
3. 在推送前检查 `git diff --cached`

## 发布说明

- CI 现在只维护 Rust 实现。
- `v*` 标签会通过 GitHub Actions 发布预编译二进制。
- 仓库中的 Python 文件已降级为 legacy 参考代码，不再继续维护。
