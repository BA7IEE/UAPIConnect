# 9 月新版 Codex 兼容修复

## 反馈与原因

用户在 Windows、9 月 4 日发布的 Codex 上反馈：U-API 的 `requires_openai_auth = false` 导致 401；改为 `true` 后实测恢复。中文已设置为 `zh-CN` 仍未生效；模型 `gpt-5.6` 缺少 Max／Ultra。

- 认证：生成器、配置归属检查都只接受旧的 `false`。只改生成器会让旧配置无法升级，也会误判用户手工修复过的配置。
- 语言：关闭完整页面增强后跳过语言兼容；此外，已有语言补丁只处理 Statsig `getDynamicConfig`。本地新版客户端通过 `getLayer("72216192")` 读取 `enable_i18n`，语言值正确不足以启用翻译。
- 推理：内置能力表只匹配 Sol／Terra／Luna，没有匹配无后缀 `gpt-5.6`。本地客户端还使用 `enabled-reasoning-efforts`、`show-ultra-in-model-picker-slider` 和 Ultra 显示 gate 过滤目录提供的档位。

[官方 GPT-5.6 Sol 说明](https://developers.openai.com/api/docs/models/gpt-5.6-sol) 明确 `gpt-5.6` 别名对应 Sol，API 支持 Max。Ultra 在这里沿用仓库已有的 **Codex 桌面能力定义**，不是据此宣称所有第三方 API 都接受 `ultra`；仍要实际验证 U-API 和 Windows 客户端组合。

## 实现边界

1. 新生成的固定 U-API provider 使用 `requires_openai_auth = true`。旧 `false` 和已修复 `true` 都可归属识别并规范化；固定地址、Responses 协议和 profile 身份校验保留，缺失/非布尔认证字段仍拒绝接管。密钥只在实时 `auth.json` 中使用，不放进配置、普通设置或日志。
2. `gpt-5.6` 复用 Sol 元数据，但 catalog 的 slug、实际请求模型名保留 `gpt-5.6`。不强制用户换成 `gpt-5.6-sol`，不为未知模型添加能力。
3. 完整页面增强仍关闭。受管发行版只加载 `uapi/desktop-compat.js`，复用已有 CDP 重连机制，不启动 HTTP helper，不加载用户脚本、菜单、主题或宠物驱动。桥接只允许 `/backend/status` 健康检查。
4. 语言补丁同时适配旧 `getDynamicConfig` 和新 `getLayer`，只改变翻译配置。推理补丁只允许 catalog 已声明的扩展档位通过原生显示过滤；Luna 不补 Ultra。
5. 设置通过客户端自身设置接口读写，记录变更前值；经 U-API Connect 切回官方模式并重启时，只恢复仍等于本工具写入值的推理设置，保留用户后来主动修改的值。此恢复需要兼容脚本运行，不能把直接退出或离线卸载当成已完成恢复。
6. 同一会话、同一兼容配置最多自动刷新一次；无法写恢复记录时不修改设置，无法写刷新标记时不自动刷新。设置接口或必要兼容点不可用时不报告完全就绪。

## 验证范围

本地结果：`cargo fmt --all -- --check`、全目标 `cargo check --workspace --all-targets --locked` 通过；全量 Rust 测试 1,387 通过、0 失败、2 项沿用上游忽略；发行策略测试 16 通过；U-API 启动器 release 构建和上游差异边界审计通过。首轮全量运行中旧模型列表模拟服务器发生超时，单独重跑及全量复跑均通过，未修改该测试来掩盖失败。随后增加的设置写后回读、短暂故障重试、冻结 SDK 对象检查也通过专项回归。

- Rust：新旧认证配置启动迁移、密钥保留、`gpt-5.6` catalog 的模型名及 Max／Ultra、切官方清理。
- 启动器：关闭完整增强仍运行精简兼容、不启动 helper，兼容失败显示 `running_degraded`。
- Node 模拟客户端（由 `cargo test` 执行）：两种翻译接口、延迟初始化、推理开关、模型能力隔离、恢复和用户后改值、接口失败、存储损坏/不可用、防刷新循环、内嵌网页隔离。
- 待确认：Windows 实际中文显示，以及 `gpt-5.6` 在 Max／Ultra 下的真实 U-API 对话。详见 [Windows 验收清单](WINDOWS_ACCEPTANCE.md)。

历史候选安装包不会自动包含本次修复；验收时应核对新构建关联的提交和安装包版本。未修改本机真实 Codex 配置、凭证或安装程序。
