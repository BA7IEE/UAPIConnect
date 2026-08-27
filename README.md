# U-API Connect

[![Release](https://img.shields.io/github/v/release/BA7IEE/UAPIConnect)](https://github.com/BA7IEE/UAPIConnect/releases)
[![Build](https://github.com/BA7IEE/UAPIConnect/actions/workflows/uapi-build.yml/badge.svg)](https://github.com/BA7IEE/UAPIConnect/actions/workflows/uapi-build.yml)
[![License](https://img.shields.io/github/license/BA7IEE/UAPIConnect)](LICENSE)

U-API Connect 是给 OpenAI Codex 桌面应用使用的连接工具。它把服务地址固定为 `https://token.u-studio.cn/v1`，负责验证密钥、读取可用模型、生成 Codex 配置，以及在 U-API 与官方登录之间安全切换。

它不包含 Codex，也不会提供服务密钥。使用前需要先安装官方 Codex 桌面应用，并准备自己的 U-API 服务密钥。

## 下载

只从本仓库的 [Releases](https://github.com/BA7IEE/UAPIConnect/releases) 下载：

- Windows 10/11 x64：`UAPIConnect-<版本>-windows-x64-setup.exe`
- Intel Mac：`UAPIConnect-<版本>-macos-x64.dmg`
- Apple Silicon Mac：`UAPIConnect-<版本>-macos-arm64.dmg`
- 校验文件：`SHA256SUMS`

不要把 Actions 页面里的临时 artifact 当作正式安装包转发。正式交付物只在 Release 页面发布，并同时提供 SHA-256 校验值。

Windows 校验示例：

```powershell
Get-FileHash .\UAPIConnect-*-windows-x64-setup.exe -Algorithm SHA256
```

macOS 校验示例：

```bash
shasum -a 256 ./UAPIConnect-*-macos-arm64.dmg
grep 'macos-arm64.dmg$' SHA256SUMS
```

比较两行开头的哈希值；Intel Mac 把文件名中的 `arm64` 换成 `x64`。这样只下载一个 DMG 时，不会因为校验清单里的其他平台文件不存在而误报失败。

## 签名与安全提示

Windows Release 工作流支持 Authenticode 签名，但是否带有签名取决于发布者是否配置了有效的代码签名证书。下载后可用下面的命令检查，只有 `Status` 为 `Valid` 才表示签名有效：

```powershell
Get-AuthenticodeSignature .\UAPIConnect-*-windows-x64-setup.exe |
  Format-List Status,StatusMessage,SignerCertificate
```

没有签名时，Windows SmartScreen 可能显示“Windows 已保护你的电脑”。即使签名有效，普通证书的 SmartScreen 声誉也可能需要一段时间积累。无论是否签名，都应同时核对下载来源和 SHA-256；来源或校验值不一致时不要安装。

macOS 应用目前只有临时签名，没有 Apple Developer ID 签名和公证，Gatekeeper 可能拦截。请先核对 SHA-256，再尝试在 Finder 中右键应用并选择“打开”。仍被隔离时，可对已核验的两个应用执行：

```bash
xattr -dr com.apple.quarantine "/Applications/U-API Connect.app"
xattr -dr com.apple.quarantine "/Applications/U-API Connect 设置.app"
```

## 安装与首次使用

Windows 双击安装程序即可。安装程序按当前用户安装到 `%LOCALAPPDATA%\Programs\U-API Connect`，并创建“U-API Connect”和“U-API Connect 设置”两个入口。界面依赖 Microsoft Edge WebView2 Runtime；安装程序会先检查运行库，缺失时通过微软签名的 Evergreen bootstrapper 静默安装。这个过程需要联网，失败时安装会中止，不会继续写入应用文件。WebView2 已损坏时，应先从微软官方渠道修复后重试。

macOS 打开对应架构的 DMG，把“U-API Connect”和“U-API Connect 设置”拖到“应用程序”。DMG 要求 macOS 12 或更高版本。

首次使用按下面操作：

1. 打开“U-API Connect 设置”。
2. 输入自己的服务密钥并验证，确认已经读到可用模型。
3. 保存配置，状态正常后点击启动，或从“U-API Connect”入口打开 Codex。
4. 要恢复官方账号连接，在设置页切换到“官方模式”，再重启 Codex。

服务密钥保存在系统凭证库中。U-API Connect 的设置和日志位于用户目录下的 `.uapi-connect`，实时生效的 Codex 配置仍位于 `.codex`。复制诊断信息时密钥会被脱敏，但提交 issue 前仍应自行复查，不要上传 `auth.json`、`config.toml` 或任何真实密钥。

## 升级

当前版本不提供应用内自动更新。到本仓库 Release 页面下载更高版本，退出 U-API Connect 和设置工具后直接安装即可。Windows 安装程序会先结束旧进程再覆盖程序文件。

## 卸载

Windows 可从“设置 → 应用 → 已安装的应用”或开始菜单卸载。卸载程序会先停止 U-API Connect，再调用内置清理流程：移除 U-API 自有凭据、受管配置和模型目录，并恢复或移除 U-API 写入的 Codex 实时配置。它不会删除与 U-API 无关的 Codex 配置、认证和 profile。

如果安全清理无法完整完成，卸载程序会中止并保留程序文件，不会静默删掉可用于重试的管理工具。Windows“设置”页能否同步显示失败状态，取决于系统启动卸载器的方式；应以程序文件和 U-API 状态是否仍保留为准。此时重新打开“U-API Connect 设置”检查或修复后再卸载。

macOS 卸载前先退出两个应用，然后在终端运行清理命令：

```bash
"/Applications/U-API Connect 设置.app/Contents/MacOS/CodexPlusPlusManager" --uninstall-cleanup
```

只有命令返回成功后，再把“U-API Connect”和“U-API Connect 设置”移到废纸篓。清理命令失败时先保留应用，以便重试。

## 排查问题

- 找不到官方 Codex：先确认官方桌面应用可以独立启动，再回到“检查与修复”页刷新状态。
- 密钥验证失败：确认密钥仍有效、网络可以访问固定服务地址，并查看页面给出的具体错误。
- 模型为空：在“检查与修复”页重新读取模型；仍失败时复制已脱敏的诊断信息。
- Codex 仍走旧连接：完全退出 Codex 后，从 U-API Connect 入口重新启动。

提交问题时请附操作系统版本、U-API Connect 版本、复现步骤和脱敏后的诊断信息。不要公开真实密钥或 Codex 登录文件。

## 开发与验证

本仓库要求 Node.js 22、Rust stable，以及对应平台的打包工具。常用检查：

```bash
node --test scripts/uapi/tests/distribution.test.mjs

export UAPI_CONNECT_DISTRIBUTION=1

cd apps/codex-plus-manager
npm ci
npm run check
npm run vite:build

cd ../..
cargo fmt --all -- --check
cargo test --workspace --locked
```

Windows 安装、升级和注册表卸载链由 `scripts/uapi/tests/windows-installer-lifecycle.ps1` 在 Windows CI 中实测。macOS Release 分别在 Intel 与 Apple Silicon runner 上生成 U-API 专用 DMG。只有所有平台验证完成后，工作流才会把安装包、`SHA256SUMS` 和 `latest.json` 发布到 Release。

正式发布时只需在已经合并到 `main` 的提交上推送 `v<基础版本>-uapi.<序号>` 格式的 tag，例如 `v1.2.55-uapi.1`。不要提前手工创建 Release；tag 工作流会先完成三平台验证，全部通过后再创建 Release，避免公开空包或未验证资产。

发布者若要启用 Windows Authenticode，需要同时配置仓库 Secrets `WINDOWS_CERTIFICATE` 和 `WINDOWS_CERTIFICATE_PASSWORD`。前者是带私钥、具备 Code Signing EKU 的 PFX 文件经 Base64 编码后的内容，后者是 PFX 密码。两项都不配置时会正常生成未签名 Release；只配置其中一项时工作流会中止，证书文件和密码都不得提交到仓库。

## 上游与许可

本项目基于 [BigPizzaV3/CodexPlusPlus](https://github.com/BigPizzaV3/CodexPlusPlus) 修改，当前代码基线版本为 1.2.55。通用启动、配置管理和桌面应用能力来自上游；U-API Connect 增加了固定服务发行策略、独立界面、凭证隔离与专用安装交付链。上游项目与贡献者保留其原有署名。

项目采用 [GNU Affero General Public License v3.0](LICENSE)，SPDX 标识为 `AGPL-3.0-only`。本项目不是 OpenAI 官方产品，也未获得 OpenAI、ChatGPT 或 Codex 商标授权。
