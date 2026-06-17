//! 协议解析层：统一的 `Parser` trait + 多协议实现。
//!
//! 设计要点：
//! - 解析器是**纯函数式增量解析**：喂入原始字节，产出 [`ParseEvent`]，不直接触碰共享状态。
//!   这样同一套解析器既可用于实时串口，也可用于回放历史文件。
//! - 通过 trait 抽象，后续新增 UBX / RTCM / 自研协议只需实现 [`Parser`]。

pub mod custom;
pub mod nmea;

use crate::config::Protocol;
use crate::model::{Dop, GnssSystem, Satellite};

/// 解析产生的事件。
#[derive(Clone, Debug)]
pub enum ParseEvent {
    /// 一组卫星观测（来自 GSV）。
    Satellites(Vec<Satellite>),
    /// 定位信息（来自 GGA）。
    Fix {
        utc: Option<String>,
        latitude: Option<f64>,
        longitude: Option<f64>,
        /// 海拔（海平面/大地水准面以上正高，GGA 字段 9）。
        altitude: Option<f64>,
        /// 大地水准面差距（geoidal separation，GGA 字段 11）。
        geoid_sep: Option<f64>,
        fix_quality: Option<u8>,
        sats_used: Option<u16>,
        hdop: Option<f64>,
    },
    /// 运动信息（来自 RMC）。
    Rmc {
        utc: Option<String>,
        date: Option<String>,
        latitude: Option<f64>,
        longitude: Option<f64>,
        speed_kn: Option<f64>,
        course_deg: Option<f64>,
        valid: bool,
    },
    /// DOP 与定位类型（来自 GSA）。
    Dop {
        dop: Dop,
        fix_type: Option<u8>,
        used_prns: Vec<(GnssSystem, u16)>,
    },
    /// 自定义曲线指标。
    Metric { name: String, value: f64 },
    /// CPU 耗时剖析（来自 `INFO->PROF(us): ...`），每个模块一个采样，单位毫秒。
    Profile { samples: Vec<(String, f64)> },
    /// 一条已识别的报文（用于控制台展示）。
    Sentence(String),
    /// 解析错误（如校验失败）。
    Error(String),
    /// 一个历元结束的标记（GGA 触发，供后续聚合逻辑使用）。
    EpochTick,
}

/// 增量解析器统一接口。
pub trait Parser: Send {
    /// 喂入原始字节，向 `out` 追加解析事件。
    fn feed(&mut self, bytes: &[u8], out: &mut Vec<ParseEvent>);
}

/// 根据协议构造解析器。
pub fn build_parser(protocol: Protocol) -> Box<dyn Parser> {
    match protocol {
        Protocol::Nmea => Box::new(nmea::NmeaParser::new()),
        Protocol::Custom => Box::new(custom::CustomParser::new()),
        Protocol::Auto => Box::new(AutoParser::new()),
    }
}

/// 自动分流解析器：文本行交给 NMEA，二进制同步头交给自定义解析器。
pub struct AutoParser {
    nmea: nmea::NmeaParser,
    custom: custom::CustomParser,
}

impl AutoParser {
    pub fn new() -> Self {
        Self {
            nmea: nmea::NmeaParser::new(),
            custom: custom::CustomParser::new(),
        }
    }
}

impl Default for AutoParser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser for AutoParser {
    fn feed(&mut self, bytes: &[u8], out: &mut Vec<ParseEvent>) {
        // 两个解析器都内置了自己的同步逻辑，可并行喂入同一字节流：
        // NMEA 仅识别以 '$'/'!' 起始、以 CRLF 结束的文本行；
        // 自定义解析器仅识别 0xAA 0x55 同步头的二进制帧；
        // 彼此忽略对方的字节，互不干扰。
        self.nmea.feed(bytes, out);
        self.custom.feed(bytes, out);
    }
}
