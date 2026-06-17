//! 扩展接缝：供外部功能（私有测试模式等）注入。core 自身不含任何实现。
//!
//! 设计意图（领域中立，开源无碍）：
//! - [`Screen`]：顶层屏幕切换（主视图 / 扩展视图），与主视图内部的 `Overlay` 平级之上。
//! - [`UiExt`]：UI 线程扩展，扩展视图激活时独占整屏（按键 + 渲染 + 顶栏状态）。
//! - [`EventObserver`]：管线线程事件观察者，在每个解析事件落地后被回调，
//!   可只读 [`AppState`] 与 PC 接收时间（TTFF 等"首个有效定位时刻"在此捕获）。
//!
//! 线程归属：`EventObserver` 被搬入管线线程，`UiExt` 留在 UI 线程的 `App` 中；
//! 二者通过实现方自己的共享状态（如 `Arc<Mutex<...>>`）通信，与 `App`/`AppState`
//! 的拆分同构。锁顺序铁律：先 `AppState`、后实现方的私有状态。

use std::sync::mpsc::Sender;

use chrono::{DateTime, Local};
use ratatui::Frame;
use ratatui::crossterm::event::KeyEvent;
use ratatui::layout::Rect;

use crate::model::AppState;
use crate::parser::ParseEvent;
use crate::source::TxCommand;

/// 顶层屏幕：主视图 / 扩展视图。
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum Screen {
    /// 现有主仪表盘视图。
    #[default]
    Main,
    /// 注入的扩展视图（独占整屏）。
    Ext,
}

/// UI 线程扩展：扩展视图激活时独占整屏。
pub trait UiExt: Send {
    /// 扩展视图激活时的按键处理；可通过 `tx` 向下位机发送命令。
    fn on_key(&mut self, key: KeyEvent, tx: &Sender<TxCommand>);
    /// 渲染扩展视图（独占给定区域）。
    fn render(&self, frame: &mut Frame, area: Rect, state: &AppState);
    /// 顶栏状态片段（如"冷启动 3/10"）；返回 `None` 则不显示。
    fn header_status(&self) -> Option<String> {
        None
    }
    /// 扩展视图标题（用于切换提示）。
    fn title(&self) -> &str {
        "扩展"
    }
}

/// 管线线程事件观察者：每个解析事件落地后被调用（只读 [`AppState`] + PC 接收时间）。
pub trait EventObserver: Send {
    fn observe(&self, ev: &ParseEvent, pc_time: DateTime<Local>, state: &AppState);
}
