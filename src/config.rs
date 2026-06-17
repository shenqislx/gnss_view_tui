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

        // 相对路径优先相对配置文件所在目录解析。
        let resolved = if gt_path.is_absolute() {
            gt_path.clone()
        } else if let Some(parent) = self.config.parent().filter(|p| !p.as_os_str().is_empty()) {
            let p = parent.join(&gt_path);
            if p.exists() { p } else { gt_path.clone() }
        } else {
            gt_path.clone()
        };

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
