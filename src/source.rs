//! 数据来源线程：串口 / 回放 / 演示，统一产出 [`RawChunk`] 喂给管线。

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use chrono::Local;

use crate::config::Cli;
use crate::model::{AppState, LogKind};
use crate::pipeline::RawChunk;

/// 串口写/控制命令（TX）。
pub enum TxCommand {
    /// 发送原始字节。
    Bytes(Vec<u8>),
    /// DTR 复位脉冲：拉低 DTR `low_ms` 毫秒后恢复（类似 picocom 的 pulse DTR）。
    DtrReset { low_ms: u64 },
}

/// 实时串口读写线程。
///
/// 单线程同时负责读与写：读取使用短超时，空闲间隙处理 TX 命令，
/// 从而避免共享端口句柄带来的并发复杂度。
pub fn spawn_serial(
    cli: &Cli,
    raw_tx: Sender<RawChunk>,
    cmd_rx: Receiver<TxCommand>,
    state: Arc<Mutex<AppState>>,
    running: Arc<AtomicBool>,
) -> JoinHandle<()> {
    let port_name = cli.port.clone().unwrap_or_default();
    let baud = cli.baud;
    let data_bits = match cli.data_bits {
        5 => serialport::DataBits::Five,
        6 => serialport::DataBits::Six,
        7 => serialport::DataBits::Seven,
        _ => serialport::DataBits::Eight,
    };
    let parity: serialport::Parity = cli.parity.into();
    let stop_bits = match cli.stop_bits {
        2 => serialport::StopBits::Two,
        _ => serialport::StopBits::One,
    };

    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while running.load(Ordering::Relaxed) {
            let open = serialport::new(&port_name, baud)
                .data_bits(data_bits)
                .parity(parity)
                .stop_bits(stop_bits)
                .timeout(Duration::from_millis(50))
                .open();

            let mut port = match open {
                Ok(p) => {
                    if let Ok(mut s) = state.lock() {
                        s.stats.connected = true;
                        s.push_log(LogKind::Info, format!("已打开串口 {port_name}@{baud}"));
                    }
                    p
                }
                Err(e) => {
                    if let Ok(mut s) = state.lock() {
                        s.stats.connected = false;
                        s.push_log(LogKind::Error, format!("打开串口失败: {e}，2s 后重试"));
                    }
                    if wait_or_stop(&running, Duration::from_secs(2)) {
                        break;
                    }
                    continue;
                }
            };

            // 读写循环
            loop {
                if !running.load(Ordering::Relaxed) {
                    return;
                }
                // 处理待发送命令
                while let Ok(cmd) = cmd_rx.try_recv() {
                    match cmd {
                        TxCommand::Bytes(data) => {
                            if let Err(e) = port.write_all(&data) {
                                if let Ok(mut s) = state.lock() {
                                    s.push_log(LogKind::Error, format!("发送失败: {e}"));
                                }
                                break;
                            }
                            let _ = port.flush();
                        }
                        TxCommand::DtrReset { low_ms } => {
                            let r = port
                                .write_data_terminal_ready(false)
                                .and_then(|_| {
                                    thread::sleep(Duration::from_millis(low_ms));
                                    port.write_data_terminal_ready(true)
                                });
                            if let Ok(mut s) = state.lock() {
                                match r {
                                    Ok(_) => s.push_log(
                                        LogKind::Info,
                                        format!("已发送 DTR 复位脉冲 ({low_ms}ms)"),
                                    ),
                                    Err(e) => {
                                        s.push_log(LogKind::Error, format!("DTR 复位失败: {e}"))
                                    }
                                }
                            }
                        }
                    }
                }

                match port.read(&mut buf) {
                    Ok(0) => {}
                    Ok(n) => {
                        let chunk = RawChunk {
                            data: buf[..n].to_vec(),
                            pc_time: Local::now(),
                        };
                        if raw_tx.send(chunk).is_err() {
                            return;
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(e) => {
                        if let Ok(mut s) = state.lock() {
                            s.stats.connected = false;
                            s.push_log(LogKind::Error, format!("读取错误: {e}，尝试重连"));
                        }
                        break; // 跳出读写循环，重新打开
                    }
                }
            }

            if wait_or_stop(&running, Duration::from_secs(1)) {
                break;
            }
        }
    })
}

/// 回放线程：读取此前落盘的 `.raw` 文件，按块喂入管线。
pub fn spawn_replay(
    path: std::path::PathBuf,
    raw_tx: Sender<RawChunk>,
    state: Arc<Mutex<AppState>>,
    running: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) => {
                if let Ok(mut s) = state.lock() {
                    s.push_log(LogKind::Error, format!("打开回放文件失败: {e}"));
                }
                return;
            }
        };
        if let Ok(mut s) = state.lock() {
            s.stats.connected = true;
            s.push_log(LogKind::Info, format!("开始回放: {}", path.display()));
        }
        let mut reader = std::io::BufReader::new(file);
        let mut buf = [0u8; 4096];
        loop {
            if !running.load(Ordering::Relaxed) {
                return;
            }
            match reader.read(&mut buf) {
                Ok(0) => {
                    if let Ok(mut s) = state.lock() {
                        s.stats.connected = false;
                        s.push_log(LogKind::Info, "回放结束".to_string());
                    }
                    return;
                }
                Ok(n) => {
                    let chunk = RawChunk {
                        data: buf[..n].to_vec(),
                        pc_time: Local::now(),
                    };
                    if raw_tx.send(chunk).is_err() {
                        return;
                    }
                    // 控制回放节奏，模拟实时。
                    if wait_or_stop(&running, Duration::from_millis(80)) {
                        return;
                    }
                }
                Err(e) => {
                    if let Ok(mut s) = state.lock() {
                        s.push_log(LogKind::Error, format!("回放读取错误: {e}"));
                    }
                    return;
                }
            }
        }
    })
}

/// 演示线程：生成内置模拟 NMEA 数据，无需硬件即可体验。
pub fn spawn_demo(
    raw_tx: Sender<RawChunk>,
    state: Arc<Mutex<AppState>>,
    running: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        if let Ok(mut s) = state.lock() {
            s.stats.connected = true;
            s.push_log(LogKind::Info, "演示模式：生成模拟 GNSS 数据".to_string());
        }
        let mut t: f64 = 0.0;
        loop {
            if !running.load(Ordering::Relaxed) {
                return;
            }
            let sentences = crate::demo::generate_epoch(t);
            let chunk = RawChunk {
                data: sentences.into_bytes(),
                pc_time: Local::now(),
            };
            if raw_tx.send(chunk).is_err() {
                return;
            }
            t += 1.0;
            if wait_or_stop(&running, Duration::from_millis(1000)) {
                return;
            }
        }
    })
}

/// 可中断的等待：返回 true 表示收到停止信号。
fn wait_or_stop(running: &Arc<AtomicBool>, dur: Duration) -> bool {
    let step = Duration::from_millis(20);
    let mut waited = Duration::ZERO;
    while waited < dur {
        if !running.load(Ordering::Relaxed) {
            return true;
        }
        thread::sleep(step);
        waited += step;
    }
    !running.load(Ordering::Relaxed)
}
