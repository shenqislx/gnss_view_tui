//! 内置演示数据生成器：合成带正确校验和的 NMEA 报文流。

/// 计算并附加 NMEA 校验和，返回完整一行（含 CRLF）。
fn nmea_line(body: &str) -> String {
    let cs = body.bytes().fold(0u8, |acc, b| acc ^ b);
    format!("${body}*{cs:02X}\r\n")
}

/// 生成一个历元（约 1 秒）的模拟报文。`t` 为递增的秒计数。
pub fn generate_epoch(t: f64) -> String {
    let mut out = String::new();

    // 缓慢漂移的坐标（上海附近）。
    let lat = 31.2304 + (t * 0.0001).sin() * 0.0005;
    let lon = 121.4737 + (t * 0.0001).cos() * 0.0005;
    let (lat_nmea, lat_h) = to_nmea_lat(lat);
    let (lon_nmea, lon_h) = to_nmea_lon(lon);
    let alt = 12.0 + (t * 0.05).sin() * 3.0;

    let hh = (t as u64 / 3600) % 24;
    let mm = (t as u64 / 60) % 60;
    let ss = t as u64 % 60;
    let utc = format!("{hh:02}{mm:02}{ss:02}.00");

    // GGA：定位质量、使用卫星数、HDOP、海拔、大地水准面差距。
    let sats_used = 9 + ((t as i64) % 3);
    let hdop = 0.8 + (t * 0.1).sin().abs() * 0.4;
    // 大地水准面差距（上海一带约 +8~9 m），用于演示椭球高与海拔分开展示。
    let geoid_sep = 8.5 + (t * 0.02).sin() * 0.3;
    out.push_str(&nmea_line(&format!(
        "GPGGA,{utc},{lat_nmea},{lat_h},{lon_nmea},{lon_h},1,{sats_used:02},{hdop:.1},{alt:.1},M,{geoid_sep:.1},M,,"
    )));

    // RMC：日期、速度、航向、有效性。
    let speed = 0.2 + (t * 0.2).sin().abs() * 1.5;
    let course = (t * 5.0) % 360.0;
    out.push_str(&nmea_line(&format!(
        "GPRMC,{utc},A,{lat_nmea},{lat_h},{lon_nmea},{lon_h},{speed:.1},{course:.1},150626,,,A"
    )));

    // GSA：定位类型 3D、PDOP/HDOP/VDOP，以及在用 PRN。
    let pdop = 1.2 + (t * 0.07).sin().abs() * 0.6;
    let vdop = 1.0 + (t * 0.05).cos().abs() * 0.6;
    out.push_str(&nmea_line(&format!(
        "GPGSA,A,3,01,03,06,11,17,19,22,28,,,,,{pdop:.1},{hdop:.1},{vdop:.1}"
    )));

    // GSV：GPS 8 颗，CN0 随时间起伏。
    out.push_str(&gsv_block("GP", t, 8, &[1, 3, 6, 11, 17, 19, 22, 28], 0.0));
    // GSV：BeiDou 6 颗。
    out.push_str(&gsv_block("GB", t, 6, &[7, 10, 13, 20, 26, 30], 1.7));

    // CPU 耗时剖析（微秒），各模块随时间波动，供 CPU Load 曲线演示。
    let pe = 90000.0 + (t * 0.4).sin() * 20000.0;
    let mot = 3000.0 + (t * 0.6).cos() * 1500.0;
    let nd = 75000.0 + (t * 0.3).sin() * 15000.0;
    let prt = 5000.0 + (t * 0.5).sin().abs() * 2000.0;
    let bbm = 60.0 + (t * 0.7).cos().abs() * 40.0;
    let vit = (t * 0.9).sin().abs() * 800.0;
    out.push_str(&format!(
        "INFO->PROF(us): PE {pe:.0}  MOT {mot:.0}  ND {nd:.0}  PRT {prt:.0}  BBM {bbm:.0}  VIT {vit:.0}\r\n"
    ));

    out
}

fn gsv_block(talker: &str, t: f64, total: u16, prns: &[u16], phase: f64) -> String {
    let mut out = String::new();
    let msgs = total.div_ceil(4);
    for m in 0..msgs {
        let mut body = format!("{talker}GSV,{msgs},{},{total}", m + 1);
        for i in 0..4 {
            let idx = (m * 4 + i) as usize;
            if idx >= prns.len() {
                break;
            }
            let prn = prns[idx];
            let elev = 20 + (idx as u16 * 7) % 60;
            let az = (idx as u16 * 47 + (t as u16 * 2)) % 360;
            // CN0 在 28~48 之间起伏，不同卫星相位不同。
            let cn0 =
                38.0 + ((t * 0.3 + phase + idx as f64 * 0.9).sin()) * 9.0;
            let cn0 = cn0.clamp(20.0, 52.0) as u16;
            body.push_str(&format!(",{prn:02},{elev:02},{az:03},{cn0:02}"));
        }
        out.push_str(&nmea_line(&body));
    }
    out
}

fn to_nmea_lat(deg: f64) -> (String, char) {
    let hemi = if deg >= 0.0 { 'N' } else { 'S' };
    let a = deg.abs();
    let d = a.trunc();
    let m = (a - d) * 60.0;
    (format!("{:02}{:07.4}", d as u32, m), hemi)
}

fn to_nmea_lon(deg: f64) -> (String, char) {
    let hemi = if deg >= 0.0 { 'E' } else { 'W' };
    let a = deg.abs();
    let d = a.trunc();
    let m = (a - d) * 60.0;
    (format!("{:03}{:07.4}", d as u32, m), hemi)
}

#[cfg(test)]
mod tests {
    use crate::parser::{ParseEvent, Parser, nmea::NmeaParser};

    #[test]
    fn demo_stream_is_valid_nmea() {
        let data = super::generate_epoch(5.0);
        let mut parser = NmeaParser::new();
        let mut out = Vec::new();
        parser.feed(data.as_bytes(), &mut out);
        // 不应有校验错误，且应包含定位与卫星事件。
        assert!(!out.iter().any(|e| matches!(e, ParseEvent::Error(_))), "演示数据校验和应正确");
        assert!(out.iter().any(|e| matches!(e, ParseEvent::Fix { .. })));
        assert!(out.iter().any(|e| matches!(e, ParseEvent::Satellites(_))));
        assert!(out.iter().any(|e| matches!(e, ParseEvent::Dop { .. })));
    }
}
