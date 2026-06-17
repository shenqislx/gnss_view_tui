//! 自定义二进制协议解析器（示例实现，演示可扩展性）。
//!
//! 帧结构：
//! ```text
//! +------+------+-------+--------+-----------+----------+
//! | 0xAA | 0x55 | class | len:u16| payload   | checksum |
//! +------+------+-------+--------+-----------+----------+
//!                       (LE)      (len bytes)  (XOR)
//! ```
//! checksum = class ^ len_lo ^ len_hi ^ payload[..]
//!
//! 已定义的 class：
//! - `0x01` Metric：payload = name_len:u8, name[name_len], value:f32(LE)
//!   -> 产出一个自定义曲线指标。
//!
//! 后续新增 UBX / RTCM / 自研协议时，可参照此文件实现独立的同步与解析逻辑。

use super::{ParseEvent, Parser};

const SYNC1: u8 = 0xAA;
const SYNC2: u8 = 0x55;
const MAX_PAYLOAD: usize = 4096;

#[derive(Debug)]
enum State {
    Sync1,
    Sync2,
    Class,
    LenLo,
    LenHi { class: u8 },
    Payload { class: u8, len: usize },
    Checksum { class: u8, len: usize },
}

pub struct CustomParser {
    state: State,
    payload: Vec<u8>,
    class: u8,
    len: usize,
}

impl CustomParser {
    pub fn new() -> Self {
        Self {
            state: State::Sync1,
            payload: Vec::with_capacity(64),
            class: 0,
            len: 0,
        }
    }

    fn reset(&mut self) {
        self.state = State::Sync1;
        self.payload.clear();
    }
}

impl Default for CustomParser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser for CustomParser {
    fn feed(&mut self, bytes: &[u8], out: &mut Vec<ParseEvent>) {
        for &b in bytes {
            match self.state {
                State::Sync1 => {
                    if b == SYNC1 {
                        self.state = State::Sync2;
                    }
                }
                State::Sync2 => {
                    self.state = if b == SYNC2 {
                        State::Class
                    } else if b == SYNC1 {
                        State::Sync2
                    } else {
                        State::Sync1
                    };
                }
                State::Class => {
                    self.class = b;
                    self.state = State::LenLo;
                }
                State::LenLo => {
                    self.len = b as usize;
                    self.state = State::LenHi { class: self.class };
                }
                State::LenHi { class } => {
                    self.len |= (b as usize) << 8;
                    if self.len > MAX_PAYLOAD {
                        out.push(ParseEvent::Error(format!("自定义帧长度异常: {}", self.len)));
                        self.reset();
                    } else if self.len == 0 {
                        self.state = State::Checksum { class, len: 0 };
                    } else {
                        self.payload.clear();
                        self.state = State::Payload {
                            class,
                            len: self.len,
                        };
                    }
                }
                State::Payload { class, len } => {
                    self.payload.push(b);
                    if self.payload.len() == len {
                        self.state = State::Checksum { class, len };
                    }
                }
                State::Checksum { class, len } => {
                    let lo = (len & 0xFF) as u8;
                    let hi = ((len >> 8) & 0xFF) as u8;
                    let calc = self
                        .payload
                        .iter()
                        .fold(class ^ lo ^ hi, |acc, &x| acc ^ x);
                    if calc == b {
                        decode_frame(class, &self.payload, out);
                    } else {
                        out.push(ParseEvent::Error("自定义帧校验失败".to_string()));
                    }
                    self.reset();
                }
            }
        }
    }
}

fn decode_frame(class: u8, payload: &[u8], out: &mut Vec<ParseEvent>) {
    match class {
        0x01 => {
            if payload.is_empty() {
                return;
            }
            let name_len = payload[0] as usize;
            if payload.len() < 1 + name_len + 4 {
                out.push(ParseEvent::Error("Metric 帧长度不足".to_string()));
                return;
            }
            let name = String::from_utf8_lossy(&payload[1..1 + name_len]).to_string();
            let value_bytes = &payload[1 + name_len..1 + name_len + 4];
            let value = f32::from_le_bytes([
                value_bytes[0],
                value_bytes[1],
                value_bytes[2],
                value_bytes[3],
            ]) as f64;
            out.push(ParseEvent::Sentence(format!("[CUSTOM] {name}={value:.3}")));
            out.push(ParseEvent::Metric { name, value });
            out.push(ParseEvent::EpochTick);
        }
        other => {
            out.push(ParseEvent::Sentence(format!("[CUSTOM] 未知 class=0x{other:02X}")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一帧 Metric 数据。
    fn metric_frame(name: &str, value: f32) -> Vec<u8> {
        let mut payload = vec![name.len() as u8];
        payload.extend_from_slice(name.as_bytes());
        payload.extend_from_slice(&value.to_le_bytes());
        let len = payload.len();
        let lo = (len & 0xFF) as u8;
        let hi = ((len >> 8) & 0xFF) as u8;
        let cs = payload.iter().fold(0x01u8 ^ lo ^ hi, |a, &b| a ^ b);
        let mut frame = vec![SYNC1, SYNC2, 0x01, lo, hi];
        frame.extend_from_slice(&payload);
        frame.push(cs);
        frame
    }

    #[test]
    fn decodes_metric_frame() {
        let mut p = CustomParser::new();
        let mut out = Vec::new();
        // 前面混入噪声字节，验证同步逻辑。
        let mut stream = vec![0x00, 0xAA, 0x11, 0xFF];
        stream.extend(metric_frame("temp", 36.6));
        p.feed(&stream, &mut out);
        let metric = out.iter().find_map(|e| match e {
            ParseEvent::Metric { name, value } => Some((name.clone(), *value)),
            _ => None,
        });
        let (name, value) = metric.expect("应解析出 Metric");
        assert_eq!(name, "temp");
        assert!((value - 36.6).abs() < 1e-3);
    }
}
