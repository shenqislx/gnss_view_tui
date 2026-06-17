//! 数据管线：原始字节 -> 落盘 + 解析 -> 更新共享状态。
//!
//! 串口/回放/演示线程只负责产出 [`RawChunk`]，本管线集中处理：
//! 1. 先落盘（保证不丢数）；
//! 2. 再解析为 [`ParseEvent`]；
//! 3. 最后归并进 [`AppState`]。

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Local};

use crate::ext::EventObserver;
use crate::model::{AppState, Dop, LogKind};
use crate::parser::{ParseEvent, Parser};
use crate::recorder::Recorder;

/// 一块带 PC 接收时间的原始数据。
pub struct RawChunk {
    pub data: Vec<u8>,
    pub pc_time: DateTime<Local>,
}

/// 运行数据管线，直到 `rx` 关闭。
pub fn run(
    rx: Receiver<RawChunk>,
    state: Arc<Mutex<AppState>>,
    mut parser: Box<dyn Parser>,
    mut recorder: Option<Recorder>,
    observer: Option<Box<dyn EventObserver>>,
) {
    let mut events: Vec<ParseEvent> = Vec::with_capacity(64);

    if let Some(rec) = &recorder
        && let Ok(mut s) = state.lock() {
            s.stats.recording_path = Some(rec.path().display().to_string());
            s.push_log(
                LogKind::Info,
                format!("原始数据落盘: {}", rec.path().display()),
            );
        }

    while let Ok(chunk) = rx.recv() {
        // 1) 落盘
        if let Some(rec) = recorder.as_mut()
            && let Err(e) = rec.write(&chunk.data, chunk.pc_time)
                && let Ok(mut s) = state.lock() {
                    s.push_log(LogKind::Error, format!("落盘失败: {e}"));
                }

        // 2) 解析
        events.clear();
        parser.feed(&chunk.data, &mut events);

        // 3) 更新状态
        if let Ok(mut s) = state.lock() {
            s.stats.bytes_total += chunk.data.len() as u64;
            s.stats.last_rx = Some(chunk.pc_time);
            for ev in events.drain(..) {
                // 锁顺序铁律：持 AppState 锁内回调观察者（其内部再锁私有状态）。
                // 仅在存在观察者时克隆事件，零观察者时零开销。
                match &observer {
                    Some(obs) => {
                        apply_event(&mut s, ev.clone());
                        obs.observe(&ev, chunk.pc_time, &s);
                    }
                    None => apply_event(&mut s, ev),
                }
            }
            s.prune_satellites(15);
        }
    }

    if let Some(mut rec) = recorder.take() {
        let _ = rec.flush();
    }
}

/// 测试辅助：在测试中将单个事件应用到状态。
#[cfg(test)]
pub fn apply_event_for_test(s: &mut AppState, ev: ParseEvent) {
    apply_event(s, ev);
}

fn apply_event(s: &mut AppState, ev: ParseEvent) {
    match ev {
        ParseEvent::Satellites(sats) => {
            s.stats.sentences_ok += 1;
            for sat in sats {
                s.upsert_satellite(sat);
            }
        }
        ParseEvent::Fix {
            utc,
            latitude,
            longitude,
            altitude,
            geoid_sep,
            fix_quality,
            sats_used,
            hdop,
        } => {
            s.stats.sentences_ok += 1;
            let pvt = &mut s.pvt;
            if utc.is_some() {
                pvt.utc = utc;
            }
            if latitude.is_some() {
                pvt.latitude = latitude;
            }
            if longitude.is_some() {
                pvt.longitude = longitude;
            }
            if altitude.is_some() {
                pvt.altitude = altitude;
            }
            if geoid_sep.is_some() {
                pvt.geoid_sep = geoid_sep;
            }
            if fix_quality.is_some() {
                pvt.fix_quality = fix_quality;
            }
            if sats_used.is_some() {
                pvt.sats_used = sats_used;
            }
            if hdop.is_some() {
                s.dop.hdop = hdop;
            }
        }
        ParseEvent::Rmc {
            utc,
            date,
            latitude,
            longitude,
            speed_kn,
            course_deg,
            valid,
        } => {
            s.stats.sentences_ok += 1;
            let pvt = &mut s.pvt;
            if utc.is_some() {
                pvt.utc = utc;
            }
            if date.is_some() {
                pvt.date = date;
            }
            if latitude.is_some() {
                pvt.latitude = latitude;
            }
            if longitude.is_some() {
                pvt.longitude = longitude;
            }
            if speed_kn.is_some() {
                pvt.speed_kn = speed_kn;
            }
            if course_deg.is_some() {
                pvt.course_deg = course_deg;
            }
            pvt.valid = valid;
        }
        ParseEvent::Dop {
            dop,
            fix_type,
            used_prns,
        } => {
            s.stats.sentences_ok += 1;
            merge_dop(&mut s.dop, dop);
            if fix_type.is_some() {
                s.pvt.fix_type = fix_type;
            }
            // 先清空在用标记，再按 GSA 重新标记。
            let used_set: Vec<u16> = used_prns.iter().map(|(_, prn)| *prn).collect();
            for sat in s.satellites.values_mut() {
                if used_set.contains(&sat.prn) {
                    sat.used_in_fix = true;
                }
            }
        }
        ParseEvent::Profile { samples } => {
            s.stats.sentences_ok += 1;
            s.push_profile(&samples);
        }
        ParseEvent::Metric { name, value } => {
            // 自定义二进制指标也并入剖析曲线（作为单独一个系列）。
            s.stats.sentences_ok += 1;
            s.push_profile(&[(name, value)]);
        }
        ParseEvent::Sentence(text) => {
            s.push_log(LogKind::Rx, text);
        }
        ParseEvent::Error(text) => {
            s.stats.parse_errors += 1;
            s.push_log(LogKind::Error, text);
        }
        ParseEvent::EpochTick => {
            // 历元推进：若已有有效经纬度，记录一个轨迹点并累计位置偏差。
            if let (Some(lat), Some(lon)) = (s.pvt.latitude, s.pvt.longitude) {
                s.push_track_point(lat, lon);
                let alt = s.pvt.altitude;
                s.push_pos_bias(lat, lon, alt);
            }
        }
    }
}

fn merge_dop(dst: &mut Dop, src: Dop) {
    if src.pdop.is_some() {
        dst.pdop = src.pdop;
    }
    if src.hdop.is_some() {
        dst.hdop = src.hdop;
    }
    if src.vdop.is_some() {
        dst.vdop = src.vdop;
    }
}
