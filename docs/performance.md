# 性能基准与调优记录

测量日期：2026-11-15。原基线为当前净室重写仓库的 `99a76ee`；语义着色补测基于 `827c9e8` 加本次改动。未取得、未访问旧版 Kanso。

## 结果范围

第一轮完成 VT、10 万行回滚搜索、帧快照、编码转换的 Criterion 基准，以及隔离的 Release 启动/RSS 测量工具，调优了 OSC 扫描和字体回退发现。后续的启动与图集优化记录在“冷启动与空闲内存”一节。

**VT 的 `bare / metadata` 是裸依赖引擎与 Kanso 真实元数据路径的对照，不是界面装饰全关/全开验收。** 当前解析器始终维护时间戳与命令块；时间戳栏在 Swift 渲染层控制显示，命令分隔线使用帧中的命令块。本次补入真实输出的语义分类，并增加 `decorations_*` 全关/全开的产帧对照，见下节；它覆盖 Rust 解析、产帧和装饰元数据消费，不包含 Swift 布局、Metal 提交或屏幕实际呈现。

## 语义着色与四项装饰补测

复用下述三种 VT 负载、网格、回滚和 8 KiB 输入批次，两条路径使用相同输入；每输入 1 MiB 产一帧（末尾不足 1 MiB 也产帧），复用帧缓冲。`off` 关闭语义识别、不消费装饰数据；`all` 开启真实语义识别，并读取可见行的时间戳、行号、分隔线标志和命令块。时间戳和命令块的底层维护在两条路径中始终存在，与产品关闭显示时的行为一致。正则初始化在计时外预热，匹配与可见区域缓存维护计入耗时。

运行 `cargo xtask bench`，使用入口默认的 20 样本、预热 1 s、测量 3 s，顺序执行完整基准，未与编译或其他基准并发。环境与原记录相同。吞吐下降为 `1 − off mean / all mean`，入口在任一负载超过 10% 时失败。

| 负载 | off mean ms（95% CI） | all mean ms（95% CI） | off / all MB/s | 吞吐下降 |
|---|---:|---:|---:|---:|
| 300,000 行数字 | 20.124（20.100–20.150） | 20.399（20.348–20.466） | 114.34 / 112.79 | 1.35% |
| 300,000 行 SGR | 27.272（27.238–27.310） | 27.714（27.635–27.819） | 183.37 / 180.45 | 1.59% |
| 20,000 条长行软换行 | 83.737（83.559–83.906） | 84.212（83.897–84.516） | 246.74 / 245.34 | 0.56% |

三组产帧吞吐对照均满足 ≤10%。原始日志为 `target/semantic-bench.log`，样本在 `target/criterion/decorations_*/{off,all}/new/`（构建产物，不入库）。此结果针对固定输入批量与产帧频率；不能据此宣称完整 UI 装饰呈现或任意刷新频率的端到端吞吐已验收。

## 环境与方法

| 项目 | 配置 |
|---|---|
| 硬件 | Apple M5 Max，Mac17,7，128 GiB RAM，arm64 |
| 系统 | macOS 27.3，26C5035r |
| 工具链 | rustc/cargo 1.94.1，Xcode 27.2（27B52） |
| Rust 构建 | Release，thin LTO，1 codegen unit，保留行号信息 |
| Criterion | 0.7.0；上游和本机 registry manifest 均声明 `Apache-2.0 OR MIT`；关闭默认功能，仅启用 `cargo_bench_support` |
| 统计口径 | 40 样本、预热 2 s、测量 5 s；表中是 mean 和 bootstrap 95% 置信区间；MB/s 使用十进制 |

按顺序运行，不与编译、其他基准或 profiler 并发。每个 VT 样本创建新终端，初始化/析构在计时外；计时包含输入解析与回滚维护。固定 120×40 网格、10,000 行回滚、8 KiB 输入批次。三种负载分别是 300,000 行数字、相同数字加 SGR 绿色/重置、20,000 条约 1 KiB 的长行软换行。每 1,024 行插入一组 OSC 133 A/B/C/D，以覆盖命令块维护；两条路径使用完全相同的字节。

搜索预先填入 100,040 个逻辑行（100,000 回滚 + 40 屏幕行），分别测无结果、单次命中、正则单次命中的完整搜索，计时外断言结果。帧基准写入 200×60 个单元格，区分新分配、复用缓冲和真实 `Terminal::snapshot`。

编码负载是 `日本語端末 performance 0123456789\r\n` 重复 32,768 次；先断言三种编码解码往返一致，再测 8 KiB 分批解码和整串编码。decode 的吞吐分母为编码后的输入字节，encode 为 UTF-8 源字节。UTF-8 encode 返回借用，不进行转换或复制，只报告调用耗时。早期使用不可完整表示为 Shift-JIS 的中文负载，其数字不列入结果。

## 最终基准

吞吐下降定义为 `1 − bare 平均耗时 / metadata 平均耗时`，不能用耗时增长百分比替代。

| VT 负载/路径 | 输入字节 | mean ms | 95% CI ms | MB/s |
|---|---:|---:|---:|---:|
| 数字 / bare | 2,300,898 | 18.269 | 18.260–18.279 | 125.95 |
| 数字 / metadata | 2,300,898 | 19.715 | 19.697–19.734 | 116.71 |
| SGR / bare | 5,000,898 | 25.801 | 25.786–25.815 | 193.82 |
| SGR / metadata | 5,000,898 | 26.324 | 26.309–26.340 | 189.97 |
| 长行软换行 / bare | 20,660,810 | 74.383 | 74.251–74.527 | 277.76 |
| 长行软换行 / metadata | 20,660,810 | 80.800 | 80.513–81.092 | 255.70 |

三组元数据路径的吞吐下降为 **7.34% / 1.99% / 7.94%**，均低于 10%；适用范围仅限上述解析层对照。

| 基准 | mean | 95% CI | 目标/判断 |
|---|---:|---:|---|
| 回滚搜索 miss | 16.209 ms | 16.200–16.219 ms | <50 ms，通过 |
| 回滚搜索 hit | 16.300 ms | 16.289–16.312 ms | <50 ms，通过 |
| 回滚搜索 regex | 17.006 ms | 16.974–17.042 ms | <50 ms，通过 |
| FrameBuilder new | 15.765 µs | 15.712–15.820 µs | 无单独绝对目标 |
| FrameBuilder reuse | 15.370 µs | 15.344–15.395 µs | 无单独绝对目标 |
| Terminal snapshot | 30.185 µs | 30.146–30.221 µs | 不代表按键到上屏延迟 |

| 编码转换 | mean | 95% CI | MB/s |
|---|---:|---:|---:|
| UTF-8 decode | 0.543 ms | 0.542–0.545 ms | 2412.30 |
| UTF-8 encode（借用） | 1.866 ns | 1.856–1.875 ns | 不适用 |
| GBK decode | 0.851 ms | 0.850–0.853 ms | 1347.34 |
| GBK encode | 14.143 ms | 14.117–14.168 ms | 92.68 |
| Shift-JIS decode | 0.685 ms | 0.684–0.686 ms | 1673.87 |
| Shift-JIS encode | 12.330 ms | 12.318–12.346 ms | 106.30 |

## 调优证据

### OSC 扫描

Instruments Time Profiler 在 SGR 基线上采到 8,239 个 CPU 样本，`OscScanner::scan` 占 self 样本约 7.6%。原逻辑逐个定位 ESC，彩色输出的每行 SGR 也反复经过 OSC 状态机。改为先查找 OSC 起始所必需的 `]`，只在它前面的最后一个非忽略控制字节为 ESC 时进入解析；末尾 ESC 和允许的 C0 字节仍跨输入批次保存状态。没有绕过真实 VT 引擎，没有关闭时间戳或命令跟踪。

| SGR metadata | mean ms | MB/s | 相对同轮 bare 下降 |
|---|---:|---:|---:|
| 调优前 `before-final` | 34.574 | 144.64 | 23.24% |
| 调优后 `final` | 26.324 | 189.97 | 1.99% |

耗时减少 **23.86%**，吞吐提升 **31.34%**。独立复测 `after-marker` 为 26.260 ms、190.44 MB/s、下降 1.46%。数字输出和软换行原本达标，没有针对它们另做调优。曾尝试调整时间戳写入顺序，收益不足以与波动区分，已撤回。

新增测试遍历负载的所有两段切分及固定输入块宽度，覆盖 SGR、OSC、跨块 ESC+C0、取消控制字符、连续 ESC；`kanso-term` 的其余测试一起验证滚动、时间戳、命令块与重排语义。

### 字体回退发现

启动 Time Profiler 的 247 个 CPU 样本中，`TerminalFonts.installedNerdFamily` inclusive 约 20.6%，Metal 资源初始化约 0.8%。原构造器默认参数立即枚举字体家族，即使首屏只有主字体能够显示的 ASCII 也会付出这笔开销。现在首次出现主字体不覆盖的字符时才查找 Nerd Font，仍缓存查找结果并复用原回退链。

`tools/measure-font-init.swift` 以 `swiftc -O` 编译，独立进程创建真实 TerminalFonts 并解析 ASCII 提示符；新旧版本交替各跑 10 次。初始化阶段中位数由 **57.902 ms → 2.864 ms（−95.05%）**；范围分别为 57.206–100.364 ms 和 2.779–12.251 ms。此数字只覆盖字体初始化阶段，不是优化后 App 冷启动时间。测试验证构造和 ASCII 不触发发现、缺字形只查一次。

## 冷启动与空闲内存

`cargo xtask bench-startup` 默认先构建本 worktree 的 Release-Direct，再直接启动该构建的可执行文件。每轮创建独立 HOME、CFFIXED_USER_HOME、TMPDIR、core 数据目录与日志，注入内存 UserDefaults 和空密钥存储；使用 `/bin/sh -i`、固定提示符、关闭 shell integration，不加载用户 shell 配置。不会启动或修改 `/Applications` 内的 App。

计时从创建进程之前开始，到提示符所在 Metal drawable 完成呈现、终端是窗口 first responder、探针写出 ready 为止；脚本每 1 ms 轮询，包含进程创建和探针通知开销。这里的“冷”指每轮新进程、新数据目录，不清系统文件、字体服务或 Metal 缓存，也不包含 Finder/LaunchServices 的双击启动开销。

出现 ready 后每 0.5 s 采一次 `ps RSS`，持续 10 s，取后 5 s 的中位数作为该轮空闲内存，然后保存 `vmmap -summary`。最终取 5 轮中位数；没有为了获得数字提前结束到首帧的等待。

| 轮次（优化前） | 首帧可输入 ms | 稳定 RSS MB |
|---|---:|---:|
| 1 | 778.19 | 129.25 |
| 2 | 672.09 | 129.35 |
| 3 | 641.37 | 129.88 |
| 4 | 654.23 | 129.88 |
| 5 | 638.90 | 129.86 |
| 中位数 | **654.23** | **129.86** |

均超过 150 ms / 60 MB。目标机器是 M1，本次机器是 M5 Max，不能据此宣称通过 M1 验收。

基线 `vmmap` 的 physical footprint 为 40.8 MiB（42.78 MB）、峰值 43.5 MiB；malloc 已分配约 17.0 MiB、resident 31.0 MiB。RSS 与 footprint 对共享映射和内存归属的统计不同，不能用较小的 footprint 替换 RSS 宣称达标。现有记录能定位分配与共享映射的量级，不能把全部 RSS 差额直接归因于某个业务分配；本次没有据此盲改回滚容量或图集。

**字体优化后的首帧与稳定 RSS 复测受阻：系统 `CGSessionCopyCurrentDictionary` 明确报告 `CGSSessionScreenIsLocked = 1`，实际 drawable 呈现回调未发生。** 已清理本任务启动的测量进程，脚本现在在锁屏或无图形会话时直接说明原因。保持屏幕解锁且窗口可见后重跑即可；本报告没有用锁屏状态下的内存或字体阶段耗时替代完整复测。

### 后续优化：`wp/startup-mem`（2026-11-15）

本轮从 `827c9e8` 开始，已包含上一轮字体延迟发现。先添加测量探针（`93b98f9`），再优化；没有使用旧版仓库。`PerformanceProbe` 在 main、AppKit 就绪、启动通知、菜单、core、设置、辅助面板、窗口布局、PTY、终端视图、补全初始化、窗口显示、实际首帧呈现处记录单调时钟。仅设置 `KANSO_PERF_ROOT` 时记录，窗口显示后和 ready 时保存 `phases.json`；`renderer.json` 另外保存管线耗时和图集字节数。正式启动脚本保留原先的 ready 判定与 10 s RSS 采样，并在每轮末尾从 `vmmap -summary` 提取 `phys_footprint`（活动监视器的内存归属口径），以字节保存并输出十进制 MB。

测量期间多次检查仍为锁屏，故先在相同的隔离环境中分别串行启动 5 个新进程：窗口创建后等待 10 s，保存阶段记录、RSS 与 vmmap，再结束本轮进程及 shell。这组数据**没有发生 drawable 呈现，既不是可输入时间，也不是完整渲染后的空闲内存**。以下阶段耗时和锁屏内存均取各组 5 轮中位数：

| 优化 | 优化前 | 优化后 | 范围与证据 |
|---|---:|---:|---|
| 补全初始化 | 211.802 ms | 0.020 ms | `terminal-view → completion`；同步 `Host.current().names` 占据大头 |
| 辅助面板构建 | 38.727 ms | 16.299 ms | `settings → panels`；Sender、Filer、Git 默认收起时只创建容器 |
| 辅助面板对应进程 RSS | 111.17 MB | 101.55 MB | 锁屏、窗口创建 10 s 后采样 |
| 辅助面板对应 phys_footprint | 36.49 MB | 30.51 MB | 同组 vmmap，约 34.8 → 29.1 MiB |
| 91 字形离屏帧的图集纹理 | 8 MiB | 0.3125 MiB | 同一 Release 帧，灰度与彩色合计 8,388,608 → 327,680 字节 |

补全读取器沿用原来的主机名解析和本地目录归属判断，仅在首次遇到非空的目录主机名时解析并缓存；禁用补全、未上报目录主机、主机名为空时均不解析。测试覆盖延迟执行、缓存及本地/远端归属判断。隐藏面板的模型与事件处理继续存在，视图首次展开时创建并复用；恢复为可见的 Sender 仍在窗口显示前构建，测试覆盖首次展开、重新展开的视图身份、焦点及可见状态恢复。

图集原来首次灰度/彩色字形各申请一张 4 MiB 纹理；现在从 256² 开始，新页按需翻倍至原有边长上限，遇到大字形直接创建足够大的页。已有页不迁移，UV 按所在纹理的实际边长计算。浅色和深色 900×600 pt、2× 离屏帧的 PNG 优化前后逐字节一致；测试另外覆盖跨页增长、旧字形坐标保持及首个大字形。该 7.6875 MiB 是实际纹理分配差额，不能直接当作窗口 RSS 或 footprint 的差额。

没有继续优化的部分也有依据：同一离屏帧的 Metal 编译与管线创建约 1.2–1.5 ms；锁屏剖析中 core（含新数据库迁移和运行时创建）约 10.5 ms，PTY 打开约 4 ms。Alacritty 0.25.1 的网格初始只分配可见行，回滚按需增长，没有发现预分配全部回滚行的问题。面板延迟后，代表性 vmmap 的 malloc 已分配合计约 13.8 MiB，resident 约 19.8 MiB；仍有框架与文件映射常驻，不能把 RSS 与 footprint 的全部差额归因于业务堆分配。

**最终完整冷启动、首帧后稳定 RSS 与 phys_footprint 暂无有效新结果，不能宣称达到 150 ms / 60 MB。** 面板优化后的锁屏 `main → shown` 中位数为 152.94 ms，尚不包括进程创建与 drawable 呈现；剩余主要阶段是 `NSApplication.shared` 约 39 ms、AppKit 启动通知前约 26 ms、窗口构建/布局约 37 ms。锁屏数据只能解释阶段成本，不能替代解锁后的验收，也不能与上面的历史 654.23 ms / 129.86 MB 作完整前后对比。测量机器仍为 M5 Max，M1 验收另需实机。

验证：`cargo xtask apple-test --only-testing KansoTests/CompletionTests`、`KansoTests/SftpDualTests`、`KansoTests/RendererTests` 均通过；完整 `cargo xtask ci` 通过，包含三个渠道构建和 595 个 Swift 测试（0 失败，真实钥匙串往返测试按约定跳过 1 个）。测试临时目录放在本 worktree 的 `target/performance/test-tmp`，设置 `GIT_CEILING_DIRECTORIES` 为该目录，防止“非 Git 仓库”测试向上发现 worktree；XCTest 的 HOME、CFFIXED_USER_HOME 与 TMPDIR 也指向隔离目录。

本轮原始记录保存在 `target/performance/startup/`：`phases-before/`、`phases-host-lazy/`、`phases-panels-before/`、`phases-panels-lazy/` 内有每轮 `phases.json`、`memory.txt` 和组汇总 `report.json`；`atlas-before/`、`atlas-after/` 有 PNG 与 `renderer.json`。解锁后运行 `cargo xtask bench-startup --runs 5` 即可生成正式的新结果，每轮目录也会保留阶段与图集记录。

## 复现

```sh
cargo xtask bench
cargo xtask bench --sample-size 40 --warm-up-time 2 --measurement-time 5 --save-baseline final
cargo xtask bench vt_sgr --sample-size 40 --warm-up-time 2 --measurement-time 5
cargo xtask bench-startup --runs 5
cargo xtask bench-startup --no-build --runs 5
```

`bench` 的默认值是 20 样本/1 s 预热/3 s 测量；正式对比使用表中 40/2/5 参数。摘要只读取本次更新的 Criterion 结果，过滤运行不会混入之前的其他项目。基准不加入 `cargo xtask ci` 的必跑项。启动测量需 Python 3、Xcode 和已解锁的 macOS 图形会话。

字体阶段可以独立于窗口呈现复测；以下命令只写 target，并使用当前仓库的基线文件：

```sh
mkdir -p target/performance/font-init
git show 99a76ee:apple/Packages/TerminalKit/Sources/TerminalKit/TerminalFonts.swift > target/performance/font-init/TerminalFonts-before.swift
swiftc -O -module-cache-path target/performance/swift-cache target/performance/font-init/TerminalFonts-before.swift tools/measure-font-init.swift -o target/performance/font-init/before
swiftc -O -module-cache-path target/performance/swift-cache apple/Packages/TerminalKit/Sources/TerminalKit/TerminalFonts.swift tools/measure-font-init.swift -o target/performance/font-init/after
python3 - <<'PY'
from pathlib import Path
import statistics, subprocess
root = Path('target/performance/font-init').resolve()
results = {'before': [], 'after': []}
for run in range(10):
    for kind in results:
        isolated = root / f'{run}-{kind}'
        isolated.mkdir(exist_ok=True)
        env = {'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'HOME': str(isolated),
               'CFFIXED_USER_HOME': str(isolated), 'TMPDIR': str(isolated)}
        results[kind].append(float(subprocess.check_output([str(root / kind)], cwd=isolated, env=env, text=True)))
for kind, values in results.items():
    print(kind, statistics.median(values), min(values), max(values))
PY
```

剖析：先用 `cargo bench -p kanso-bench --bench terminal --no-run` 取得本机基准可执行文件路径，再将该路径传给 `xcrun xctrace record --template 'Time Profiler' --time-limit 12s --no-prompt --output target/performance/sgr.trace --launch -- <基准可执行文件> --bench --profile-time 8 vt_sgr/metadata`。不要把 profiler 下的计时与无 profiler 的 Criterion 结果混用。

原始记录位于 `target/performance/`（构建产物，不入库）：`before-final.log`、`final.log`、`sgr-before.trace`、`startup-before.trace`、`startup-before.json`、`memory-before.txt`、`font-init/results.json`。Criterion 的原始样本与估计位于 `target/criterion/<组>/<项目>/{before-final,final}/`；最新启动结果在 `target/performance/startup/latest.json`，每轮目录保存日志和内存摘要。

## 目标对照与遗留

| 规格目标 | 当前结论 |
|---|---|
| 四项装饰全开吞吐下降 ≤10% | 真实语义识别与装饰产帧对照下降 1.35%/1.59%/0.56%，通过；完整 UI 呈现吞吐尚未测量 |
| 冷启动 <150 ms（M1） | 基线 M5 为 654.23 ms，超标；字体阶段已优化，完整优化后复测被锁屏阻塞 |
| 空闲内存 <60 MB | 基线 RSS 129.86 MB，超标；优化后稳定 RSS 尚未取得 |
| 10 万行回滚搜索 <50 ms | 16.209–17.006 ms，通过；未修改搜索实现 |
| cat 1 GB ≥Alacritty 应用 80% | 未测；本机没有 Alacritty 应用基线；上述裸依赖对照不可替代 |
| 按键到上屏 <1 帧 | 未测输入到实际呈现的端到端延迟 |
| 20 后台输出会话 UI 60/120 fps | 未做多会话呈现压力测量 |
| SFTP 千兆 ≥80 MB/s | 未测；没有配套千兆远端测试环境 |

验证使用 worktree 内的临时目录；Git 测试的空目录也位于 worktree 中，因此额外设置发现上界，避免将它误识别为外层仓库：

```sh
mkdir -p target/performance/tmp
TMPDIR="$PWD/target/performance/tmp" GIT_CEILING_DIRECTORIES="$PWD/target/performance/tmp" cargo xtask ci
```

验证结果：完整 Criterion 40/2/5 运行通过，入口调整后的 `cargo xtask bench --test` 通过；`cargo test -p kanso-term` 通过，字体回退测试通过；完整 `cargo xtask ci` 通过，包含 Rust 测试、三渠道 Apple 构建、Apple 测试、绑定与本地化校验、cargo deny。未跳过 Apple 或许可证检查。
