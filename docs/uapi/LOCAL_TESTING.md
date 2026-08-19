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

## 安装

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
- 只同步支持 Responses API 的兼容模型；
- Codex 原生模型选择器可切换多个模型；
- GPT 模型可完成对话、读写文件、执行命令和测试；
- 已过滤的 Chat-only 国产模型不再出现；
- 关闭后打开 `U-API Connect` 可直接启动 Codex；
- “刷新模型”不会写死某个模型；
- 用户已有 MCP、Skills、Plugins 和其他 TOML 配置仍保留；
- “复制诊断信息”不含完整密钥；
- 不显示广告、其他中转商和 Provider 导入入口。

## 重新配置

打开：

```text
/Applications/U-API Connect 设置.app
```

进入“连接设置”重新输入 Key。新 Key 验证成功后才覆盖旧配置。

## 备份与回滚

配置写入使用上游 CodexPlusPlus 的原子写入和备份路径。发生失败时应恢复旧配置，并在设置程序中显示错误。

## 上游同步

仓库包含两个提交：

```text
722d6f2  baseline: CodexPlusPlus 1.2.49
后续提交  U-API Connect 定制
```

完整同步流程见：

```text
docs/uapi/UPSTREAM_SYNC.md
```
