//! GNSS（NMEA + 私有协议）长期监控 TUI —— 开源默认可执行入口。
//!
//! 仅负责解析命令行并调用库入口 [`gnss_view_tui::run`]，不注入任何扩展。

use anyhow::Result;
use clap::Parser as _;

fn main() -> Result<()> {
    let cli = gnss_view_tui::Cli::parse();
    gnss_view_tui::run(cli, None, None)
}
