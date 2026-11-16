//! `--sftp-bench`：不开窗口，只在核心层测量 SFTP 连接与列目录（`Core::sftp_list` 的流式批次）。
//!
//! 用来把“核心 / 网络”的耗时和“GPUI 渲染”的耗时分开：窗口版的 `PERF` 日志对照本模式的数字，
//! 差值就是 UI 线程的开销。同一目录连续列两次（第二次是热缓存），并把所有批次并入 `DirModel`
//! 测量模型的归并排序成本。

use std::time::{Duration, Instant};

use futures::channel::mpsc::UnboundedReceiver;
use kanso_core::Core;
use kanso_gpui::filer::{DirModel, SftpSpec, Sort};
use kanso_types::CoreEvent;
use kanso_types::event::SessionState;
use kanso_types::prompt::PromptResponse;

use super::{Args, log, memory, open_sftp_session};

fn wait_connected(
    core: &Core,
    rx: &mut UnboundedReceiver<CoreEvent>,
    session: kanso_types::SessionId,
    limit: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + limit;
    loop {
        match rx.try_recv() {
            Ok(CoreEvent::SessionStateChanged { session: s, state }) if s == session => match state
            {
                SessionState::Connected => return Ok(()),
                SessionState::Closed(r) => return Err(format!("连接关闭：{r:?}")),
                SessionState::Connecting => {}
            },
            Ok(CoreEvent::Prompt {
                prompt, request, ..
            }) => {
                log(&format!("认证提问，自动取消：{request:?}"));
                core.respond_prompt(prompt, PromptResponse::Cancel);
            }
            Ok(_) => {}
            Err(futures::channel::mpsc::TryRecvError::Closed) => {
                return Err("事件通道已关闭".into());
            }
            Err(_) => {
                if Instant::now() > deadline {
                    return Err("连接超时".into());
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

pub(crate) fn run(
    core: &Core,
    mut rx: UnboundedReceiver<CoreEvent>,
    spec: &SftpSpec,
    args: &Args,
) -> i32 {
    let t0 = Instant::now();
    let session = match open_sftp_session(core, spec, args) {
        Ok(s) => s,
        Err(e) => {
            log(&format!("打开 SFTP 会话失败：{e}"));
            return 1;
        }
    };
    if let Err(e) = wait_connected(
        core,
        &mut rx,
        session,
        Duration::from_secs(args.timeout_secs),
    ) {
        log(&format!("BENCH 失败：{e}"));
        return 1;
    }
    log(&format!("BENCH 连接耗时 {} ms", t0.elapsed().as_millis()));
    let path = match &spec.path {
        Some(p) => p.clone(),
        None => match futures::executor::block_on(core.sftp_home(session)) {
            Ok(h) => h,
            Err(e) => {
                log(&format!("BENCH 家目录失败：{e}"));
                return 1;
            }
        },
    };
    let mut code = 0;
    for round in 1..=2 {
        let start = Instant::now();
        let mut stream = match core.sftp_list(session, &path) {
            Ok(s) => s,
            Err(e) => {
                log(&format!("BENCH 列目录失败：{e}"));
                return 1;
            }
        };
        let mut model = DirModel::new(Sort::default());
        let (mut first, mut batches, mut max_batch, mut model_us) = (None, 0usize, 0usize, 0u128);
        loop {
            match futures::executor::block_on(stream.next_batch()) {
                Ok(Some(batch)) => {
                    first.get_or_insert_with(|| start.elapsed().as_millis());
                    batches += 1;
                    max_batch = max_batch.max(batch.len());
                    let m = Instant::now();
                    model.push_batch(batch);
                    model_us += m.elapsed().as_micros();
                }
                Ok(None) => break,
                Err(e) => {
                    log(&format!("BENCH 列目录出错：{e}"));
                    code = 1;
                    break;
                }
            }
        }
        log(&format!(
            "BENCH 第 {round} 次列 {path}：{} 项，{batches} 批（最大 {max_batch} 项/批），首批 {} ms，全部 {} ms，模型归并排序合计 {:.1} ms，顺序正确 {}；{}",
            model.len(),
            first.unwrap_or(0),
            start.elapsed().as_millis(),
            model_us as f64 / 1000.0,
            model.is_sorted(),
            memory::describe()
        ));
    }
    core.close_session(session);
    code
}
