//! 界面文字的英文译文表：`(简体中文源文, 英文)`。`tr()` 在英文下按源文查这张表。
//! 新增界面文字时在这里补一行；`app::i18n` 的测试会检查源码里每个 `tr("…")` 都有对应条目。
//!
//! 已经走核心消息目录（`Message::Ui*`，`locales/*.ftl`）的文字不要再放这里。

pub const EN: &[(&str, &str)] = &[
    // 语言设置
    ("跟随系统", "Follow system"),
    ("语言", "Language"),
    ("界面语言", "Interface language"),
    (
        "切换后立即生效；“跟随系统”按系统界面语言选择，不支持的语言显示英文。",
        "Takes effect immediately. “Follow system” uses the system display language; unsupported languages fall back to English.",
    ),
    // 数据导入导出
    ("导出完成", "Export Complete"),
    (
        "已导出到 {}。\n文件里不包含任何密码或口令。",
        "Exported to {}.\nThe file contains no passwords or passphrases.",
    ),
    ("导入数据", "Import Data"),
    ("将导入这个文件：", "This file will be imported:"),
    (
        "会按 id 合并：文件里 id 相同的会话、分组、命令会替换本地的，其余作为新数据加入，本地数据不会被删除；被替换的会话保留已保存的凭据。有问题的单条数据会被跳过。",
        "Data is merged by id: sessions, groups and commands with the same id replace the local ones, everything else is added, and no local data is deleted. Replaced sessions keep their saved credentials. Invalid entries are skipped.",
    ),
    ("导入完成", "Import Complete"),
    ("操作没有成功", "Operation Failed"),
    ("已跳过", "Skipped"),
    ("警告", "Warnings"),
    ("好", "OK"),
    ("取消", "Cancel"),
    ("导入", "Import"),
    ("选择要导入的 .kanso 文件", "Choose a .kanso file to import"),
    // OpenSSH 配置导入
    ("导入 OpenSSH 配置", "Import OpenSSH Config"),
    ("导入 OpenSSH 配置…", "Import OpenSSH Config…"),
    ("选择 OpenSSH 配置文件", "Choose an OpenSSH config file"),
    ("选择…", "Choose…"),
    ("不分组", "No group"),
    ("导入到分组", "Import into group"),
    ("全选（已选 {} / {}）", "Select all ({} of {} selected)"),
    ("已保存，将更新", "Saved, will update"),
    ("私钥 {}", "Key {}"),
    ("经由 {}", "via {}"),
    ("正在读取…", "Reading…"),
    ("正在导入…", "Importing…"),
    (
        "配置文件里没有可导入的主机。",
        "No hosts to import in this file.",
    ),
    (
        "找到 {} 台主机，其中 {} 台已保存（导入时会更新）。",
        "Found {} hosts, {} of them already saved (they will be updated).",
    ),
    // 空状态页
    ("没有打开的会话", "No open sessions"),
    ("新建本地 Shell", "New Local Shell"),
    ("新建会话…", "New Session…"),
    ("或按", "or press"),
];
