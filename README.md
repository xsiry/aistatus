# aistatus

终端版多账号 Codex/OpenAI 配额状态面板，目标是用接近 btop 的信息密度展示：

- 账号列表
- 5 小时额度百分比
- 周额度百分比
- 会员等级 / 账号类型
- 刷新状态 / 认证异常 / 协议漂移

当前实现优先支持 **Linux + macOS**，并以 **Codex 协议 v2** 作为订阅额度主数据源。

## 当前能力

- 多账号 profile 配置与默认账号标记
- `browser / headless / api_key` 三种登录材料模型
- native keyring + 加密文件 fallback secret store
- `doctor` 健康诊断
- Codex 协议适配与 schema drift 检测
- fixture 驱动的 ratatui TUI

## 明确区分的两类数据

### 1. ChatGPT / Codex 订阅额度

这是本项目的主展示对象：

- `5h` 百分比
- `weekly` 百分比
- `planType` / membership tier

这些来自 Codex app-server 协议适配层。

### 2. OpenAI API usage

这和 ChatGPT/Codex 订阅额度不是一回事。

项目里已经预留 `OpenAiApiUsageProvider` skeleton，但它目前只作为**单独 usage family** 存在，**不会**把 API usage 数值伪装成订阅额度。

## 安装

需要 Rust stable：

```bash
curl https://sh.rustup.rs -sSf | sh -s -- -y --default-toolchain stable
. "$HOME/.cargo/env"
```

## 常用命令

### 查看主命令

```bash
cargo run -- --help
```

### 运行 doctor（健康样例）

```bash
cargo run -- doctor --config .sisyphus/fixtures/sample-config.toml
```

预期输出会是 `overall: ok`。

### 运行 doctor（损坏 session 样例）

```bash
cargo run -- doctor --fixtures corrupted-session
```

预期输出会包含 `empty browser session payload`。

### 查看 profile 列表

```bash
cargo run -- profile list --config .sisyphus/fixtures/sample-config.toml
```

### 设置默认 profile

```bash
cargo run -- profile set-default acct-api-key --config .sisyphus/fixtures/sample-config.toml
```

### 添加 profile

```bash
cargo run -- profile add \
  --config /tmp/aistatus-config.toml \
  --id acct-plus \
  --name "Primary ChatGPT" \
  --auth-mode browser \
  --account-kind chatgpt \
  --provider codex_protocol \
  --membership-tier plus \
  --plan-type plus
```

### 登录材料写入（示例）

keyring backend：

```bash
cargo run -- profile login \
  --config /tmp/aistatus-config.toml \
  --id acct-plus \
  --auth-mode browser \
  --secret "session-cookie-value"
```

加密文件 backend：

```bash
cargo run -- profile login \
  --config /tmp/aistatus-config.toml \
  --id acct-api \
  --auth-mode api_key \
  --secret "sk-live-123" \
  --file-store
```

### 清除登录材料

```bash
cargo run -- profile logout acct-plus --config /tmp/aistatus-config.toml
```

### 运行 fixture TUI

```bash
cargo run -- tui --fixtures sample-quotas
```

这条命令会进入真实的交互式 TUI，会等待按键输入，所以适合本地人工检查，不适合直接放进 CI。

按键：

- `j` / `↓`：下一个账号
- `k` / `↑`：上一个账号
- `r`：标记刷新中状态
- `?`：显示/隐藏帮助
- `q`：退出

## 配置与 secrets

### Plain config

纯配置文件只存：

- `default_profile_id`
- profile 元数据
- `SecretRef`

不存：

- API key 明文
- browser/headless session payload

### Secret material

secret payload 通过以下方式保存：

- native keyring
- 加密文件 fallback
- fixture / sidecar `SecretMaterial` JSON（用于开发和 CI）

## Fixture 位置

- doctor fixtures: `.sisyphus/fixtures/`
- TUI fixture: `crates/tui/tests/fixtures/sample-quotas.json`
- core/config/provider fixtures: `crates/*/tests/fixtures/`

## 协议兼容策略

- 当前 Codex 适配层钉死到 `schema v2`
- 如果 schema 漂移：直接报 incompatibility，不静默降级
- 已知 `planType` 会归一化到稳定 membership buckets
- 未知 `planType` 保留 `raw_plan_type`

## 不支持项

当前**明确不支持**：

- Windows
- 抓取 ChatGPT 网页私有接口
- 把 OpenAI API usage 伪装成订阅额度
- 自动联动 OpenCode/Codex 外部会话切换
- 后台 daemon / 远程同步

## 本地验证

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo build --workspace
cargo run -- doctor --config .sisyphus/fixtures/sample-config.toml
cargo test -p aistatus-tui diagnostics_view_shows_refresh_feedback
```

其中：

- `cargo run -- doctor --config .sisyphus/fixtures/sample-config.toml` 是 doctor fixture smoke
- `cargo test -p aistatus-tui diagnostics_view_shows_refresh_feedback` 是非交互 TUI fixture smoke，它会加载 `sample-quotas` fixture，并验证诊断视图里的 refresh 反馈，不会启动会挂住的 raw-mode 会话
- `cargo run -- tui --fixtures sample-quotas` 仍然保留给本地人工操作验证

## CI

CI 会执行：

- fmt check
- clippy (`--workspace --all-targets --all-features -- -D warnings`)
- workspace tests
- workspace build
- doctor fixture smoke
- non-interactive TUI fixture smoke

对应 workflow 在：`.github/workflows/ci.yml`
