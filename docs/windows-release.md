# Windows 发行包

本版 Windows 发行物：NSIS 安装程序 + 绿色版 zip，x64。**不做代码签名，不做自动更新**（与 macOS 版目前“ad-hoc 签名、未公证”的状态一致）。Authenticode 签名见文末“后续计划”。

## 发行入口

在 Windows 执行 `cargo xtask win-package`。脚本编译 `kanso-gpui` 的 x86_64 MSVC Release，把 `kanso-gpui.exe` 以 `Kanso.exe` 暂存，安装程序与免安装 ZIP 共用下表三项输入。Windows 界面来自 GPUI。macOS AppKit 的 DMG 构建命令见 [macOS 发行流程](apple-build.md)。

## 产物

`cargo xtask win-package` 输出到 `target/win-package/`（设置了 `CARGO_TARGET_DIR` 时在其下），并打印安装程序和免安装 ZIP 的字节数：

| 文件 | 说明 |
|---|---|
| `Kanso-<版本>-x64/` | 暂存目录：`Kanso.exe`、`LICENSE.txt`（GPL-3.0）、`THIRD_PARTY_NOTICES.windows.md`（Windows 构建的依赖声明） |
| `Kanso-<版本>-x64-portable.zip` | 绿色版：解压即用，zip 内有同名顶层目录。用户数据仍在 `%LOCALAPPDATA%\Kanso`，不写进解压目录 |
| `Kanso-<版本>-x64-setup.exe` | 安装程序（仅在找到 makensis 时生成） |
| 以上每个 zip / exe 的 `.sha256` | 内容 `<哈希>  <文件名>`，用 `certutil -hashfile <文件> SHA256` 或 `shasum -c` 校验 |

## 构建环境

只能在 Windows 上构建（macOS / Linux 上运行 `win-package` 会明确报错退出）。

- Rust：`rust-toolchain.toml` 指定的版本，目标 `x86_64-pc-windows-msvc`。
- MSVC 链接环境：Visual Studio Build Tools（“使用 C++ 的桌面开发”，含 Windows 10/11 SDK）。仓库当前在 VM 里用 `rust-lld` 作链接器，同样可行。
- **Windows SDK 的 `rc.exe`**：`crates/kanso-gpui/build.rs` 用它把图标、版本信息、应用清单编成 `.res` 链进 exe。查找顺序：环境变量 `RC` → `PATH` → `Windows Kits\10\bin\<最高版本>\x64\rc.exe`。`win-package` 会设置 `KANSO_REQUIRE_RESOURCES=1`，找不到 `rc.exe` 时构建失败；普通 `cargo build` 只给警告，exe 照常生成但没有图标 / 版本信息。
- **NSIS 3.x**（建议 3.09 及以上，需要含 Unicode 与 `SimpChinese` 语言文件的标准安装包）：只用于生成安装程序，是构建工具，不并入 Kanso 的代码。NSIS 采用 zlib/libpng 许可（宽松，GPL 兼容）；安装程序里嵌入的 NSIS 安装存根按其许可分发，已在 `THIRD_PARTY_NOTICES.windows.md` 记录。查找顺序：环境变量 `MAKENSIS` → `PATH` → `%ProgramFiles(x86)%\NSIS` / `%ProgramFiles%\NSIS` / `%LOCALAPPDATA%\Programs\NSIS`。找不到则只生成 zip 并给出提示。
- `tar.exe`（Windows 10 1803 起自带）使用 Deflate 9 生成资源管理器可直接解压的 ZIP，失败时用 PowerShell `Compress-Archive -CompressionLevel Optimal`。NSIS 安装程序使用 16 MiB 字典的 solid LZMA。`certutil` 计算 SHA-256。
- 离线环境（如测试 VM）：先在联网机器上 `cargo fetch`，把依赖放进离线 registry，再运行 `cargo xtask win-package --offline`。

## 命令

```sh
cargo xtask win-package                 # 构建 + zip + 安装程序（有 makensis 时）
cargo xtask win-package --skip-build    # 直接打包已有的 release exe（target\x86_64-pc-windows-msvc\release\kanso-gpui.exe）
cargo xtask win-package --no-installer  # 只做 zip
cargo xtask win-package --offline       # 构建时给 cargo 加 --offline
cargo xtask notices                     # 重新生成 THIRD_PARTY_NOTICES.md 与 THIRD_PARTY_NOTICES.windows.md
```

`cargo xtask notices` 现在同时生成两份声明：`THIRD_PARTY_NOTICES.md`（macOS，内容不变）和 `THIRD_PARTY_NOTICES.windows.md`（`kanso-gpui` 在 `x86_64-pc-windows-msvc` 下的普通依赖，含 gpui 及其仅 Windows 的依赖树）。改了依赖后两份都要提交。

## exe 本身

- `win-package` 为 x64 Windows 目标及其依赖静态链接 MSVC 运行库，用户可以直接启动。使用 `--skip-build` 时须提供同样以 `-Ctarget-feature=+crt-static` 构建的 Release 程序；可用 `dumpbin /dependents Kanso.exe` 核对导入表。
- Release 使用 GUI 子系统（`windows_subsystem = "windows"`），双击不弹控制台；Debug 保留控制台。
- 日志：`%LOCALAPPDATA%\Kanso\kanso-gpui.log`；启动时把上一次的日志改名为 `kanso-gpui.log.1`。panic 会连同线程名、位置与回溯写进日志；主线程上未被捕获的 panic 还会弹出原生错误框（写明日志路径）并以退出码 101 结束。
- 资源嵌入（`build.rs`，仅目标为 Windows 时运行，对 Mac / Linux 的构建与测试无影响）：
  - 图标：`crates/kanso-gpui/windows/kanso.ico`（16/24/32/48/64/256），由 macOS 应用图标生成，来源见 `docs/assets-provenance.md`；重新生成用 `python3 tools/gen-windows-icons.py`。
  - 版本信息：ProductName Kanso、FileVersion / ProductVersion 取工作区版本、CompanyName 与 LegalCopyright 为 chhuax、GPL-3.0-or-later。
  - 应用清单：`crates/kanso-gpui/windows/kanso.manifest`——PerMonitorV2 DPI、Common Controls v6、`activeCodePage` UTF-8、`longPathAware`、Windows 10/11 兼容声明、`asInvoker`。
  - gpui 0.2.2 默认的 `windows-manifest` 特性会再嵌入它自己的清单（同样是 PerMonitorV2 + Common Controls 6），与上面的清单重复，所以 `crates/kanso-gpui/Cargo.toml` 里关掉了它。DPI 感知仍由清单提供，gpui 自己不调用 `SetProcessDpiAwareness`。排查清单冲突时可设 `KANSO_SKIP_MANIFEST=1` 构建来不嵌入我们的清单。
- 运行时资源：图标 SVG（`include_bytes!`）、终端主题 JSON（`include_str!`）、多语言目录都编进 exe，安装后只需 `Kanso.exe` 一个文件即可运行，不依赖构建目录。

## 安装程序行为（`packaging/windows/kanso.nsi`）

- 按用户安装，不要管理员权限；默认目录 `%LOCALAPPDATA%\Programs\Kanso`，已安装过则沿用上次目录（升级即覆盖安装）。
- 页面：欢迎 → 许可（GPL-3.0）→ 选目录 → 安装 → 完成（“运行 Kanso”默认勾选，“创建桌面快捷方式”默认不勾选）。界面语言：系统界面语言为简体中文时用简体中文，其余用 English。
- 开始菜单快捷方式 `Kanso`。卸载信息写在 `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Kanso`（DisplayName、DisplayVersion、Publisher、DisplayIcon、UninstallString、InstallLocation、EstimatedSize、NoModify、NoRepair），可在“设置 → 应用”里卸载。
- 安装 / 卸载前检测 Kanso 是否在运行（以写入方式打开 `Kanso.exe`，被锁定即视为运行中），提示“重试 / 取消”，不会强杀进程。静默模式下视为取消。
- 卸载：删除程序文件与快捷方式，只对空的安装目录做 `RMDir`。**默认保留用户数据**；交互卸载时会询问是否同时删除 `%LOCALAPPDATA%\Kanso`（会话、设置、日志；默认“否”），静默卸载（`/S`）一律保留。保存在 Windows 凭据管理器里的密码（目标名前缀 `com.chhuax.kanso/`）不会被卸载程序删除，需要的话在“控制面板 → 凭据管理器”里手动清理。
- 静默安装 / 卸载：`Kanso-<版本>-x64-setup.exe /S [/D=<目录>]`、`"%LOCALAPPDATA%\Programs\Kanso\Uninstall.exe" /S`。

## 首次安装与代码签名

Kanso 使用 NSIS 的 `RequestExecutionLevel user` 按当前用户安装，正常安装无需提升到管理员。浏览器下载的安装包可能出现「Windows 已保护你的电脑」。官网分发可用受信任的 Authenticode 签名显示已验证的发布者，并持续使用同一发布者身份。新发布者和新文件仍可能在积累信誉期间触发 SmartScreen。[Microsoft SmartScreen 说明](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation)

从公开信任的 CA 获取代码签名证书，或按主体资格申请 [Azure Artifact Signing](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/code-signing-options)。符合条件的公开开源项目可申请 [SignPath Foundation](https://signpath.org/terms)。私钥留在签名服务或硬件密钥设备中。

### 配置已获得的证书

在装有 Windows SDK 的构建机上，让 SignTool 可以访问签名证书。用实际证书指纹和签发方提供的 RFC 3161 时间戳地址替换示例中的两个占位符。

```powershell
$env:KANSO_SIGN_CMD = 'signtool sign /sha1 <证书指纹> /fd SHA256 /tr <RFC3161_URL> /td SHA256 "%1"'
cargo xtask win-package
```

打包脚本先签暂存的 `Kanso.exe`，因此 ZIP 内也是签名后的应用。NSIS 的 `!finalize` 和 `!uninstfinalize` 分别签安装器与卸载器。生成签名成品后再计算 SHA-256。使用云服务时，把模板换成其官方 SignTool 调用，保留 `%1` 作为待签文件路径。[Authenticode 时间戳](https://learn.microsoft.com/en-us/windows/win32/seccrypto/time-stamping-authenticode-signatures)

发布前用 `signtool verify /pa /v` 分别验签安装器、ZIP 解出的 `Kanso.exe` 和测试安装生成的 `Uninstall.exe`，核对发布者与时间戳。再从浏览器下载到测试机检查 SmartScreen 实际表现。未配置 `KANSO_SIGN_CMD` 时生成未签名构建。[SignTool 文档](https://learn.microsoft.com/en-us/windows/desktop/seccrypto/signtool)

## 在干净环境里验证安装 / 卸载 / 升级

用一台没有开发环境的 Windows 10/11 机器或全新 VM（例如恢复快照）：

1. 把 `*-setup.exe`、`*-portable.zip` 和 `.sha256` 拷进去，先用 `certutil -hashfile` 核对哈希。
2. 安装：双击 setup，应不弹 UAC；中文系统显示中文界面，英文系统显示英文。确认默认目录、许可页、完成页两个复选框的默认状态；勾选“创建桌面快捷方式”后桌面出现快捷方式。
3. 启动：从开始菜单运行；无控制台窗口；`%LOCALAPPDATA%\Kanso\kanso-gpui.log` 生成。资源管理器里查看 `Kanso.exe` 属性：图标、详细信息页的产品名 / 版本 / 版权。
4. 升级：先装旧版（或同版本）并保持 Kanso 运行，再运行新的 setup：应提示关闭 Kanso；关闭后重试，应覆盖安装，设置与会话保留，“设置 → 应用”里只有一条 Kanso。
5. 卸载：从“设置 → 应用”卸载。选“否”保留数据后确认程序文件、开始菜单与桌面快捷方式、注册表项（`HKCU\Software\Kanso`、Uninstall 下的 Kanso）都已清除，`%LOCALAPPDATA%\Kanso` 仍在；重装后再卸载并选“是”，该目录应被删除。
6. 绿色版：解压到任意目录（含带空格、中文的路径）直接运行。
7. 高 DPI：把显示缩放设为 200%（多显示器时试着在不同缩放的显示器间拖动窗口），界面应清晰且尺寸正确，不被系统位图拉伸。

## 自动发布

推送 `v*` 标签后，`.github/workflows/release-windows.yml` 会在 GitHub 的 `windows-latest` 机器上安装 NSIS，执行 `cargo xtask win-package`，再把安装程序、免安装 ZIP 和各自的 `.sha256` 上传到同名 Release。

- 同名 Release 不存在时先建草稿，macOS 的 DMG 可以补进同一个 Release 后再发布。
- 标签必须和 kanso-gpui 的版本号一致，否则直接失败。
- 也可以在 Actions 页面手动运行，填入已有标签。
- 改动打包相关文件的 PR 只试打包，不上传。
