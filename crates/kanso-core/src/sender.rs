//! Sender 的逐行、批量与重复发送调度；时间与终端状态由调用方提供。

use std::collections::HashSet;

use kanso_types::profile::LineEnding;
use kanso_types::{Message, SessionId};

#[derive(Clone, Debug, Default)]
/// 面板运行状态和本次更新产生的提示。
pub struct SenderStatus {
    /// 手动批次仍在运行。
    pub manual_running: bool,
    /// 重复任务仍在运行。
    pub repeat_running: bool,
    /// 已完成的计划轮次。
    pub rounds: u32,
    /// 计划次数，零表示无限。
    pub count: u32,
    /// 距离下一轮的毫秒数。
    pub next_in_ms: u64,
    /// 本轮已写入行数。
    pub sent: u32,
    /// 本轮计划写入行数。
    pub total: u32,
    /// 供状态栏显示的结果。
    pub notices: Vec<Message>,
    /// 本次更新包含发送失败或跳过的对象。
    pub has_failures: bool,
}

/// 启动时冻结的发送选项。
pub struct SenderRequest {
    /// 原始命令文本。
    pub text: String,
    /// 每行追加的行尾。
    pub ending: LineEnding,
    /// 固定目标，空值表示全部已连接终端。
    pub target: Option<SessionId>,
    /// 是否为重复任务。
    pub repeating: bool,
    /// 两轮之间的秒数。
    pub interval_seconds: u32,
    /// 计划次数，零表示无限。
    pub count: u32,
}

struct Recipient {
    id: SessionId,
    next: usize,
    deadline: u64,
    tracked: bool,
    done: bool,
    failed: bool,
}

struct Job {
    lines: Vec<String>,
    ending: &'static str,
    target: Option<SessionId>,
    repeating: bool,
    interval: u64,
    count: u32,
    rounds: u32,
    next_round: u64,
    recipients: Option<Vec<Recipient>>,
    sent: u32,
    total: u32,
    first_reason: Option<Message>,
}

#[derive(Default)]
pub(crate) struct Scheduler {
    manual: Option<Job>,
    repeating: Option<Job>,
    status: SenderStatus,
    /// 正在被 MCP 的 `mcp_terminal_run` 占用的标签（spec/mcp.md §4.2）。放在调度器里而不是 Core 的
    /// 另一把锁里，是为了让"Sender 是否正在发送"的检查和占用登记在同一次加锁内完成，
    /// `tick` 也在同一把锁下看到占用，Sender 与 MCP 的命令就不会互相插进对方的输入里。
    mcp_busy: HashSet<SessionId>,
}

impl Scheduler {
    pub(crate) fn start(
        &mut self,
        request: SenderRequest,
        targets: Vec<SessionId>,
        now: u64,
    ) -> Result<(), Message> {
        let lines: Vec<_> = request
            .text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_owned)
            .collect();
        if lines.is_empty() {
            return Err(Message::SenderEmptyCommand {});
        }
        if !request.repeating && self.manual.is_some() {
            return Err(Message::SenderBusy {});
        }
        if request.target.is_none() && targets.is_empty() && !request.repeating {
            return Err(Message::SenderNoSessions {});
        }
        let mut job = Job {
            lines,
            ending: match request.ending {
                LineEnding::None => "",
                LineEnding::Lf => "\n",
                LineEnding::CrLf => "\r\n",
            },
            target: request.target,
            repeating: request.repeating,
            interval: u64::from(request.interval_seconds.clamp(1, 86400)) * 1000,
            count: request.count.min(100000),
            rounds: 0,
            next_round: now,
            recipients: None,
            sent: 0,
            total: 0,
            first_reason: None,
        };
        if !request.repeating {
            job.begin_round(targets);
            if let Some(recipients) = self
                .repeating
                .as_ref()
                .and_then(|job| job.recipients.as_ref())
            {
                let busy: HashSet<_> = recipients
                    .iter()
                    .filter(|item| !item.done)
                    .map(|item| item.id)
                    .collect();
                for item in job.recipients.as_mut().expect("手动批次已开始") {
                    if busy.contains(&item.id) {
                        item.done = true;
                        item.failed = true;
                        job.first_reason.get_or_insert(Message::SenderBusy {});
                    }
                }
            }
            self.manual = Some(job);
        } else {
            if self.repeating.take().is_some() {
                self.status.notices.push(Message::SenderRepeatReplaced {});
            }
            self.repeating = Some(job);
        }
        Ok(())
    }

    pub(crate) fn stop(&mut self, repeating: bool) {
        let slot = if repeating {
            &mut self.repeating
        } else {
            &mut self.manual
        };
        if let Some(job) = slot.take() {
            self.status.notices.push(Message::SenderStopped {
                sent: i64::from(job.sent),
                total: i64::from(job.total),
            });
        }
    }

    pub(crate) fn tick(
        &mut self,
        now: u64,
        open: &[SessionId],
        connected: &[SessionId],
        mut ready: impl FnMut(SessionId) -> Result<bool, Message>,
        mut write: impl FnMut(SessionId, &str, &str) -> Result<bool, Message>,
    ) -> SenderStatus {
        let mut occupied = HashSet::new();
        for job in [&self.manual, &self.repeating].into_iter().flatten() {
            if let Some(recipients) = &job.recipients {
                occupied.extend(
                    recipients
                        .iter()
                        .filter(|item| !item.done)
                        .map(|item| item.id),
                );
            }
        }
        for slot in [&mut self.manual, &mut self.repeating] {
            let Some(job) = slot else { continue };
            if job.repeating && job.target.is_some_and(|id| !open.contains(&id)) {
                self.status.notices.push(Message::SenderTargetClosed {});
                *slot = None;
                continue;
            }
            if job.recipients.is_none() {
                if now < job.next_round {
                    continue;
                }
                job.begin_round(connected.to_vec());
                for item in job.recipients.as_mut().expect("本轮已开始") {
                    if occupied.contains(&item.id) {
                        item.failed = true;
                        item.done = true;
                        job.first_reason.get_or_insert(Message::SenderBusy {});
                    } else {
                        occupied.insert(item.id);
                    }
                }
            }
            let recipients = job.recipients.as_mut().expect("本轮已开始");
            for recipient in recipients.iter_mut().filter(|item| !item.done) {
                // MCP 的命令正在这个标签上运行：还没开始发的对象按"忙"跳过，与两个 Sender 任务
                // 撞上同一个标签时的处理一致。已经发出第一行的对象不会出现在这里：
                // `mcp_claim` 在 Sender 还要发内容时会被拒绝。
                if recipient.next == 0 && self.mcp_busy.contains(&recipient.id) {
                    recipient.done = true;
                    recipient.failed = true;
                    job.first_reason.get_or_insert(Message::SenderBusy {});
                    continue;
                }
                let idle = match ready(recipient.id) {
                    Ok(idle) => idle,
                    Err(reason) => {
                        recipient.done = true;
                        recipient.failed = true;
                        job.first_reason.get_or_insert(reason);
                        continue;
                    }
                };
                if recipient.next > 0 && now < recipient.deadline && (!recipient.tracked || !idle) {
                    continue;
                }
                if recipient.next == job.lines.len() {
                    recipient.done = true;
                    continue;
                }
                match write(recipient.id, &job.lines[recipient.next], job.ending) {
                    Ok(tracked) => {
                        recipient.next += 1;
                        job.sent += 1;
                        recipient.tracked = tracked;
                        recipient.deadline = now + if tracked { 30000 } else { 100 };
                    }
                    Err(reason) => {
                        recipient.done = true;
                        recipient.failed = true;
                        job.first_reason.get_or_insert(reason);
                    }
                }
            }
            self.status.sent = job.sent;
            self.status.total = job.total;
            if recipients.iter().all(|item| item.done) {
                let success = recipients.iter().filter(|item| !item.failed).count();
                job.rounds += 1;
                self.status.rounds = job.rounds;
                let skipped = recipients.len() - success;
                self.status.notices.push(Message::SenderSummary {
                    success: success as i64,
                    total: recipients.len() as i64,
                    skipped: skipped as i64,
                });
                self.status.has_failures |= skipped > 0 || recipients.is_empty();
                if let Some(reason) = job.first_reason.take() {
                    self.status.notices.push(reason);
                } else if recipients.is_empty() {
                    self.status.notices.push(Message::SenderNoSessions {});
                }
                for item in recipients.iter() {
                    occupied.remove(&item.id);
                }
                if !job.repeating || (job.count > 0 && job.rounds >= job.count) {
                    if job.repeating {
                        self.status.notices.push(Message::SenderRepeatCompleted {
                            rounds: i64::from(job.rounds),
                        });
                    }
                    *slot = None;
                } else {
                    job.next_round = now + job.interval;
                    job.recipients = None;
                }
            }
        }
        self.status.manual_running = self.manual.is_some();
        self.status.repeat_running = self.repeating.is_some();
        if let Some(job) = &self.repeating {
            self.status.rounds = job.rounds;
            self.status.count = job.count;
            self.status.next_in_ms = if job.recipients.is_none() {
                job.next_round.saturating_sub(now)
            } else {
                0
            };
        }
        std::mem::take(&mut self.status)
    }
}

impl Scheduler {
    /// MCP 申请占用 `session` 来执行一条命令。Sender 这一轮还要给它发内容，或者它已被另一条
    /// MCP 命令占用时返回 `false`，不登记。成功后必须在命令结束（含被取消）时调用 [`mcp_release`](Self::mcp_release)。
    pub(crate) fn mcp_claim(&mut self, session: SessionId) -> bool {
        !self.is_sending_to(session) && self.mcp_busy.insert(session)
    }

    /// 释放 [`mcp_claim`](Self::mcp_claim) 的占用；没有占用时忽略。
    pub(crate) fn mcp_release(&mut self, session: SessionId) {
        self.mcp_busy.remove(&session);
    }

    /// 这一轮里是否还有内容要发给 `session`（spec/mcp.md §4.2 的"Sender 正在发送"）。
    ///
    /// 重复任务在两轮之间没有接收方，此时不算正在发送：MCP 的命令可以插在两轮之间执行。
    pub(crate) fn is_sending_to(&self, session: SessionId) -> bool {
        [&self.manual, &self.repeating]
            .into_iter()
            .flatten()
            .any(|job| {
                job.recipients
                    .as_ref()
                    .is_some_and(|rs| rs.iter().any(|r| r.id == session && !r.done && !r.failed))
            })
    }
}

impl Job {
    fn begin_round(&mut self, connected: Vec<SessionId>) {
        let targets = self.target.map_or(connected, |id| vec![id]);
        self.total = (targets.len() * self.lines.len()) as u32;
        self.sent = 0;
        self.first_reason = None;
        self.recipients = Some(
            targets
                .into_iter()
                .map(|id| Recipient {
                    id,
                    next: 0,
                    deadline: 0,
                    tracked: false,
                    done: false,
                    failed: false,
                })
                .collect(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn id(raw: u64) -> SessionId {
        SessionId::from_raw(raw)
    }
    fn request(target: Option<SessionId>, repeating: bool) -> SenderRequest {
        SenderRequest {
            text: "  first\n\t \nsecond\n".into(),
            ending: LineEnding::Lf,
            target,
            repeating,
            interval_seconds: 1,
            count: 2,
        }
    }

    #[test]
    fn independent_prompt_wait_and_thirty_second_deadline() {
        let mut scheduler = Scheduler::default();
        scheduler
            .start(request(None, false), vec![id(1), id(2)], 0)
            .unwrap();
        let writes = RefCell::new(Vec::new());
        let tick = |scheduler: &mut Scheduler, now| {
            scheduler.tick(
                now,
                &[id(1), id(2)],
                &[id(1), id(2)],
                |session| Ok(session == id(2)),
                |session, text, ending| {
                    writes
                        .borrow_mut()
                        .push((session, text.to_owned(), ending.to_owned()));
                    Ok(true)
                },
            )
        };
        assert!(tick(&mut scheduler, 0).manual_running);
        tick(&mut scheduler, 1);
        assert_eq!(
            writes
                .borrow()
                .iter()
                .map(|item| item.1.as_str())
                .collect::<Vec<_>>(),
            ["  first", "  first", "second"]
        );
        tick(&mut scheduler, 29999);
        assert_eq!(writes.borrow().len(), 3);
        tick(&mut scheduler, 30000);
        assert_eq!(writes.borrow().len(), 4);
        assert_eq!(writes.borrow()[3].2, "\n");
        assert!(!tick(&mut scheduler, 60000).manual_running);
    }

    #[test]
    fn untracked_spacing_and_all_line_endings() {
        for (ending, expected) in [
            (LineEnding::None, ""),
            (LineEnding::Lf, "\n"),
            (LineEnding::CrLf, "\r\n"),
        ] {
            let mut scheduler = Scheduler::default();
            let mut options = request(Some(id(1)), false);
            options.ending = ending;
            scheduler.start(options, vec![id(1)], 0).unwrap();
            let writes = RefCell::new(Vec::new());
            for now in [0, 99, 100, 200] {
                scheduler.tick(
                    now,
                    &[id(1)],
                    &[id(1)],
                    |_| Ok(false),
                    |_, text, ending| {
                        writes.borrow_mut().push(format!("{text}{ending}"));
                        Ok(false)
                    },
                );
                assert_eq!(writes.borrow().len(), if now < 100 { 1 } else { 2 });
            }
            assert_eq!(
                *writes.borrow(),
                [format!("  first{expected}"), format!("second{expected}")]
            );
        }
    }

    #[test]
    fn manual_targets_are_fixed_and_stop_reports_progress() {
        let mut scheduler = Scheduler::default();
        scheduler
            .start(request(None, false), vec![id(1)], 0)
            .unwrap();
        let status = scheduler.tick(
            0,
            &[id(1), id(2)],
            &[id(1), id(2)],
            |_| Ok(true),
            |session, _, _| {
                assert_eq!(session, id(1));
                Ok(false)
            },
        );
        assert_eq!((status.sent, status.total), (1, 2));
        scheduler.stop(false);
        let status = scheduler.tick(10, &[], &[], |_| panic!(), |_, _, _| panic!());
        assert_eq!(
            status.notices,
            [Message::SenderStopped { sent: 1, total: 2 }]
        );
        assert!(!status.manual_running);
    }

    #[test]
    fn repeat_recalculates_targets_counts_empty_rounds_and_waits_after_completion() {
        let mut scheduler = Scheduler::default();
        scheduler.start(request(None, true), vec![], 0).unwrap();
        let first = scheduler.tick(0, &[], &[], |_| panic!(), |_, _, _| panic!());
        assert_eq!((first.rounds, first.next_in_ms), (1, 1000));
        scheduler.tick(999, &[id(2)], &[id(2)], |_| panic!(), |_, _, _| panic!());
        let second = scheduler.tick(
            1000,
            &[id(2)],
            &[id(2)],
            |_| Ok(true),
            |session, _, _| {
                assert_eq!(session, id(2));
                Ok(false)
            },
        );
        assert_eq!(second.sent, 1);
        scheduler.tick(1100, &[id(2)], &[id(2)], |_| Ok(true), |_, _, _| Ok(false));
        let final_status =
            scheduler.tick(1200, &[id(2)], &[id(2)], |_| Ok(true), |_, _, _| panic!());
        assert!(!final_status.repeat_running);
        assert!(
            final_status
                .notices
                .contains(&Message::SenderRepeatCompleted { rounds: 2 })
        );
    }

    #[test]
    fn busy_batches_never_interleave_and_replacement_is_reported() {
        for repeat_first in [true, false] {
            let mut scheduler = Scheduler::default();
            scheduler
                .start(request(None, repeat_first), vec![id(1)], 0)
                .unwrap();
            scheduler.tick(0, &[id(1)], &[id(1)], |_| Ok(true), |_, _, _| Ok(true));
            scheduler
                .start(request(None, !repeat_first), vec![id(1)], 1)
                .unwrap();
            let status = scheduler.tick(
                1,
                &[id(1)],
                &[id(1)],
                |_| Ok(false),
                |_, _, _| panic!("占用期间不得写入"),
            );
            assert!(status.notices.contains(&Message::SenderBusy {}));
            assert!(status.has_failures);
            scheduler
                .start(request(None, true), vec![id(1)], 2)
                .unwrap();
            let status = scheduler.tick(2, &[id(1)], &[id(1)], |_| Ok(false), |_, _, _| Ok(true));
            assert!(status.notices.contains(&Message::SenderRepeatReplaced {}));
        }
    }

    #[test]
    fn mcp_claim_and_sender_exclude_each_other() {
        // Sender 还有内容要发给 1 号标签：MCP 不能占用它，但可以占用别的标签。
        let mut scheduler = Scheduler::default();
        scheduler
            .start(request(Some(id(1)), false), vec![id(1)], 0)
            .unwrap();
        assert!(scheduler.is_sending_to(id(1)));
        assert!(!scheduler.mcp_claim(id(1)));
        assert!(scheduler.mcp_claim(id(2)));
        // 同一个标签不能被两条 MCP 命令同时占用；释放后可以再占用。
        assert!(!scheduler.mcp_claim(id(2)));
        scheduler.mcp_release(id(2));
        assert!(scheduler.mcp_claim(id(2)));

        // MCP 占着 2 号标签时启动的 Sender 任务：一行也不发给它，记为忙。
        let mut scheduler = Scheduler::default();
        assert!(scheduler.mcp_claim(id(2)));
        scheduler
            .start(request(None, false), vec![id(1), id(2)], 0)
            .unwrap();
        let mut written = Vec::new();
        let mut now = 0;
        let mut notices = Vec::new();
        while now <= 1000 {
            let status = scheduler.tick(
                now,
                &[id(1), id(2)],
                &[id(1), id(2)],
                |_| Ok(true),
                |session, _, _| {
                    written.push(session);
                    Ok(false)
                },
            );
            notices.extend(status.notices);
            now += 100;
        }
        assert!(
            written.iter().all(|s| *s == id(1)),
            "wrote to MCP tab: {written:?}"
        );
        assert!(!written.is_empty());
        assert!(
            notices.iter().any(|m| matches!(
                m,
                Message::SenderSummary {
                    success: 1,
                    total: 2,
                    ..
                }
            )),
            "{notices:?}"
        );
    }

    #[test]
    fn lock_disconnect_transfer_and_write_failure_cancel_remaining_lines() {
        for reason in [
            Message::SenderTerminalUnavailable {},
            Message::SenderTerminalClosed {},
            Message::SenderTerminalTransferring {},
            Message::SenderWriteFailed {},
        ] {
            let mut scheduler = Scheduler::default();
            scheduler
                .start(request(None, false), vec![id(1), id(2)], 0)
                .unwrap();
            scheduler.tick(
                0,
                &[id(1), id(2)],
                &[id(1), id(2)],
                |_| Ok(true),
                |_, _, _| Ok(false),
            );
            let failed = scheduler.tick(
                100,
                &[id(1), id(2)],
                &[id(2)],
                |session| {
                    if session == id(1) && reason != (Message::SenderWriteFailed {}) {
                        Err(reason.clone())
                    } else {
                        Ok(true)
                    }
                },
                |session, _, _| {
                    if session == id(1) {
                        Err(reason.clone())
                    } else {
                        Ok(false)
                    }
                },
            );
            assert!(failed.manual_running);
            let status = scheduler.tick(
                200,
                &[id(1), id(2)],
                &[id(1), id(2)],
                |_| Ok(true),
                |_, _, _| panic!("恢复状态后也不续发"),
            );
            assert!(status.notices.contains(&Message::SenderSummary {
                success: 1,
                total: 2,
                skipped: 1
            }));
            assert!(status.notices.contains(&reason));
            assert!(status.has_failures);
        }
    }

    #[test]
    fn closing_fixed_repeat_stops_and_blank_content_is_rejected() {
        let mut scheduler = Scheduler::default();
        scheduler
            .start(request(Some(id(1)), true), vec![id(1)], 0)
            .unwrap();
        let status = scheduler.tick(0, &[id(2)], &[id(2)], |_| panic!(), |_, _, _| panic!());
        assert!(!status.repeat_running);
        assert!(status.notices.contains(&Message::SenderTargetClosed {}));
        let mut blank = request(None, false);
        blank.text = "\n \t\r\n".into();
        assert!(scheduler.start(blank, vec![id(1)], 0).is_err());
    }
}
