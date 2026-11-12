//! 原始数据落盘：保证串口原始字节完整、不丢、可回放。
//!
//! 产物（位于输出目录）：
//! - `<session>.raw`：逐字节原样写入的串口数据流（用于回放）。
//! - `<session>.rtcm`：NTRIP 转发的 RTCM 差分数据（见 `ntrip`）。
//!
//! 接收机时间内嵌于 NMEA 报文中；回放按固定节奏喂入即可复现解析结果。

use std::fs::{self, File};
use std::io::{BufWriter, ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub struct Recorder {
    raw: BufWriter<File>,
    raw_path: PathBuf,
    since_flush: usize,
}

impl Recorder {
    /// 在 `dir` 下新建一组串口原始数据会话文件（`.raw`）。
    pub fn new(dir: &Path) -> Result<Self> {
        Self::create(dir, "raw")
    }

    /// 在 `dir` 下新建一组 RTCM 差分数据会话文件（`.rtcm`）。
    pub fn new_rtcm(dir: &Path) -> Result<Self> {
        Self::create(dir, "rtcm")
    }

    fn create(dir: &Path, ext: &str) -> Result<Self> {
        fs::create_dir_all(dir)
            .with_context(|| format!("创建落盘目录失败: {}", dir.display()))?;
        let now = chrono::Local::now();
        let stamp = now.format("%Y%m%d_%H%M%S");
        let millis = now.timestamp_subsec_millis();
        for index in 0..1000u32 {
            let name = if index == 0 {
                format!("gnss_{stamp}_{millis:03}.{ext}")
            } else {
                format!("gnss_{stamp}_{millis:03}_{index}.{ext}")
            };
            let raw_path = dir.join(name);
            match File::options().write(true).create_new(true).open(&raw_path) {
                Ok(file) => {
                    return Ok(Self {
                        raw: BufWriter::with_capacity(64 * 1024, file),
                        raw_path,
                        since_flush: 0,
                    });
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    anyhow::bail!("创建 {} 失败: {e}", raw_path.display());
                }
            }
        }
        anyhow::bail!("落盘文件名冲突过多: {}", dir.display())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtcm_file_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "gnss_rtcm_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut rec = Recorder::new_rtcm(&dir).unwrap();
        assert_eq!(rec.path().extension().and_then(|e| e.to_str()), Some("rtcm"));
        let mut other = Recorder::new_rtcm(&dir).unwrap();
        assert_ne!(rec.path(), other.path(), "同一秒内的第二个文件不能覆盖第一个");
        rec.write(&[0xD3, 0x00, 0x01]).unwrap();
        other.write(&[0x02]).unwrap();
        rec.flush().unwrap();
        other.flush().unwrap();
        assert_eq!(std::fs::read(rec.path()).unwrap(), vec![0xD3, 0x00, 0x01]);
        assert_eq!(std::fs::read(other.path()).unwrap(), vec![0x02]);
        drop(rec);
        drop(other);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
