//! GNSS（NMEA + 私有协议）长期监控 TUI —— 库入口。
//!
//! 架构总览：
//! ```text
//!   [来源线程]                 [管线线程]                 [界面线程/主线程]
//!   serial/replay/demo  --raw-->  落盘 + 解析 + 归并  --状态-->  ratatui 渲染
//!        ^                                                          |
//!        +---------------------- TX 命令 ----------------------------+
//! ```
//!
//! [`run`] 是统一入口：开源默认 bin 以 `None` 注入；外部（私有）功能可通过
//! [`UiExt`] / [`EventObserver`] 注入扩展视图与事件观察者，core 自身不依赖任何实现。

mod app;
mod config;
mod demo;
mod ext;
mod model;
mod parser;
mod pipeline;
mod recorder;
mod source;
mod ui;

use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Result, bail};

use crate::app::App;
use crate::config::Source;
use crate::model::AppState;
use crate::parser::build_parser;
use crate::recorder::Recorder;

// 对外导出：扩展接缝与私有 crate 需要的类型。
pub use config::Cli;
pub use ext::{EventObserver, Screen, UiExt};
pub use source::TxCommand;

/// 只读数据模型与解析事件（供扩展实现读取）。
pub mod state_api {
    pub use crate::model::*;
    pub use crate::parser::ParseEvent;
}

// Re-export 出现在接缝签名中的第三方库，锁定唯一版本（扩展实现方应经此引用）。
pub use chrono;
pub use ratatui;
pub use ratatui::crossterm;

/// 运行应用直到退出。
///
/// - `cli`：核心运行参数（来源、串口、协议、落盘、窗口、地面真值等）。
/// - `ui_ext`：可选 UI 扩展（占据扩展屏，按 `Tab` 切换）。
/// - `observer`：可选管线事件观察者（在管线线程被回调）。
///
/// 开源默认 bin 传 `None, None`，行为与无扩展时完全一致。
pub fn run(
    cli: Cli,
    ui_ext: Option<Box<dyn UiExt>>,
    observer: Option<Box<dyn EventObserver>>,
) -> Result<()> {
    // 串口模式必须指定端口（否则提示使用 --demo）。
    if matches!(cli.source(), Source::Serial) && cli.port.is_none() {
        bail!("未指定串口。请用 --port <设备> 指定，或使用 --demo 体验演示，或 --replay <文件> 回放。");
    }

    // 加载地面真值（Ground Truth）：有效则作为固定参考点，否则回退首点。
    let (gt_config, gt_logs) = cli.load_ground_truth();
    let mut app_state = AppState::new(cli.source_label(), cli.window_clamped());
    if let Some(gt) = gt_config {
        app_state.ground_truth = Some(crate::model::GroundTruth {
            lat: gt.latitude,
            lon: gt.longitude,
            alt: gt.altitude,
            valid: true,
        });
    }
    for msg in gt_logs {
        app_state.push_log(crate::model::LogKind::Info, msg);
    }
    let state = Arc::new(Mutex::new(app_state));
    let running = Arc::new(AtomicBool::new(true));

    // 通道：来源 -> 管线（原始数据）；界面 -> 串口（TX 命令）。
    let (raw_tx, raw_rx) = mpsc::channel();
    let (cmd_tx, cmd_rx) = mpsc::channel();

    // 落盘器：仅实时串口模式默认开启（回放/演示不重复落盘）。
    let recorder = match (cli.source(), cli.no_record) {
        (Source::Serial, false) => match Recorder::new(&cli.output) {
            Ok(r) => Some(r),
            Err(e) => {
                eprintln!("初始化落盘失败: {e}");
                None
            }
        },
        _ => None,
    };

    // 管线线程
    let parser = build_parser(cli.protocol);
    let pipeline_state = Arc::clone(&state);
    let pipeline_handle = thread::spawn(move || {
        pipeline::run(raw_rx, pipeline_state, parser, recorder, observer);
    });

    // 来源线程
    let source_handle = match cli.source() {
        Source::Serial => source::spawn_serial(
            &cli,
            raw_tx,
            cmd_rx,
            Arc::clone(&state),
            Arc::clone(&running),
        ),
        Source::Replay(path) => {
            source::spawn_replay(path, raw_tx, Arc::clone(&state), Arc::clone(&running))
        }
        Source::Demo => source::spawn_demo(raw_tx, Arc::clone(&state), Arc::clone(&running)),
    };

    // 界面（主线程）。失败也要保证恢复终端。
    let mut terminal = ratatui::init();
    let mut app = App::new(Arc::clone(&state), cmd_tx, Arc::clone(&running)).with_ext(ui_ext);
    let ui_result = app.run(&mut terminal);
    ratatui::restore();

    // 收尾：通知所有线程退出。
    running.store(false, std::sync::atomic::Ordering::Relaxed);
    let _ = source_handle.join();
    // 管线线程在 raw_tx 全部释放后自然结束。
    drop(app);
    let _ = pipeline_handle.join();

    ui_result
}
