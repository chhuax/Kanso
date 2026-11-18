//! 终端内传输（ZMODEM / XMODEM，docs/spec/encoding-transfer.md §2、§3）的界面逻辑：
//! 核心提问（选文件、选保存位置）对应的系统文件选择器方案与应答，以及传输进度条的内容。
//!
//! 核心在对话框期间独占并暂存原始字节：用户取消选择就应答 `Cancel`，核心把暂存的字节还给终端。
//! 本文件不依赖窗口系统，三个平台都能编译与测试。

use kanso_types::event::TransferDirection;
use kanso_types::prompt::{PromptRequest, PromptResponse};
use kanso_types::transfer::XferProtocol;
use kanso_types::{SessionId, TransferId};

use crate::app::filer_text::msg;
use crate::app::transfer_queue::{
    Bar, BarFinish, DONE_LINGER_MS, FAIL_LINGER_MS, Finish, SpeedMeter,
};
use crate::filer::format_size;
use kanso_types::message::Message;

/// 系统文件选择器的用法。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PickerPlan {
    /// 选择已有的文件（只能选文件，不能选目录）。
    OpenFiles {
        /// 是否多选（ZMODEM 发送可多选，XMODEM 发送只选一个）。
        multiple: bool,
    },
    /// 选择保存位置（另存为）。
    Save {
        /// 默认文件名；没有则为空。
        suggested_name: String,
    },
}

/// 提问对应的选择器方案；不是文件传输类提问返回 `None`。
pub fn picker_plan(request: &PromptRequest) -> Option<PickerPlan> {
    match request {
        PromptRequest::ZmodemPickFiles => Some(PickerPlan::OpenFiles { multiple: true }),
        PromptRequest::XmodemPickFile => Some(PickerPlan::OpenFiles { multiple: false }),
        PromptRequest::ZmodemSaveFile { suggested_name, .. } => Some(PickerPlan::Save {
            suggested_name: suggested_name.clone(),
        }),
        PromptRequest::XmodemSaveFile => Some(PickerPlan::Save {
            suggested_name: msg(Message::UiXferDefaultName {}),
        }),
        _ => None,
    }
}

/// 打开选择器前给用户的一句说明（状态栏提示）：谁要发什么、该选什么。
pub fn picker_hint(request: &PromptRequest) -> Option<String> {
    match request {
        PromptRequest::ZmodemPickFiles => Some(msg(Message::UiXferHintPickFiles {})),
        PromptRequest::XmodemPickFile => Some(msg(Message::UiXferHintPickFile {})),
        PromptRequest::ZmodemSaveFile {
            suggested_name,
            size,
        } => Some(match size {
            Some(s) => msg(Message::UiXferHintSaveSized {
                name: suggested_name.to_string(),
                amount: (format_size(*s)).to_string(),
            }),
            None => msg(Message::UiXferHintSave {
                name: suggested_name.to_string(),
            }),
        }),
        PromptRequest::XmodemSaveFile => Some(msg(Message::UiXferHintXmodemSave {})),
        _ => None,
    }
}

/// 选择器结果 → 应答。取消（`None` 或空）一律是 `Cancel`；类型对不上的提问也取消。
pub fn response_for(request: &PromptRequest, picked: Option<Vec<String>>) -> PromptResponse {
    let picked = picked.filter(|p| !p.is_empty());
    match (picker_plan(request), picked) {
        (Some(PickerPlan::OpenFiles { multiple }), Some(mut paths)) => {
            if !multiple {
                paths.truncate(1);
            }
            PromptResponse::Files(paths)
        }
        (Some(PickerPlan::Save { .. }), Some(paths)) => {
            PromptResponse::SavePath(paths.into_iter().next().unwrap_or_default())
        }
        _ => PromptResponse::Cancel,
    }
}

/// 保存对话框的初始目录：用户的“下载”目录，没有就退回家目录。
pub fn default_save_dir(home: &str) -> String {
    let sep = if home.contains('\\') { '\\' } else { '/' };
    if home.is_empty() {
        return String::new();
    }
    format!("{}{sep}Downloads", home.trim_end_matches(['/', '\\']))
}

/// 协议名。
pub fn protocol_name(p: XferProtocol) -> &'static str {
    match p {
        XferProtocol::Zmodem => "ZMODEM",
        XferProtocol::Xmodem => "XMODEM",
        XferProtocol::Sftp => "SFTP",
    }
}

/// 一个终端内传输。
#[derive(Clone, Debug)]
pub struct TermTransfer {
    /// 传输 id。
    pub id: TransferId,
    /// 所属会话。
    pub session: SessionId,
    /// 协议。
    pub protocol: XferProtocol,
    /// 方向（上传 = 发给对方，下载 = 从对方接收）。
    pub direction: TransferDirection,
    /// 文件名。
    pub name: String,
    done: u64,
    total: Option<u64>,
    speed: SpeedMeter,
    finished: Option<(BarFinish, u64, String)>,
}

/// 全部终端内传输（按会话各自显示，不互相覆盖）。
#[derive(Clone, Debug, Default)]
pub struct TermTransfers {
    items: Vec<TermTransfer>,
}

impl TermTransfers {
    /// 传输开始。SFTP 传输不归这里管（返回 `false`）。
    pub fn on_started(
        &mut self,
        id: TransferId,
        session: SessionId,
        protocol: XferProtocol,
        direction: TransferDirection,
        name: &str,
        total: Option<u64>,
    ) -> bool {
        if protocol == XferProtocol::Sftp {
            return false;
        }
        // 同一会话同一时刻只有一个终端内传输；ZMODEM 多文件会连续发多个开始事件，旧的收尾状态直接替换。
        self.items.retain(|i| i.session != session);
        self.items.push(TermTransfer {
            id,
            session,
            protocol,
            direction,
            name: name.to_owned(),
            done: 0,
            total,
            speed: SpeedMeter::default(),
            finished: None,
        });
        true
    }

    /// 进度。
    pub fn on_progress(&mut self, id: TransferId, done: u64, total: Option<u64>, now_ms: u64) {
        if let Some(i) = self.items.iter_mut().find(|i| i.id == id) {
            i.done = done;
            if total.is_some() {
                i.total = total;
            }
            i.speed.push(now_ms, done);
        }
    }

    /// 结束。返回所属会话（不是本模型管理的传输返回 `None`）。
    pub fn on_finished(
        &mut self,
        id: TransferId,
        finish: &Finish,
        now_ms: u64,
    ) -> Option<SessionId> {
        let i = self.items.iter_mut().find(|i| i.id == id)?;
        let (kind, text) = match finish {
            Finish::Completed { .. } => (BarFinish::Done, msg(Message::UiXferDone {})),
            Finish::Cancelled => (BarFinish::Cancelled, msg(Message::UiXferCancelled {})),
            Finish::Failed { text, .. } => (
                BarFinish::Failed,
                msg(Message::UiXferFailed {
                    why: text.to_string(),
                }),
            ),
        };
        i.finished = Some((kind, now_ms, text));
        Some(i.session)
    }

    /// 会话关闭：丢掉它的传输记录。
    pub fn forget_session(&mut self, session: SessionId) {
        self.items.retain(|i| i.session != session);
    }

    /// 该会话是否有进行中的终端内传输（取消按钮、关闭确认用）。
    pub fn is_active(&self, session: SessionId) -> bool {
        self.items
            .iter()
            .any(|i| i.session == session && i.finished.is_none())
    }

    /// 清理已过停留时间的收尾状态；返回是否有变化。
    pub fn prune(&mut self, now_ms: u64) -> bool {
        let before = self.items.len();
        self.items.retain(|i| match &i.finished {
            None => true,
            Some((k, at, _)) => {
                let linger = if *k == BarFinish::Failed {
                    FAIL_LINGER_MS
                } else {
                    DONE_LINGER_MS
                };
                now_ms.saturating_sub(*at) < linger
            }
        });
        self.items.len() != before
    }

    /// 某会话要显示的进度条。
    pub fn bar(&self, session: SessionId, now_ms: u64) -> Option<Bar> {
        let i = self.items.iter().find(|i| i.session == session)?;
        let verb = match i.direction {
            TransferDirection::Upload => msg(Message::UiXferSend {}),
            TransferDirection::Download => msg(Message::UiXferReceive {}),
        };
        let mut title = format!("{} {verb}", protocol_name(i.protocol));
        if !i.name.is_empty() {
            title.push(' ');
            title.push_str(&i.name);
        }
        if let Some((kind, at, text)) = &i.finished {
            let linger = if *kind == BarFinish::Failed {
                FAIL_LINGER_MS
            } else {
                DONE_LINGER_MS
            };
            if now_ms.saturating_sub(*at) >= linger {
                return None;
            }
            return Some(Bar {
                title: format!("{title}：{text}"),
                fraction: matches!(kind, BarFinish::Done).then_some(1.0),
                detail: String::new(),
                cancellable: false,
                finish: Some(*kind),
            });
        }
        let fraction = match i.total {
            Some(t) if t > 0 => Some((i.done as f64 / t as f64).clamp(0.0, 1.0)),
            _ => None,
        };
        let mut detail = match i.total {
            Some(t) if t > 0 => format!("{} / {}", format_size(i.done), format_size(t)),
            _ => format_size(i.done),
        };
        if let Some(bps) = i.speed.bytes_per_sec() {
            detail.push_str("  ");
            detail.push_str(&crate::app::transfer_queue::speed_text(bps));
        }
        Some(Bar {
            title,
            fraction,
            detail,
            cancellable: true,
            finish: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(n: u64) -> SessionId {
        SessionId::from_raw(n)
    }
    fn tid(n: u64) -> TransferId {
        TransferId::from_raw(n)
    }

    #[test]
    fn plans_match_the_spec() {
        assert_eq!(
            picker_plan(&PromptRequest::ZmodemPickFiles),
            Some(PickerPlan::OpenFiles { multiple: true })
        );
        assert_eq!(
            picker_plan(&PromptRequest::XmodemPickFile),
            Some(PickerPlan::OpenFiles { multiple: false })
        );
        assert_eq!(
            picker_plan(&PromptRequest::ZmodemSaveFile {
                suggested_name: "a.bin".into(),
                size: Some(10)
            }),
            Some(PickerPlan::Save {
                suggested_name: "a.bin".into()
            })
        );
        assert!(matches!(
            picker_plan(&PromptRequest::XmodemSaveFile),
            Some(PickerPlan::Save { .. })
        ));
        assert_eq!(
            picker_plan(&PromptRequest::Credential {
                endpoint: kanso_types::message::Endpoint {
                    host: "h".into(),
                    port: 22,
                    via: vec![]
                },
                user: "u".into(),
                kind: kanso_types::prompt::CredentialKind::Password
            }),
            None
        );
    }

    #[test]
    fn responses_cancel_unless_something_was_picked() {
        let pick = PromptRequest::ZmodemPickFiles;
        assert_eq!(response_for(&pick, None), PromptResponse::Cancel);
        assert_eq!(response_for(&pick, Some(vec![])), PromptResponse::Cancel);
        assert_eq!(
            response_for(&pick, Some(vec!["a".into(), "b".into()])),
            PromptResponse::Files(vec!["a".into(), "b".into()])
        );
        // XMODEM 只发一个文件。
        assert_eq!(
            response_for(
                &PromptRequest::XmodemPickFile,
                Some(vec!["a".into(), "b".into()])
            ),
            PromptResponse::Files(vec!["a".into()])
        );
        assert_eq!(
            response_for(
                &PromptRequest::XmodemSaveFile,
                Some(vec!["C:\\x.bin".into()])
            ),
            PromptResponse::SavePath("C:\\x.bin".into())
        );
        assert_eq!(
            response_for(&PromptRequest::XmodemSaveFile, None),
            PromptResponse::Cancel
        );
    }

    #[test]
    fn hints_name_the_file_and_size() {
        let h = picker_hint(&PromptRequest::ZmodemSaveFile {
            suggested_name: "a.bin".into(),
            size: Some(2048),
        })
        .unwrap();
        assert!(h.contains("a.bin") && h.contains("2.0 KB"));
        let h = picker_hint(&PromptRequest::ZmodemSaveFile {
            suggested_name: "a.bin".into(),
            size: None,
        })
        .unwrap();
        assert!(h.contains("a.bin") && !h.contains("KB"));
    }

    #[test]
    fn save_dir_follows_the_home_style() {
        assert_eq!(default_save_dir(r"C:\Users\me"), r"C:\Users\me\Downloads");
        assert_eq!(default_save_dir("/home/me/"), "/home/me/Downloads");
        assert_eq!(default_save_dir(""), "");
    }

    #[test]
    fn progress_and_finish_per_session() {
        let mut m = TermTransfers::default();
        assert!(!m.on_started(
            tid(1),
            sid(1),
            XferProtocol::Sftp,
            TransferDirection::Upload,
            "x",
            None
        ));
        m.on_started(
            tid(2),
            sid(1),
            XferProtocol::Zmodem,
            TransferDirection::Download,
            "",
            None,
        );
        assert_eq!(m.bar(sid(1), 0).unwrap().title, "ZMODEM 接收");
        assert!(m.on_started(
            tid(2),
            sid(1),
            XferProtocol::Zmodem,
            TransferDirection::Download,
            "a.bin",
            Some(1000)
        ));
        assert!(m.is_active(sid(1)));
        assert!(!m.is_active(sid(2)));
        m.on_progress(tid(2), 500, Some(1000), 0);
        let bar = m.bar(sid(1), 0).unwrap();
        assert_eq!(bar.title, "ZMODEM 接收 a.bin");
        assert_eq!(bar.fraction, Some(0.5));
        assert!(bar.cancellable);
        assert!(m.bar(sid(2), 0).is_none());
        assert_eq!(m.on_finished(tid(2), &Finish::Cancelled, 100), Some(sid(1)));
        let bar = m.bar(sid(1), 200).unwrap();
        assert_eq!(bar.finish, Some(BarFinish::Cancelled));
        assert!(!m.is_active(sid(1)));
        assert!(m.prune(100 + DONE_LINGER_MS));
        assert!(m.bar(sid(1), 100 + DONE_LINGER_MS).is_none());
    }

    #[test]
    fn failure_text_is_kept_in_the_bar_and_unknown_ids_ignored() {
        let mut m = TermTransfers::default();
        m.on_started(
            tid(3),
            sid(1),
            XferProtocol::Xmodem,
            TransferDirection::Upload,
            "f",
            None,
        );
        assert_eq!(m.bar(sid(1), 0).unwrap().fraction, None);
        assert_eq!(m.on_finished(tid(99), &Finish::Cancelled, 0), None);
        m.on_finished(
            tid(3),
            &Finish::Failed {
                text: "超时".into(),
                already_exists: false,
            },
            0,
        );
        let bar = m.bar(sid(1), 1000).unwrap();
        assert!(bar.title.contains("超时") && bar.title.contains("XMODEM 发送"));
        assert!(m.bar(sid(1), FAIL_LINGER_MS).is_none());
    }
}
