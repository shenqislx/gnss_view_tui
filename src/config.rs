//! 命令行参数与运行配置。

use std::path::{Path, PathBuf};

use clap::{Parser, ValueEnum};
use serde::Deserialize;

/// 数据来源协议。
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Protocol {
    /// 标准 NMEA-0183 文本协议。
    Nmea,
    /// 自定义二进制帧协议（示例实现，见 `parser::custom`）。
    Custom,
    /// 同时尝试 NMEA 与自定义二进制（按字节流自动分流）。
    Auto,
}

/// 串口校验位。
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Parity {
    None,
    Odd,
    Even,
}

impl From<Parity> for serialport::Parity {
    fn from(p: Parity) -> Self {
        match p {
            Parity::None => serialport::Parity::None,
            Parity::Odd => serialport::Parity::Odd,
            Parity::Even => serialport::Parity::Even,
        }
    }
}

/// 运行模式：实时串口、回放、内置演示。
#[derive(Clone, Debug)]
pub enum Source {
    /// 实时读取串口。
    Serial,
    /// 回放此前落盘的原始数据文件。
    Replay(PathBuf),
    /// 内置模拟数据，无需硬件即可体验界面。
    Demo,
}

/// GNSS 监控 TUI。
#[derive(Parser, Debug)]
#[command(
    name = "gnss_view_tui",
    version,
    about = "GNSS（NMEA + 私有协议）长期监控 TUI：实时可视化 + 原始数据落盘",
    long_about = None
)]
pub struct Cli {
    /// 串口设备路径，例如 /dev/ttyUSB0 或 COM3。
    #[arg(short, long)]
    pub port: Option<String>,

    /// 波特率（支持高速率，例如 921600 / 3000000）。
    #[arg(short, long, default_value_t = 115200)]
    pub baud: u32,

    /// 数据位（5/6/7/8）。
    #[arg(long, default_value_t = 8)]
    pub data_bits: u8,

    /// 校验位。
    #[arg(long, value_enum, default_value_t = Parity::None)]
    pub parity: Parity,

    /// 停止位（1/2）。
    #[arg(long, default_value_t = 1)]
    pub stop_bits: u8,

    /// 解析协议。
    #[arg(long, value_enum, default_value_t = Protocol::Nmea)]
    pub protocol: Protocol,

    /// 原始数据落盘目录。
    #[arg(short, long, default_value = "./records")]
    pub output: PathBuf,

    /// 关闭原始数据落盘（默认开启）。
    #[arg(long, default_value_t = false)]
    pub no_record: bool,

    /// 滑动窗口折线图的历元数量（10~300）。
    #[arg(long, default_value_t = 60)]
    pub window: usize,

    /// 回放指定的原始数据文件，而非读取串口。
    #[arg(long)]
    pub replay: Option<PathBuf>,

    /// 演示模式：使用内置模拟数据，无需硬件。
    #[arg(long, default_value_t = false)]
    pub demo: bool,

    /// 配置文件路径（JSON）。用于指定地面真值（Ground Truth）等。
    /// 默认读取当前目录下的 `config.json`（不存在则忽略）。
    #[arg(long, default_value = "config.json")]
    pub config: PathBuf,
}

/// 配置文件（JSON）顶层结构。
#[derive(Debug, Default, Deserialize)]
pub struct ConfigFile {
    /// 地面真值文件路径（相对配置文件所在目录或工作目录）。
    pub ground_truth_path: Option<PathBuf>,
    /// NTRIP 播发源配置路径（相对配置文件所在目录或工作目录）。
    pub ntrip_path: Option<PathBuf>,
}

/// 地面真值（Ground Truth）配置：固定参考点的经纬高。
///
/// 支持两种来源格式：
/// - JSON：`{"valid": true, "latitude": .., "longitude": .., "altitude": ..}`；
/// - 纯文本（如 `LLA.txt`）：三行分别为 纬度 / 经度 / 海拔（默认 valid=true）。
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct GroundTruthConfig {
    /// 该真值是否有效；为 false 时程序将回退为“首个定位点”。
    #[serde(default = "default_true")]
    pub valid: bool,
    pub latitude: f64,
    pub longitude: f64,
    #[serde(default)]
    pub altitude: f64,
}

fn default_true() -> bool {
    true
}

fn default_gga_interval() -> u64 {
    10
}

fn default_ntrip_port() -> u16 {
    2101
}

/// GGA 上报来源。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GgaMode {
    /// 上报串口解析到的最新 GGA 语句。
    #[default]
    Serial,
    /// 用 [`FixedPosition`] 合成 GGA。
    Fixed,
}

/// 手动输入的固定坐标，用于在无实时定位时向 NTRIP 服务器上报位置。
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct FixedPosition {
    pub latitude: f64,
    pub longitude: f64,
    #[serde(default)]
    pub altitude: f64,
}

/// NTRIP caster 连接与 GGA 上报参数。
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct NtripConfig {
    pub host: String,
    #[serde(default = "default_ntrip_port")]
    pub port: u16,
    pub mountpoint: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    /// 上报间隔（秒）。`0` 表示不上报。默认 10。
    #[serde(default = "default_gga_interval")]
    pub gga_interval_secs: u64,
    #[serde(default)]
    pub gga_mode: GgaMode,
    pub fixed_position: Option<FixedPosition>,
}

impl NtripConfig {
    /// 不含密码的一行摘要，用于启动日志。
    pub fn summary(&self) -> String {
        let who = if self.username.is_empty() {
            String::new()
        } else {
            format!("{}@", self.username)
        };
        let gga = if self.gga_interval_secs == 0 {
            "不上报 GGA".to_string()
        } else {
            let mode = match self.gga_mode {
                GgaMode::Serial => "串口 GGA",
                GgaMode::Fixed => "固定位置 GGA",
            };
            format!("{mode} 每 {}s", self.gga_interval_secs)
        };
        format!(
            "NTRIP {who}{}:{}/{}，{gga}",
            self.host, self.port, self.mountpoint
        )
    }
}

/// 解析并校验 NTRIP JSON。
fn parse_ntrip_json(text: &str) -> anyhow::Result<NtripConfig> {
    let mut cfg: NtripConfig = serde_json::from_str(text)?;
    cfg.host = cfg.host.trim().to_string();
    cfg.mountpoint = cfg
        .mountpoint
        .trim()
        .trim_start_matches('/')
        .to_string();
    cfg.username = cfg.username.trim().to_string();
    if cfg.host.is_empty() {
        anyhow::bail!("host 为空");
    }
    if cfg.host.chars().any(|c| c.is_control() || c.is_whitespace() || c == '/') {
        anyhow::bail!("host 无效");
    }
    if cfg.port == 0 {
        anyhow::bail!("port 无效");
    }
    if cfg.mountpoint.is_empty() {
        anyhow::bail!("mountpoint 为空");
    }
    if cfg
        .mountpoint
        .chars()
        .any(|c| c.is_control() || c.is_whitespace())
    {
        anyhow::bail!("mountpoint 无效");
    }
    if cfg.username.chars().any(|c| c == '\r' || c == '\n')
        || cfg.password.chars().any(|c| c == '\r' || c == '\n')
    {
        anyhow::bail!("用户名或密码含换行");
    }
    if let Some(pos) = cfg.fixed_position {
        if !pos.latitude.is_finite() || !pos.longitude.is_finite() || !pos.altitude.is_finite() {
            anyhow::bail!("fixed_position 含非有限数值");
        }
        if !(-90.0..=90.0).contains(&pos.latitude) || !(-180.0..=180.0).contains(&pos.longitude) {
            anyhow::bail!("fixed_position 经纬度超出范围");
        }
    }
    if cfg.gga_mode == GgaMode::Fixed && cfg.fixed_position.is_none() {
        anyhow::bail!("gga_mode 为 fixed 时必须提供 fixed_position");
    }
    Ok(cfg)
}

/// 相对路径优先相对配置文件所在目录解析；该路径不存在时退回原始相对路径（相对工作目录）。
fn resolve_config_path(config_file: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        return path;
    }
    if let Some(parent) = config_file.parent().filter(|p| !p.as_os_str().is_empty()) {
        let joined = parent.join(&path);
        if joined.exists() {
            return joined;
        }
    }
    path
}

/// 解析地面真值文件：根据扩展名选择 JSON 或纯文本（三行 LLA）。
fn parse_ground_truth(path: &Path) -> anyhow::Result<GroundTruthConfig> {
    let text = std::fs::read_to_string(path)?;
    let is_json = path
        .extension()
        .map(|e| e.eq_ignore_ascii_case("json"))
        .unwrap_or(false);
    if is_json {
        Ok(serde_json::from_str(&text)?)
    } else {
        // 纯文本：前三行为 纬度 / 经度 / 海拔（忽略空行与 # 注释）。
        let nums: Vec<f64> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .filter_map(|l| l.parse::<f64>().ok())
            .collect();
        if nums.len() < 2 {
            anyhow::bail!("地面真值文本至少需要 纬度/经度 两行数值");
        }
        Ok(GroundTruthConfig {
            valid: true,
            latitude: nums[0],
            longitude: nums[1],
            altitude: nums.get(2).copied().unwrap_or(0.0),
        })
    }
}

impl Cli {
    /// 由命令行参数推导运行来源。
    pub fn source(&self) -> Source {
        if self.demo {
            Source::Demo
        } else if let Some(path) = &self.replay {
            Source::Replay(path.clone())
        } else {
            Source::Serial
        }
    }

    /// 归一化滑动窗口大小到 [10, 300]。
    pub fn window_clamped(&self) -> usize {
        self.window.clamp(10, 300)
    }

    /// 加载配置文件与地面真值。
    ///
    /// 返回 (有效地面真值, 提示信息)。地面真值缺失/无效时返回 `None`，
    /// 程序将回退为“首个定位点”。提示信息用于写入控制台，便于排查。
    pub fn load_ground_truth(&self) -> (Option<GroundTruthConfig>, Vec<String>) {
        let mut logs = Vec::new();

        if !self.config.exists() {
            // 默认路径不存在时静默忽略；显式指定但缺失才提示。
            if self.config != Path::new("config.json") {
                logs.push(format!("配置文件不存在: {}", self.config.display()));
            }
            return (None, logs);
        }

        let cfg: ConfigFile = match std::fs::read_to_string(&self.config)
            .map_err(anyhow::Error::from)
            .and_then(|t| Ok(serde_json::from_str::<ConfigFile>(&t)?))
        {
            Ok(c) => c,
            Err(e) => {
                logs.push(format!("配置文件解析失败 ({}): {e}", self.config.display()));
                return (None, logs);
            }
        };

        let Some(gt_path) = cfg.ground_truth_path else {
            logs.push("配置未指定 ground_truth_path，将以首个定位点为参考".to_string());
            return (None, logs);
        };

        let resolved = resolve_config_path(&self.config, gt_path);

        match parse_ground_truth(&resolved) {
            Ok(gt) if gt.valid => {
                logs.push(format!(
                    "地面真值已加载: {:.7}, {:.7}, {:.3} m ({})",
                    gt.latitude,
                    gt.longitude,
                    gt.altitude,
                    resolved.display()
                ));
                (Some(gt), logs)
            }
            Ok(_) => {
                logs.push("地面真值标记为无效 (valid=false)，将以首个定位点为参考".to_string());
                (None, logs)
            }
            Err(e) => {
                logs.push(format!("地面真值加载失败 ({}): {e}", resolved.display()));
                (None, logs)
            }
        }
    }

    /// 加载 NTRIP 配置。
    ///
    /// 根配置缺失或未写 `ntrip_path` 时返回 `None`（功能关闭，不记日志，不阻断启动）。
    /// 路径已指定但文件缺失或内容不合法时，同样返回 `None`，并把原因写入提示。
    /// 相对路径优先相对配置文件所在目录解析。
    pub fn load_ntrip_config(&self) -> (Option<NtripConfig>, Vec<String>) {
        let mut logs = Vec::new();
        if !self.config.exists() {
            return (None, logs);
        }

        let cfg: ConfigFile = match std::fs::read_to_string(&self.config)
            .map_err(anyhow::Error::from)
            .and_then(|t| Ok(serde_json::from_str::<ConfigFile>(&t)?))
        {
            Ok(c) => c,
            Err(_) => return (None, logs),
        };

        let Some(ntrip_path) = cfg.ntrip_path else {
            return (None, logs);
        };

        let resolved = resolve_config_path(&self.config, ntrip_path);
        if !resolved.exists() {
            logs.push(format!(
                "NTRIP 配置不存在: {}（可复制 data/ntrip.example.json）",
                resolved.display()
            ));
            return (None, logs);
        }

        match std::fs::read_to_string(&resolved)
            .map_err(anyhow::Error::from)
            .and_then(|t| parse_ntrip_json(&t))
        {
            Ok(ntrip) => (Some(ntrip), logs),
            Err(e) => {
                logs.push(format!("NTRIP 配置解析失败 ({}): {e}", resolved.display()));
                (None, logs)
            }
        }
    }

    /// 友好的来源描述，用于状态栏展示。
    pub fn source_label(&self) -> String {
        match self.source() {
            Source::Demo => "演示模式".to_string(),
            Source::Replay(p) => format!("回放: {}", p.display()),
            Source::Serial => format!(
                "{}@{}",
                self.port.as_deref().unwrap_or("<未指定串口>"),
                self.baud
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn example_ntrip_config_parses() {
        let cfg = parse_ntrip_json(include_str!("../data/ntrip.example.json")).unwrap();
        assert_eq!(cfg.host, "rtk.ntrip.qxwz.com");
        assert_eq!(cfg.port, 8002);
        assert_eq!(cfg.mountpoint, "AUTO");
        assert_eq!(cfg.gga_mode, GgaMode::Serial);
        assert_eq!(cfg.gga_interval_secs, 10);
        let pos = cfg.fixed_position.expect("示例应带固定坐标");
        assert!((pos.latitude - 31.160996718).abs() < 1e-9);
        assert!(cfg.summary().contains("your_username@"));
        assert!(!cfg.summary().contains("your_password"));
    }

    #[test]
    fn fixed_mode_requires_position() {
        let text = r#"{"host":"127.0.0.1","mountpoint":"M","gga_mode":"fixed"}"#;
        let err = parse_ntrip_json(text).unwrap_err();
        assert!(err.to_string().contains("fixed_position"), "{err}");
    }

    #[test]
    fn rejects_header_injection_in_mountpoint() {
        let text = "{\"host\":\"127.0.0.1\",\"mountpoint\":\"A\\r\\nX\",\"username\":\"u\",\"password\":\"p\"}";
        assert!(parse_ntrip_json(text).is_err());
    }

    #[test]
    fn loads_ntrip_relative_to_config_dir() {
        let dir = std::env::temp_dir().join(format!(
            "gnss_ntrip_cfg_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        std::fs::write(
            dir.join("config.json"),
            r#"{"ntrip_path":"data/ntrip.json"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("data/ntrip.json"),
            include_str!("../data/ntrip.example.json"),
        )
        .unwrap();
        let config = dir.join("config.json");
        let cli = Cli::try_parse_from([
            "gnss_view_tui",
            "--demo",
            "--config",
            config.to_str().unwrap(),
        ])
        .unwrap();
        let (cfg, logs) = cli.load_ntrip_config();
        assert!(cfg.is_some(), "{logs:?}");
        assert!(logs.is_empty(), "{logs:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn omitted_ntrip_path_stays_silent() {
        let dir = std::env::temp_dir().join(format!(
            "gnss_ntrip_omit_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("config.json");
        std::fs::write(&config, r#"{"ground_truth_path":"data/ground_truth.json"}"#).unwrap();
        let cli = Cli::try_parse_from([
            "gnss_view_tui",
            "--demo",
            "--config",
            config.to_str().unwrap(),
        ])
        .unwrap();
        let (cfg, logs) = cli.load_ntrip_config();
        assert!(cfg.is_none());
        assert!(logs.is_empty(), "{logs:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
