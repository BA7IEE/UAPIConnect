# U-API Connect 本地构建与测试

## 最快方式

在 macOS 上双击交付包根目录的：

```text
开始构建 U-API Connect.command
```

脚本会完成：

1. 检查 Xcode Command Line Tools；
2. 检查 Node.js 22；
3. 检查或安装 Rust stable；
4. `npm ci`；
5. 前端测试、发行规则测试和定制范围审计；
6. TypeScript 检查与 Vite 构建；
7. Rust 格式检查与全工作区测试；
8. Release 构建；
9. 生成并打开当前芯片架构的 DMG。

构建日志位于：

```text
local-build-logs/<时间>/build.log
```

## Windows x64 构建

正式 Windows 构建使用 `.github/workflows/uapi-build.yml` 的 `Windows x64`
任务，在原生 Windows runner 上完成前端检查、Rust 测试、Release 编译、NSIS
打包，以及安装、运行中升级和卸载生命周期冒烟。

手动在 Windows 构建两个 U-API 可执行文件时，必须先设置：

```powershell
$env:UAPI_CONNECT_DISTRIBUTION = "1"
cargo build --release
```

这个标记会选择 U-API 专用应用清单。U-API 与当前上游 Codex++ 都使用
`asInvoker`，让程序、用户配置、Credential Manager 和 Store 版 Codex 保持在
同一个交互用户下。Windows 安装包还需要 NSIS 3；官方工作流会自动设置标记并
生成 `UAPIConnect-<版本>-windows-x64-setup.exe`。

## macOS 安装

DMG 中有两个程序：

```text
U-API Connect.app
U-API Connect 设置.app
```

拖入 Applications。测试阶段使用临时签名；若 macOS 阻止启动，执行：

```bash
xattr -dr com.apple.quarantine "/Applications/U-API Connect.app"
xattr -dr com.apple.quarantine "/Applications/U-API Connect 设置.app"
```

## 首次测试

1. 确认官方 Codex Desktop 已安装并完全退出；
2. 打开 `U-API Connect 设置`；
3. 进入“连接设置”；
4. 粘贴临时 NewAPI 密钥；
5. 点击“测试连接”；
6. 确认显示兼容模型数量；
7. 点击“保存配置”；
8. 点击右上角“启动 Codex”。

固定服务地址为：

```text
https://token.u-studio.cn/v1
```

界面不提供其他 Provider、Base URL 或协议配置。

## 核心回归测试

- 概览页能识别 Codex；
- Key 验证成功；
- 同步服务返回的可用文本模型，并正确过滤 embedding、rerank、音视频等非对话模型；
- 模型端点元数据只作为 Responses 能力提示，不因服务缺少元数据而隐藏整个模型系列；
- Codex 原生模型选择器可切换多个模型；
- GPT 模型以及至少一个非 GPT 文本模型可完成对话；对未声明 Responses 的模型需实际验证服务端兼容性；
- 关闭后打开 `U-API Connect` 可直接启动 Codex；
- “刷新模型”不会写死某个模型；
- 用户已有 MCP、Skills、Plugins 和其他 TOML 配置仍保留；
- “复制诊断信息”不含完整密钥；
- 不显示广告、其他中转商和 Provider 导入入口。

## 双模式与凭证回归

- 没有历史 ChatGPT 登录时，切到“官方订阅”后仍可启动 Codex，并进入原生登录流程；
- 已有官方登录时，切到 U-API Connect 后再切回，账号登录状态能够恢复；
- 官方模式连续启动后使用 Codex 最新写回的登录令牌，不会恢复旧快照；
- 官方模式没有实时登录文件、且安全存储暂时不可用时，仍可启动 Codex 进入原生登录，不应被设置程序拦截；
- 旧版本 `settings.json` 中的 U-API 密钥会迁移到安全存储，迁移成功后普通设置文件不再包含完整密钥；
- 旧密钥迁移失败时保留原设置并继续使用兼容路径，不得先清空仅存的旧密钥；
- 安全存储暂时不可用、但当前 `auth.json` 与固定 U-API 地址匹配时，状态判断和启动仍可使用内存中的实时密钥，且不得把它写入普通设置或诊断日志；
- Windows 使用大于 2560 字节的官方登录数据仍可切换，系统凭证库只保存固定长度主密钥；
- 两个进程首次同时保存官方登录数据时，最终主密钥和加密文件保持匹配，任一进程随后都能正常读取；
- 已有新版加密快照时，即使旧凭据槽清理失败也能继续读取；显式删除失败时不得让旧快照在下次读取时复活；
- 固定发行版的长期备份只包含必要配置，不包含明文 `auth.json`；Unix 下备份目录和文件权限分别为 `0700`、`0600`；
- Unix 下实时 `auth.json` 和加密官方快照在原子替换的临时文件阶段即保持 `0600`；
- 任一步骤模拟失败时，`config.toml`、`auth.json`、受管模型目录、设置和凭证都恢复到操作前状态。

## 重新配置

打开：

```text
/Applications/U-API Connect 设置.app
```

进入“连接设置”重新输入 Key。新 Key 验证成功后才覆盖旧配置。

## 备份与回滚

配置写入使用上游 CodexPlusPlus 的原子写入和备份路径。U-API 密钥保存在系统凭证库；体积较大的官方登录快照使用本地加密文件保存，系统凭证库只保管加密主密钥。发生失败时应恢复旧配置、受管模型目录和凭证，并在设置程序中显示错误。

## 上游同步

当前主线建立在上游稳定发行版上：

```text
v1.2.55  CodexPlusPlus 上游发行基线
后续提交  U-API Connect 定制与同步修复
```

完整同步流程见：

```text
docs/uapi/UPSTREAM_SYNC.md
```
