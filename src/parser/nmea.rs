//! NMEA-0183 增量解析器。
//!
//! 支持的报文：GGA / RMC / GSA / GSV / GLL / VTG。
//! 未识别或非 NMEA 的字节会被安全忽略（便于解析混合日志）。

use chrono::Local;

use crate::model::{Dop, GnssSystem, Satellite};

use super::{ParseEvent, Parser};

/// 单行缓冲上限，避免异常数据撑爆内存。
const MAX_LINE: usize = 1024;

pub struct NmeaParser {
    buf: Vec<u8>,
}

impl NmeaParser {
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(256),
        }
    }
}

impl Default for NmeaParser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser for NmeaParser {
    fn feed(&mut self, bytes: &[u8], out: &mut Vec<ParseEvent>) {
        for &b in bytes {
            match b {
                b'\n' | b'\r' => {
                    if !self.buf.is_empty() {
                        let line = std::mem::take(&mut self.buf);
                        parse_line(&line, out);
                    }
                }
                _ => {
                    if self.buf.len() < MAX_LINE {
                        self.buf.push(b);
                    } else {
                        // 行过长，丢弃并重置，等待下一行同步。
                        self.buf.clear();
                    }
                }
            }
        }
    }
}

fn parse_line(line: &[u8], out: &mut Vec<ParseEvent>) {
    let text = match std::str::from_utf8(line) {
        Ok(t) => t.trim(),
        Err(_) => return, // 非文本，留给其他解析器处理
    };
    if !text.starts_with('$') && !text.starts_with('!') {
        // 非 NMEA 文本：尝试识别 CPU 耗时剖析行。
        parse_profile(text, out);
        return;
    }

    // 校验和（若存在）。
    let body = &text[1..];
    let (payload, checksum_ok) = match body.split_once('*') {
        Some((p, cs)) => {
            let calc = p.bytes().fold(0u8, |acc, c| acc ^ c);
            let ok = u8::from_str_radix(cs.trim(), 16)
                .map(|v| v == calc)
                .unwrap_or(false);
            (p, ok)
        }
        None => (body, true), // 无校验和则不强制校验
    };

    if !checksum_ok {
        out.push(ParseEvent::Error(format!("校验失败: {text}")));
        return;
    }

    let fields: Vec<&str> = payload.split(',').collect();
    if fields.is_empty() {
        return;
    }
    let addr = fields[0];
    if addr.len() < 5 {
        return;
    }
    let talker = &addr[0..2];
    let kind = &addr[2..];

    match kind {
        "GGA" => parse_gga(&fields, text, out),
        "RMC" => parse_rmc(&fields, text, out),
        "GSA" => parse_gsa(&fields, text, out),
        "GSV" => parse_gsv(talker, &fields, text, out),
        "GLL" | "VTG" => out.push(ParseEvent::Sentence(text.to_string())),
        _ => out.push(ParseEvent::Sentence(text.to_string())),
    }
}

/// 解析 CPU 耗时剖析行，例如：
/// `INFO->PROF(us): PE 100950  MOT 3131  ND 81104  PRT 5480  BBM 60  VIT 0`
/// 数字单位为微秒，转换为毫秒后产出 [`ParseEvent::Profile`]。
fn parse_profile(text: &str, out: &mut Vec<ParseEvent>) {
    const TAG: &str = "PROF(us):";
    let Some(idx) = text.find(TAG) else {
        return;
    };
    let body = &text[idx + TAG.len()..];
    // 兼容两种写法：`NAME value`（空格）与 `NAME=value`（等号），可在同一行混用。
    let tokens: Vec<&str> = body.split_whitespace().collect();
    let mut samples = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i];
        if let Some((name, val)) = tok.split_once('=') {
            // NAME=value
            if let Ok(us) = val.parse::<f64>() {
                samples.push((name.to_string(), us / 1000.0));
            }
            i += 1;
        } else if i + 1 < tokens.len() {
            // NAME value
            if let Ok(us) = tokens[i + 1].parse::<f64>() {
                samples.push((tok.to_string(), us / 1000.0));
                i += 2;
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    if !samples.is_empty() {
        out.push(ParseEvent::Sentence(text.to_string()));
        out.push(ParseEvent::Profile { samples });
    }
}

fn f(fields: &[&str], i: usize) -> Option<String> {
    fields.get(i).map(|s| s.to_string()).filter(|s| !s.is_empty())
}

fn parse_f64(fields: &[&str], i: usize) -> Option<f64> {
    fields.get(i).and_then(|s| s.parse::<f64>().ok())
}

fn parse_u(fields: &[&str], i: usize) -> Option<u16> {
    fields.get(i).and_then(|s| s.parse::<u16>().ok())
}

/// 解析 ddmm.mmmm + 半球 -> 十进制度。
fn parse_coord(value: Option<&&str>, hemi: Option<&&str>) -> Option<f64> {
    let v = value?.trim();
    if v.is_empty() {
        return None;
    }
    let raw: f64 = v.parse().ok()?;
    let deg = (raw / 100.0).trunc();
    let min = raw - deg * 100.0;
    let mut dec = deg + min / 60.0;
    if let Some(h) = hemi
        && matches!(*h, "S" | "W") {
            dec = -dec;
        }
    Some(dec)
}

fn parse_gga(fields: &[&str], text: &str, out: &mut Vec<ParseEvent>) {
    let lat = parse_coord(fields.get(2), fields.get(3));
    let lon = parse_coord(fields.get(4), fields.get(5));
    let event = ParseEvent::Fix {
        utc: f(fields, 1),
        latitude: lat,
        longitude: lon,
        fix_quality: fields.get(6).and_then(|s| s.parse::<u8>().ok()),
        sats_used: parse_u(fields, 7),
        hdop: parse_f64(fields, 8),
        altitude: parse_f64(fields, 9),
        geoid_sep: parse_f64(fields, 11),
    };
    out.push(event);
    out.push(ParseEvent::Sentence(text.to_string()));
}

fn parse_rmc(fields: &[&str], text: &str, out: &mut Vec<ParseEvent>) {
    let lat = parse_coord(fields.get(3), fields.get(4));
    let lon = parse_coord(fields.get(5), fields.get(6));
    let valid = fields.get(2).map(|s| *s == "A").unwrap_or(false);
    let date = f(fields, 9).map(|d| {
        // ddmmyy -> dd/mm/20yy
        if d.len() == 6 {
            format!("20{}-{}-{}", &d[4..6], &d[2..4], &d[0..2])
        } else {
            d
        }
    });
    out.push(ParseEvent::Rmc {
        utc: f(fields, 1),
        date,
        latitude: lat,
        longitude: lon,
        speed_kn: parse_f64(fields, 7),
        course_deg: parse_f64(fields, 8),
        valid,
    });
    out.push(ParseEvent::Sentence(text.to_string()));
    // RMC 通常每个历元一条，作为滑动窗口推进点。
    out.push(ParseEvent::EpochTick);
}

fn parse_gsa(fields: &[&str], text: &str, out: &mut Vec<ParseEvent>) {
    // fields: 0 addr,1 mode1,2 fix_type,3..15 prn(12),15 pdop,16 hdop,17 vdop
    let fix_type = fields.get(2).and_then(|s| s.parse::<u8>().ok());
    let mut used = Vec::new();
    for i in 3..15 {
        if let Some(prn) = parse_u(fields, i)
            && prn != 0 {
                used.push((GnssSystem::Unknown, prn));
            }
    }
    let dop = Dop {
        pdop: parse_f64(fields, 15),
        hdop: parse_f64(fields, 16),
        vdop: parse_f64(fields, 17),
    };
    out.push(ParseEvent::Dop {
        dop,
        fix_type,
        used_prns: used,
    });
    out.push(ParseEvent::Sentence(text.to_string()));
}

fn parse_gsv(talker: &str, fields: &[&str], text: &str, out: &mut Vec<ParseEvent>) {
    let system = GnssSystem::from_talker(talker);
    let now = Local::now();
    let mut sats = Vec::new();
    // 每条 GSV 最多 4 颗：从字段 4 开始，每 4 个一组 (prn, elev, az, cn0)。
    let mut i = 4;
    while i + 3 < fields.len() {
        let prn = parse_u(fields, i);
        if let Some(prn) = prn
            && prn != 0 {
                sats.push(Satellite {
                    system: Some(system),
                    prn,
                    elevation: parse_u(fields, i + 1),
                    azimuth: parse_u(fields, i + 2),
                    cn0: parse_u(fields, i + 3),
                    used_in_fix: false,
                    last_seen: Some(now),
                    trail: Default::default(),
                });
            }
        i += 4;
    }
    if !sats.is_empty() {
        out.push(ParseEvent::Satellites(sats));
    }
    out.push(ParseEvent::Sentence(text.to_string()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(input: &str) -> Vec<ParseEvent> {
        let mut p = NmeaParser::new();
        let mut out = Vec::new();
        p.feed(input.as_bytes(), &mut out);
        out
    }

    #[test]
    fn parses_gga_position() {
        let line = "$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*47\r\n";
        let events = collect(line);
        let fix = events
            .iter()
            .find_map(|e| match e {
                ParseEvent::Fix { latitude, longitude, altitude, sats_used, .. } => {
                    Some((*latitude, *longitude, *altitude, *sats_used))
                }
                _ => None,
            })
            .expect("应解析出 Fix");
        assert!((fix.0.unwrap() - 48.1173).abs() < 1e-3);
        assert!((fix.1.unwrap() - 11.5167).abs() < 1e-3);
        assert_eq!(fix.3, Some(8));
    }

    #[test]
    fn parses_gga_geoid_separation() {
        let body = "GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,";
        let cs = body.bytes().fold(0u8, |a, b| a ^ b);
        let events = collect(&format!("${body}*{cs:02X}\r\n"));
        let (alt, sep) = events
            .iter()
            .find_map(|e| match e {
                ParseEvent::Fix { altitude, geoid_sep, .. } => Some((*altitude, *geoid_sep)),
                _ => None,
            })
            .expect("应解析出 Fix");
        assert!((alt.unwrap() - 545.4).abs() < 1e-6);
        assert!((sep.unwrap() - 46.9).abs() < 1e-6);
        // 椭球高 = 海拔 + 差距
        assert!((alt.unwrap() + sep.unwrap() - 592.3).abs() < 1e-6);
    }

    #[test]
    fn parses_prof_line() {
        let events = collect("INFO->PROF(us): PE 100950  MOT 3131  ND 81104  PRT 5480  BBM 60  VIT 0\r\n");
        let samples = events
            .iter()
            .find_map(|e| match e {
                ParseEvent::Profile { samples } => Some(samples.clone()),
                _ => None,
            })
            .expect("应解析出 Profile");
        assert_eq!(samples.len(), 6);
        assert_eq!(samples[0].0, "PE");
        // 微秒 → 毫秒
        assert!((samples[0].1 - 100.95).abs() < 1e-6);
        assert_eq!(samples[2].0, "ND");
        assert!((samples[2].1 - 81.104).abs() < 1e-6);
        assert!((samples[5].1 - 0.0).abs() < 1e-9);
    }

    #[test]
    fn parses_prof_line_mixed_separators() {
        // 真实日志中 BBM/VIT 使用等号分隔。
        let events =
            collect("INFO->PROF(us): PE 414379  MOT 8026  ND 351050  PRT 23296  BBM=1089  VIT=0\r\n");
        let samples = events
            .iter()
            .find_map(|e| match e {
                ParseEvent::Profile { samples } => Some(samples.clone()),
                _ => None,
            })
            .expect("应解析出 Profile");
        assert_eq!(samples.len(), 6);
        assert_eq!(samples[4].0, "BBM");
        assert!((samples[4].1 - 1.089).abs() < 1e-6);
        assert_eq!(samples[5].0, "VIT");
        assert!((samples[5].1 - 0.0).abs() < 1e-9);
    }

    #[test]
    fn rejects_bad_checksum() {
        let line = "$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*00\r\n";
        let events = collect(line);
        assert!(events.iter().any(|e| matches!(e, ParseEvent::Error(_))));
    }

    #[test]
    fn parses_gsv_satellites() {
        let body = "GPGSV,2,1,08,01,40,083,46,03,67,123,42,06,12,030,30,11,55,200,38";
        let cs = body.bytes().fold(0u8, |a, b| a ^ b);
        let events = collect(&format!("${body}*{cs:02X}\r\n"));
        let sats = events.iter().find_map(|e| match e {
            ParseEvent::Satellites(v) => Some(v.clone()),
            _ => None,
        });
        let sats = sats.expect("应解析出卫星");
        assert_eq!(sats.len(), 4);
        assert_eq!(sats[0].prn, 1);
        assert_eq!(sats[0].cn0, Some(46));
    }
}
