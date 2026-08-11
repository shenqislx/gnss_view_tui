//! 原始数据落盘：保证串口原始字节完整、不丢、可回放。
//!
//! 产物（位于输出目录）：
//! - `<session>.raw`：逐字节原样写入的串口数据流（用于回放）。
//!
//! 接收机时间内嵌于 NMEA 报文中；回放按固定节奏喂入即可复现解析结果。

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub struct Recorder {
    raw: BufWriter<File>,
    raw_path: PathBuf,
    since_flush: usize,
}

impl Recorder {
    /// 在 `dir` 下新建一组会话文件。
    pub fn new(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir)
            .with_context(|| format!("创建落盘目录失败: {}", dir.display()))?;
        let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let raw_path = dir.join(format!("gnss_{stamp}.raw"));
        let raw = BufWriter::with_capacity(
            64 * 1024,
            File::create(&raw_path).with_context(|| format!("创建 {} 失败", raw_path.display()))?,
        );
        Ok(Self {
            raw,
            raw_path,
            since_flush: 0,
        })
    }

    /// 写入一个原始数据块。
    pub fn write(&mut self, data: &[u8]) -> Result<()> {
        self.raw.write_all(data)?;
        self.since_flush += data.len();
        // 按量刷盘，兼顾性能与“不丢数”。
        if self.since_flush >= 16 * 1024 {
            self.flush()?;
        }
        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        self.raw.flush()?;
        self.since_flush = 0;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.raw_path
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}
