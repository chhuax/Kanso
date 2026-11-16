use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use crate::cargo;

pub fn run(args: &[String]) -> ExitCode {
    let started = std::time::SystemTime::now();
    println!(
        "VT 为裸引擎 / 元数据路径；decorations 为四项装饰全关 / 全开的产帧路径，详见 docs/performance.md。"
    );
    let status = cargo()
        .args(["bench", "-p", "kanso-bench", "--bench", "terminal", "--"])
        .args(args)
        .status();
    if !status.is_ok_and(|status| status.success()) {
        return ExitCode::FAILURE;
    }
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target"));
    match summary(&target.join("criterion"), started) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("基准摘要失败：{error}");
            ExitCode::FAILURE
        }
    }
}

fn summary(root: &Path, started: std::time::SystemTime) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n| 基准 | 平均耗时 | 95% 置信区间 | 输入吞吐 MB/s |\n|---|---:|---:|---:|");
    let mut groups = std::fs::read_dir(root)?.collect::<Result<Vec<_>, _>>()?;
    groups.sort_by_key(|entry| entry.file_name());
    for group in groups.into_iter().filter(|entry| entry.path().is_dir()) {
        let mut cases = std::fs::read_dir(group.path())?.collect::<Result<Vec<_>, _>>()?;
        cases.sort_by_key(|entry| entry.file_name());
        let mut bare = None;
        let mut metadata = None;
        let mut off = None;
        let mut all = None;
        for case in cases {
            let result = case.path().join("new/estimates.json");
            if !result.exists() || result.metadata()?.modified()? < started {
                continue;
            }
            let estimates: serde_json::Value = serde_json::from_slice(&std::fs::read(result)?)?;
            let benchmark: serde_json::Value =
                serde_json::from_slice(&std::fs::read(case.path().join("new/benchmark.json"))?)?;
            let mean = estimates["mean"]["point_estimate"]
                .as_f64()
                .ok_or("缺少平均耗时")?;
            let lower = estimates["mean"]["confidence_interval"]["lower_bound"]
                .as_f64()
                .ok_or("缺少置信区间")?;
            let upper = estimates["mean"]["confidence_interval"]["upper_bound"]
                .as_f64()
                .ok_or("缺少置信区间")?;
            let bytes = benchmark["throughput"]["Bytes"].as_f64();
            let throughput = bytes.map_or_else(
                || "—".into(),
                |bytes| format!("{:.2}", bytes / mean * 1000.0),
            );
            let (unit, divisor) = if mean < 1000.0 {
                ("ns", 1.0)
            } else {
                ("ms", 1e6)
            };
            println!(
                "| {}/{} | {:.3} {} | {:.3}–{:.3} {} | {} |",
                group.file_name().to_string_lossy(),
                case.file_name().to_string_lossy(),
                mean / divisor,
                unit,
                lower / divisor,
                upper / divisor,
                unit,
                throughput
            );
            match case.file_name().to_str() {
                Some("bare") => bare = Some(mean),
                Some("metadata") => metadata = Some(mean),
                Some("off") => off = Some(mean),
                Some("all") => all = Some(mean),
                _ => {}
            }
        }
        if let (Some(bare), Some(metadata)) = (bare, metadata) {
            println!(
                "| {} 元数据相对裸引擎吞吐下降 | {:.2}% | | |",
                group.file_name().to_string_lossy(),
                (1.0 - bare / metadata) * 100.0
            );
        }
        if let (Some(off), Some(all)) = (off, all) {
            let decline = (1.0 - off / all) * 100.0;
            println!(
                "| {} 四项全开相对全关吞吐下降 | {decline:.2}% | | |",
                group.file_name().to_string_lossy()
            );
            if decline > 10.0 {
                return Err("四项装饰吞吐下降超过 10%".into());
            }
        }
    }
    Ok(())
}

pub fn startup(args: &[String]) -> ExitCode {
    if Command::new("python3")
        .arg("tools/measure-startup.py")
        .args(args)
        .status()
        .is_ok_and(|status| status.success())
    {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
