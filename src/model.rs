//! 共享状态模型：界面线程与解析线程通过 `Arc<Mutex<AppState>>` 交互。

use std::collections::{BTreeMap, VecDeque};

use chrono::{DateTime, Local};

/// GNSS 星座系统。
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum GnssSystem {
    Gps,
    Glonass,
    Galileo,
    BeiDou,
    Qzss,
    Navic,
    Unknown,
}

impl GnssSystem {
    /// 由 NMEA talker ID（GP/GL/GA/GB/GQ/GI/GN）推断星座。
    pub fn from_talker(talker: &str) -> Self {
        match talker {
            "GP" => GnssSystem::Gps,
            "GL" => GnssSystem::Glonass,
            "GA" => GnssSystem::Galileo,
            "GB" | "BD" => GnssSystem::BeiDou,
            "GQ" => GnssSystem::Qzss,
            "GI" => GnssSystem::Navic,
            _ => GnssSystem::Unknown,
        }
    }

    /// 简短标识，用于 UI 标签。
    pub fn tag(&self) -> &'static str {
        match self {
            GnssSystem::Gps => "G",
            GnssSystem::Glonass => "R",
            GnssSystem::Galileo => "E",
            GnssSystem::BeiDou => "B",
            GnssSystem::Qzss => "Q",
            GnssSystem::Navic => "I",
            GnssSystem::Unknown => "?",
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            GnssSystem::Gps => "GPS",
            GnssSystem::Glonass => "GLONASS",
            GnssSystem::Galileo => "Galileo",
            GnssSystem::BeiDou => "BeiDou",
            GnssSystem::Qzss => "QZSS",
            GnssSystem::Navic => "NavIC",
            GnssSystem::Unknown => "其他",
        }
    }
}

/// CN0 图的星座筛选。
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SatFilter {
    /// 显示全部星座。
    All,
    /// 仅显示某个星座。
    Only(GnssSystem),
}

impl SatFilter {
    pub fn label(&self) -> String {
        match self {
            SatFilter::All => "全部".to_string(),
            SatFilter::Only(s) => s.name().to_string(),
        }
    }

    pub fn matches(&self, sat: &Satellite) -> bool {
        match self {
            SatFilter::All => true,
            SatFilter::Only(sys) => sat.system == Some(*sys),
        }
    }
}

/// 天空图每颗星保留的星轨点数（最近变化的若干点，性能优先）。
pub const SAT_TRAIL_MAX: usize = 8;

/// 单颗卫星观测信息。
#[derive(Clone, Debug, Default)]
pub struct Satellite {
    pub system: Option<GnssSystem>,
    pub prn: u16,
    pub elevation: Option<u16>,
    pub azimuth: Option<u16>,
    pub cn0: Option<u16>,
    pub used_in_fix: bool,
    /// 最近更新时刻，用于老化清理。
    pub last_seen: Option<DateTime<Local>>,
    /// 天空图星轨：最近若干个 (仰角, 方位) 采样，最新在尾部。
    /// 仅在 (仰角, 方位) 发生变化时追加，控制点数以保证性能。
    pub trail: VecDeque<(u16, u16)>,
}

impl Satellite {
    pub fn key(&self) -> SatKey {
        SatKey {
            system: self.system.unwrap_or(GnssSystem::Unknown),
            prn: self.prn,
        }
    }
}

/// 卫星唯一键：星座 + PRN。
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SatKey {
    pub system: GnssSystem,
    pub prn: u16,
}

/// 定位解算结果（PVT）。
#[derive(Clone, Debug, Default)]
pub struct Pvt {
    /// 接收机 UTC 时间（来自 GGA/RMC）。
    pub utc: Option<String>,
    /// 日期（来自 RMC）。
    pub date: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    /// 海拔（海平面/大地水准面以上正高，即“海拔高 / geoid height”）。
    pub altitude: Option<f64>,
    /// 大地水准面差距（geoidal separation）。
    pub geoid_sep: Option<f64>,
    pub speed_kn: Option<f64>,
    pub course_deg: Option<f64>,
    pub fix_quality: Option<u8>,
    pub fix_type: Option<u8>,
    pub sats_used: Option<u16>,
    pub valid: bool,
}

impl Pvt {
    /// 椭球高 = 海拔高（geoid height）+ 大地水准面差距。
    pub fn ellipsoid_height(&self) -> Option<f64> {
        let alt = self.altitude?;
        Some(alt + self.geoid_sep.unwrap_or(0.0))
    }

    /// 大地水准面差距是否非零（决定是否分开展示两种高度）。
    pub fn has_separation(&self) -> bool {
        self.geoid_sep.map(|s| s.abs() > 1e-6).unwrap_or(false)
    }

    /// 速度（m/s），由航速（knot）换算：1 kn = 0.514444 m/s。
    pub fn speed_mps(&self) -> Option<f64> {
        self.speed_kn.map(|kn| kn * 0.514_444)
    }

    pub fn fix_quality_text(&self) -> &'static str {
        match self.fix_quality {
            Some(0) => "无定位",
            Some(1) => "单点 (GPS)",
            Some(2) => "差分 (DGPS)",
            Some(3) => "PPS",
            Some(4) => "RTK 固定",
            Some(5) => "RTK 浮点",
            Some(6) => "估算",
            Some(_) => "其他",
            None => "—",
        }
    }

    pub fn fix_type_text(&self) -> &'static str {
        match self.fix_type {
            Some(1) => "无定位",
            Some(2) => "2D",
            Some(3) => "3D",
            _ => "—",
        }
    }
}

/// 精度因子（DOP）。
#[derive(Clone, Debug, Default)]
pub struct Dop {
    pub pdop: Option<f64>,
    pub hdop: Option<f64>,
    pub vdop: Option<f64>,
}

/// 运行统计。
#[derive(Clone, Debug)]
pub struct Stats {
    pub started: DateTime<Local>,
    pub bytes_total: u64,
    pub sentences_ok: u64,
    pub parse_errors: u64,
    pub last_rx: Option<DateTime<Local>>,
    pub recording_path: Option<String>,
    pub source_label: String,
    pub connected: bool,
}

impl Stats {
    fn new(source_label: String) -> Self {
        Self {
            started: Local::now(),
            bytes_total: 0,
            sentences_ok: 0,
            parse_errors: 0,
            last_rx: None,
            recording_path: None,
            source_label,
            connected: false,
        }
    }
}

/// 控制台一行日志。
#[derive(Clone, Debug)]
pub struct LogLine {
    pub time: DateTime<Local>,
    pub kind: LogKind,
    pub text: String,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LogKind {
    Rx,
    Tx,
    Info,
    Error,
}

const CONSOLE_CAPACITY: usize = 2000;

/// 多模块滑动曲线（CPU 耗时剖析）。每个模块一条序列，单位毫秒。
#[derive(Clone, Debug, Default)]
pub struct Profile {
    /// 模块名（保持首次出现顺序）。
    pub names: Vec<String>,
    /// 与 `names` 对齐的各模块时间序列（最新在尾部，单位 ms）。
    pub series: Vec<VecDeque<f64>>,
    /// 滑动窗口容量（历元数）。
    pub capacity: usize,
}

#[allow(clippy::len_without_is_empty)]
impl Profile {
    pub fn new(capacity: usize) -> Self {
        Self {
            names: Vec::new(),
            series: Vec::new(),
            capacity,
        }
    }

    /// 当前序列长度（历元数）。
    pub fn len(&self) -> usize {
        self.series.first().map(|s| s.len()).unwrap_or(0)
    }

    /// 追加一个历元的采样：未出现的已知模块补 0，新模块补齐历史为 0。
    pub fn push(&mut self, samples: &[(String, f64)]) {
        let cur_len = self.len();
        for (name, _) in samples {
            if !self.names.iter().any(|n| n == name) {
                self.names.push(name.clone());
                let mut dq = VecDeque::with_capacity(self.capacity.max(1));
                for _ in 0..cur_len {
                    dq.push_back(0.0);
                }
                self.series.push(dq);
            }
        }
        for (i, name) in self.names.iter().enumerate() {
            let v = samples
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| *v)
                .unwrap_or(0.0);
            let s = &mut self.series[i];
            s.push_back(v);
            while s.len() > self.capacity {
                s.pop_front();
            }
        }
    }

    /// 调整滑动窗口容量。
    pub fn set_capacity(&mut self, cap: usize) {
        self.capacity = cap;
        for s in &mut self.series {
            while s.len() > cap {
                s.pop_front();
            }
        }
    }

}

/// 地面真值（Ground Truth）：用于计算定位误差的固定参考点。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroundTruth {
    pub lat: f64,
    pub lon: f64,
    pub alt: f64,
    /// true：来自有效配置；false：回退为首个收到的定位点。
    pub valid: bool,
}

/// 位置偏差时间序列（ENU，单位米）。相对地面真值的 东/北/天 三分量。
#[derive(Clone, Debug, Default)]
pub struct PosBias {
    /// 东向偏差序列（最新在尾部，单位 m）。
    pub east: VecDeque<f64>,
    /// 北向偏差序列。
    pub north: VecDeque<f64>,
    /// 天向（高度）偏差序列。
    pub up: VecDeque<f64>,
    /// 滑动窗口容量（历元数，约等于秒数）。
    pub capacity: usize,
}

#[allow(clippy::len_without_is_empty)]
impl PosBias {
    pub fn new(capacity: usize) -> Self {
        Self {
            east: VecDeque::new(),
            north: VecDeque::new(),
            up: VecDeque::new(),
            capacity,
        }
    }

    /// 当前序列长度（历元数）。
    pub fn len(&self) -> usize {
        self.east.len()
    }

    /// 追加一个历元的 ENU 偏差采样。
    pub fn push(&mut self, e: f64, n: f64, u: f64) {
        for (dq, v) in [
            (&mut self.east, e),
            (&mut self.north, n),
            (&mut self.up, u),
        ] {
            dq.push_back(v);
            while dq.len() > self.capacity {
                dq.pop_front();
            }
        }
    }

    /// 调整滑动窗口容量。
    pub fn set_capacity(&mut self, cap: usize) {
        self.capacity = cap;
        for dq in [&mut self.east, &mut self.north, &mut self.up] {
            while dq.len() > cap {
                dq.pop_front();
            }
        }
    }

    /// 三分量中绝对值最大者（用于 Y 轴自适应）。
    pub fn max_abs(&self) -> f64 {
        self.east
            .iter()
            .chain(self.north.iter())
            .chain(self.up.iter())
            .fold(0.0_f64, |m, v| m.max(v.abs()))
    }
}

/// 全局共享状态。
pub struct AppState {
    pub satellites: BTreeMap<SatKey, Satellite>,
    pub pvt: Pvt,
    pub dop: Dop,
    pub stats: Stats,
    pub console: VecDeque<LogLine>,
    /// CPU 耗时剖析多模块曲线。
    pub profile: Profile,
    /// 滑动窗口容量（历元数），由 +/- 快捷键调整。
    pub curve_capacity: usize,
    /// CN0 图星座筛选。
    pub sat_filter: SatFilter,
    /// CN0 图当前页（卫星过多时分页）。
    pub cn0_page: usize,
    /// CN0 图最大页号，由渲染层每帧回写，供翻页时夹取。
    pub cn0_max_page: std::cell::Cell<usize>,
    /// 轨迹图：最近若干个定位点 (纬度, 经度)，最新在尾部。
    pub track: VecDeque<(f64, f64)>,
    /// 轨迹图视图中心 (纬度, 经度)：起点居中，越界后才重新居中。
    pub traj_center: Option<(f64, f64)>,
    /// 轨迹图半幅（米）：中心到边框的距离，即比例尺。
    pub traj_span_m: f64,
    /// 地面真值（参考点）：None 表示尚未确定（将以首个定位点回退）。
    pub ground_truth: Option<GroundTruth>,
    /// 位置偏差（ENU，米）时间序列。
    pub pos_bias: PosBias,
}

/// 轨迹图保留的最大定位点数。
pub const TRACK_MAX: usize = 600;

/// 轨迹图初始半幅（米）：50m × 50m 网格框。
const TRAJ_DEFAULT_SPAN_M: f64 = 25.0;

impl AppState {
    pub fn new(source_label: String, curve_capacity: usize) -> Self {
        let cap = curve_capacity.clamp(10, 300);
        Self {
            satellites: BTreeMap::new(),
            pvt: Pvt::default(),
            dop: Dop::default(),
            stats: Stats::new(source_label),
            console: VecDeque::with_capacity(CONSOLE_CAPACITY),
            profile: Profile::new(cap),
            curve_capacity: cap,
            sat_filter: SatFilter::All,
            cn0_page: 0,
            cn0_max_page: std::cell::Cell::new(0),
            track: VecDeque::with_capacity(TRACK_MAX),
            traj_center: None,
            traj_span_m: TRAJ_DEFAULT_SPAN_M,
            ground_truth: None,
            pos_bias: PosBias::new(cap),
        }
    }

    /// 当前有 CN0 数据的星座列表（去重、按枚举顺序排序）。
    pub fn available_systems(&self) -> Vec<GnssSystem> {
        let mut v: Vec<GnssSystem> = self
            .satellites
            .values()
            .filter(|s| s.cn0.is_some())
            .filter_map(|s| s.system)
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// 在「全部 → 各星座 → 全部」之间循环切换筛选，并回到第 1 页。
    pub fn cycle_filter(&mut self) {
        let systems = self.available_systems();
        self.sat_filter = match self.sat_filter {
            SatFilter::All => systems
                .first()
                .map(|s| SatFilter::Only(*s))
                .unwrap_or(SatFilter::All),
            SatFilter::Only(cur) => match systems.iter().position(|x| *x == cur) {
                Some(pos) => systems
                    .get(pos + 1)
                    .map(|n| SatFilter::Only(*n))
                    .unwrap_or(SatFilter::All),
                None => SatFilter::All,
            },
        };
        self.cn0_page = 0;
    }

    /// 经筛选后、含 CN0 的卫星（保持 PRN 稳定顺序）。
    pub fn filtered_satellites(&self) -> Vec<&Satellite> {
        self.satellites
            .values()
            .filter(|s| s.cn0.is_some() && self.sat_filter.matches(s))
            .collect()
    }

    /// 上一页 / 下一页（下一页夹取到渲染层回写的最大页）。
    pub fn cn0_prev_page(&mut self) {
        self.cn0_page = self.cn0_page.saturating_sub(1);
    }

    pub fn cn0_next_page(&mut self) {
        let max = self.cn0_max_page.get();
        self.cn0_page = (self.cn0_page + 1).min(max);
    }

    /// 追加控制台日志，自动截断容量。
    pub fn push_log(&mut self, kind: LogKind, text: impl Into<String>) {
        if self.console.len() >= CONSOLE_CAPACITY {
            self.console.pop_front();
        }
        self.console.push_back(LogLine {
            time: Local::now(),
            kind,
            text: text.into(),
        });
    }

    /// 追加一个历元的 CPU 耗时剖析采样（单位 ms）。
    pub fn push_profile(&mut self, samples: &[(String, f64)]) {
        self.profile.push(samples);
    }

    /// 追加一个定位点到轨迹缓冲（保留最近 [`TRACK_MAX`] 个）。
    ///
    /// 视图策略：首点设为中心并居中；之后中心**不跟随**，
    /// 仅当新点超出当前边框时才重新以该点为中心，并按需放大比例尺。
    pub fn push_track_point(&mut self, lat: f64, lon: f64) {
        if !lat.is_finite() || !lon.is_finite() {
            return;
        }
        if self.track.len() >= TRACK_MAX {
            self.track.pop_front();
        }
        self.track.push_back((lat, lon));

        match self.traj_center {
            None => {
                self.traj_center = Some((lat, lon));
                self.traj_span_m = TRAJ_DEFAULT_SPAN_M;
            }
            Some((lat0, lon0)) => {
                let (dx, dy) = enu_offset_m(lat0, lon0, lat, lon);
                if dx.abs() > self.traj_span_m || dy.abs() > self.traj_span_m {
                    // 越界：重新居中到该点，并按需放大到容纳此次位移的“整齐”比例尺。
                    let need = dx.abs().max(dy.abs()).max(self.traj_span_m);
                    self.traj_center = Some((lat, lon));
                    self.traj_span_m = nice_span_m(need);
                }
            }
        }
    }

    /// 调整滑动窗口容量（CPU Load 曲线与位置偏差时间序列统一窗口）。
    pub fn set_curve_capacity(&mut self, cap: usize) {
        self.curve_capacity = cap.clamp(10, 300);
        self.profile.set_capacity(self.curve_capacity);
        self.pos_bias.set_capacity(self.curve_capacity);
    }

    /// 依据当前定位点与地面真值，追加一个 ENU 位置偏差采样（单位米）。
    ///
    /// 若地面真值尚未确定（无有效配置），则以该首个定位点作为参考
    /// （`valid=false`），后续采样均相对此点。
    pub fn push_pos_bias(&mut self, lat: f64, lon: f64, alt: Option<f64>) {
        if !lat.is_finite() || !lon.is_finite() {
            return;
        }
        let gt = match self.ground_truth {
            Some(gt) => gt,
            None => {
                let gt = GroundTruth {
                    lat,
                    lon,
                    alt: alt.unwrap_or(0.0),
                    valid: false,
                };
                self.ground_truth = Some(gt);
                gt
            }
        };
        let (e, n) = enu_offset_m(gt.lat, gt.lon, lat, lon);
        let u = alt.map(|a| a - gt.alt).unwrap_or(0.0);
        self.pos_bias.push(e, n, u);
    }

    /// 更新/插入一颗卫星的观测信息（合并字段）。
    pub fn upsert_satellite(&mut self, obs: Satellite) {
        let key = obs.key();
        let entry = self.satellites.entry(key).or_default();
        entry.system = obs.system.or(entry.system);
        entry.prn = obs.prn;
        if obs.elevation.is_some() {
            entry.elevation = obs.elevation;
        }
        if obs.azimuth.is_some() {
            entry.azimuth = obs.azimuth;
        }
        // CN0 即使为 None（失锁）也要更新，以反映真实状态。
        entry.cn0 = obs.cn0;
        entry.last_seen = obs.last_seen;

        // 维护星轨：仅当仰角/方位均已知且相对上一点发生变化时追加。
        if let (Some(el), Some(az)) = (entry.elevation, entry.azimuth) {
            let changed = entry.trail.back().map(|&(e, a)| e != el || a != az).unwrap_or(true);
            if changed {
                entry.trail.push_back((el, az));
                while entry.trail.len() > SAT_TRAIL_MAX {
                    entry.trail.pop_front();
                }
            }
        }
    }

    /// 清除超过 `max_age_secs` 未更新的卫星。
    pub fn prune_satellites(&mut self, max_age_secs: i64) {
        let now = Local::now();
        self.satellites.retain(|_, s| match s.last_seen {
            Some(t) => (now - t).num_seconds() <= max_age_secs,
            None => true,
        });
    }

    /// 当前被用于定位的卫星数量。
    pub fn used_count(&self) -> usize {
        self.satellites.values().filter(|s| s.used_in_fix).count()
    }

    /// 当前可见（有 CN0）的卫星数量。
    pub fn visible_count(&self) -> usize {
        self.satellites.values().filter(|s| s.cn0.is_some()).count()
    }
}

/// 以 `(lat0, lon0)` 为原点，用等距矩形近似计算 `(lat, lon)` 的东向/北向偏移（米）。
pub fn enu_offset_m(lat0: f64, lon0: f64, lat: f64, lon: f64) -> (f64, f64) {
    const M_PER_DEG_LAT: f64 = 110_540.0;
    const M_PER_DEG_LON_EQ: f64 = 111_320.0;
    let dy = (lat - lat0) * M_PER_DEG_LAT;
    let dx = (lon - lon0) * M_PER_DEG_LON_EQ * lat0.to_radians().cos();
    (dx, dy)
}

/// 把距离向上取整到“整齐”的 1/2/5 ×10ⁿ 比例尺（米）。
pub fn nice_span_m(m: f64) -> f64 {
    let m = m.max(1.0);
    let exp = m.log10().floor();
    let base = 10f64.powf(exp);
    for f in [1.0, 2.0, 5.0] {
        if base * f >= m {
            return base * f;
        }
    }
    base * 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nice_span_rounds_up_to_ladder() {
        assert_eq!(nice_span_m(1.0), 1.0);
        assert_eq!(nice_span_m(3.0), 5.0);
        assert_eq!(nice_span_m(7.0), 10.0);
        assert_eq!(nice_span_m(25.0), 50.0);
        assert_eq!(nice_span_m(120.0), 200.0);
    }

    #[test]
    fn first_track_point_centers_view() {
        let mut s = AppState::new("t".into(), 60);
        assert!(s.traj_center.is_none());
        s.push_track_point(31.23, 121.47);
        assert_eq!(s.traj_center, Some((31.23, 121.47)));
        assert_eq!(s.track.len(), 1);
        // 起点居中：不立即跟随后续小幅移动。
        s.push_track_point(31.230001, 121.470001);
        assert_eq!(s.traj_center, Some((31.23, 121.47)));
    }

    #[test]
    fn track_recenters_only_when_out_of_box() {
        let mut s = AppState::new("t".into(), 60);
        s.push_track_point(31.23, 121.47);
        let span0 = s.traj_span_m;
        // 远跳：> 半幅 → 重新居中且放大比例尺。
        let far_lat = 31.23 + 1.0; // ~110 km 北
        s.push_track_point(far_lat, 121.47);
        assert_eq!(s.traj_center, Some((far_lat, 121.47)));
        assert!(s.traj_span_m > span0);
    }

    #[test]
    fn track_buffer_caps_at_max() {
        let mut s = AppState::new("t".into(), 60);
        for i in 0..(TRACK_MAX + 50) {
            s.push_track_point(31.23 + i as f64 * 1e-7, 121.47);
        }
        assert_eq!(s.track.len(), TRACK_MAX);
    }

    #[test]
    fn pos_bias_falls_back_to_first_point_when_no_gt() {
        let mut s = AppState::new("t".into(), 60);
        assert!(s.ground_truth.is_none());
        // 首点：作为参考，偏差应为 0；并记录为 valid=false。
        s.push_pos_bias(31.23, 121.47, Some(10.0));
        let gt = s.ground_truth.expect("应以首点回退为 GT");
        assert!(!gt.valid, "回退 GT 应标记为 invalid");
        assert!(s.pos_bias.east.back().unwrap().abs() < 1e-6);
        assert!(s.pos_bias.north.back().unwrap().abs() < 1e-6);
        assert!(s.pos_bias.up.back().unwrap().abs() < 1e-6);
    }

    #[test]
    fn pos_bias_uses_configured_gt_and_enu_signs() {
        let mut s = AppState::new("t".into(), 60);
        s.ground_truth = Some(GroundTruth {
            lat: 31.23,
            lon: 121.47,
            alt: 10.0,
            valid: true,
        });
        // 略偏北、偏东、偏高的点。
        s.push_pos_bias(31.2301, 121.4701, Some(12.5));
        let e = *s.pos_bias.east.back().unwrap();
        let n = *s.pos_bias.north.back().unwrap();
        let u = *s.pos_bias.up.back().unwrap();
        assert!(e > 0.0, "经度增大 → 东向为正");
        assert!(n > 0.0, "纬度增大 → 北向为正");
        assert!((u - 2.5).abs() < 1e-6, "高度差应为 +2.5 m");
    }

    #[test]
    fn pos_bias_window_synced_with_curve_capacity() {
        let mut s = AppState::new("t".into(), 60);
        for i in 0..100 {
            s.push_pos_bias(31.23 + i as f64 * 1e-7, 121.47, Some(10.0));
        }
        assert_eq!(s.pos_bias.len(), 60);
        s.set_curve_capacity(20);
        assert_eq!(s.pos_bias.len(), 20, "缩小窗口应同步裁剪位置偏差序列");
        assert_eq!(s.pos_bias.capacity, 20);
    }

    #[test]
    fn sat_trail_keeps_recent_changed_points() {
        let mut s = AppState::new("t".into(), 60);
        let mk = |el: u16, az: u16| Satellite {
            system: Some(GnssSystem::Gps),
            prn: 1,
            elevation: Some(el),
            azimuth: Some(az),
            cn0: Some(40),
            used_in_fix: false,
            last_seen: Some(Local::now()),
            trail: VecDeque::new(),
        };
        // 重复同一点不增长星轨。
        s.upsert_satellite(mk(45, 100));
        s.upsert_satellite(mk(45, 100));
        let key = SatKey { system: GnssSystem::Gps, prn: 1 };
        assert_eq!(s.satellites[&key].trail.len(), 1);
        // 变化则追加，并受 SAT_TRAIL_MAX 限制。
        for k in 0..(SAT_TRAIL_MAX + 5) {
            s.upsert_satellite(mk(45, 100 + k as u16 + 1));
        }
        assert_eq!(s.satellites[&key].trail.len(), SAT_TRAIL_MAX);
    }
}
