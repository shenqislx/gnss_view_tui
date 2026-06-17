//! 界面应用：事件循环 + 本地 UI 状态。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::ext::{Screen, UiExt};
use crate::model::{AppState, LogKind};
use crate::source::TxCommand;
use crate::ui;

/// DTR 复位脉冲拉低时长（毫秒）。
const DTR_RESET_LOW_MS: u64 = 100;

/// 交互模式。
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
}

/// 前台覆盖面板：默认无，热键打开后独占前台 UI。
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Overlay {
    None,
    /// 卫星天空图（极坐标 仰角/方位）。
    Skyplot,
    /// 定位轨迹图。
    Trajectory,
    /// 位置偏差 ENU 时间序列。
    PosBias,
}

/// 发送命令时附加的换行符。
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum LineEnding {
    Crlf,
    Lf,
    None,
}

impl LineEnding {
    pub fn label(&self) -> &'static str {
        match self {
            LineEnding::Crlf => "CRLF",
            LineEnding::Lf => "LF",
            LineEnding::None => "无",
        }
    }
    fn bytes(&self) -> &'static [u8] {
        match self {
            LineEnding::Crlf => b"\r\n",
            LineEnding::Lf => b"\n",
            LineEnding::None => b"",
        }
    }
    fn next(self) -> Self {
        match self {
            LineEnding::Crlf => LineEnding::Lf,
            LineEnding::Lf => LineEnding::None,
            LineEnding::None => LineEnding::Crlf,
        }
    }
}

pub struct App {
    state: Arc<Mutex<AppState>>,
    cmd_tx: Sender<TxCommand>,
    running: Arc<AtomicBool>,
    pub mode: Mode,
    pub input: String,
    pub console_scroll: usize,
    pub show_help: bool,
    pub line_ending: LineEnding,
    /// 当前前台覆盖面板（天空图 / 轨迹图 / 无）。
    pub overlay: Overlay,
    /// 顶层屏幕：主视图 / 扩展视图。
    pub screen: Screen,
    /// 注入的 UI 扩展（如私有测试模式）；为 None 时无扩展屏。
    pub ext: Option<Box<dyn UiExt>>,
}

impl App {
    pub fn new(
        state: Arc<Mutex<AppState>>,
        cmd_tx: Sender<TxCommand>,
        running: Arc<AtomicBool>,
    ) -> Self {
        Self {
            state,
            cmd_tx,
            running,
            mode: Mode::Normal,
            input: String::new(),
            console_scroll: 0,
            show_help: false,
            line_ending: LineEnding::Crlf,
            overlay: Overlay::None,
            screen: Screen::Main,
            ext: None,
        }
    }

    /// 注入 UI 扩展（如私有测试模式）。开源默认 bin 不调用此方法。
    pub fn with_ext(mut self, ext: Option<Box<dyn UiExt>>) -> Self {
        self.ext = ext;
        self
    }

    /// 主事件循环。
    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while self.running.load(Ordering::Relaxed) {
            // 渲染（短暂持锁）。
            {
                let state = self.state.lock().unwrap();
                terminal.draw(|f| ui::render(f, self, &state))?;
            }

            // 扩展心跳：每轮给扩展一个墙钟节拍（驱动其内部状态机）。
            if let Some(ext) = &mut self.ext {
                ext.tick(&self.cmd_tx);
            }

            // 处理输入事件。
            if event::poll(Duration::from_millis(100))?
                && let Event::Key(key) = event::read()?
                    && key.kind == KeyEventKind::Press {
                        self.on_key(key);
                    }
        }
        Ok(())
    }

    fn on_key(&mut self, key: KeyEvent) {
        // Ctrl+C 始终退出。
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit();
            return;
        }

        // 顶层屏幕切换：仅当注入了扩展时 Tab 才在主屏/扩展屏间切换。
        if key.code == KeyCode::Tab && self.ext.is_some() {
            self.screen = match self.screen {
                Screen::Main => Screen::Ext,
                Screen::Ext => Screen::Main,
            };
            return;
        }

        // 扩展屏：按键全部交给扩展处理（Ctrl+C / Tab 已在上方拦截）。
        if self.screen == Screen::Ext {
            if let Some(ext) = &mut self.ext {
                ext.on_key(key, &self.cmd_tx);
            }
            return;
        }

        if self.show_help {
            self.show_help = false;
            return;
        }

        // 覆盖面板独占前台：仅响应关闭/切换，其余按键忽略。
        if self.overlay != Overlay::None {
            self.on_key_overlay(key);
            return;
        }

        match self.mode {
            Mode::Normal => self.on_key_normal(key),
            Mode::Insert => self.on_key_insert(key),
        }
    }

    /// 覆盖面板模式下的按键：Esc 关闭，s/t/p 在面板间切换或关闭，q 退出。
    fn on_key_overlay(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.overlay = Overlay::None,
            KeyCode::Char('s') => self.toggle_overlay(Overlay::Skyplot),
            KeyCode::Char('t') => self.toggle_overlay(Overlay::Trajectory),
            KeyCode::Char('p') => self.toggle_overlay(Overlay::PosBias),
            // 位置偏差面板支持在其内调整窗口（与 CPU Load 同步）。
            KeyCode::Char('+') | KeyCode::Char('=') => self.adjust_window(10),
            KeyCode::Char('-') | KeyCode::Char('_') => self.adjust_window(-10),
            _ => {}
        }
    }

    /// 在指定面板与“关闭”之间切换。
    fn toggle_overlay(&mut self, target: Overlay) {
        self.overlay = if self.overlay == target {
            Overlay::None
        } else {
            target
        };
    }

    fn on_key_normal(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.quit(),
            KeyCode::Char('i') | KeyCode::Enter => self.mode = Mode::Insert,
            KeyCode::Char('s') => self.overlay = Overlay::Skyplot,
            KeyCode::Char('t') => self.overlay = Overlay::Trajectory,
            KeyCode::Char('p') => self.overlay = Overlay::PosBias,
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char('c') => {
                if let Ok(mut s) = self.state.lock() {
                    s.console.clear();
                }
                self.console_scroll = 0;
            }
            KeyCode::Char('l') => self.line_ending = self.line_ending.next(),
            KeyCode::Char('d') => self.dtr_reset(),
            KeyCode::Char('+') | KeyCode::Char('=') => self.adjust_window(10),
            KeyCode::Char('-') | KeyCode::Char('_') => self.adjust_window(-10),
            KeyCode::Char('f') => {
                if let Ok(mut s) = self.state.lock() {
                    s.cycle_filter();
                }
            }
            KeyCode::Left => {
                if let Ok(mut s) = self.state.lock() {
                    s.cn0_prev_page();
                }
            }
            KeyCode::Right => {
                if let Ok(mut s) = self.state.lock() {
                    s.cn0_next_page();
                }
            }
            KeyCode::Up => self.console_scroll += 1,
            KeyCode::Down => self.console_scroll = self.console_scroll.saturating_sub(1),
            KeyCode::PageUp => self.console_scroll += 8,
            KeyCode::PageDown => self.console_scroll = self.console_scroll.saturating_sub(8),
            KeyCode::End => self.console_scroll = 0,
            _ => {}
        }
    }

    fn on_key_insert(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => self.send_command(),
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(c) => self.input.push(c),
            _ => {}
        }
    }

    fn adjust_window(&mut self, delta: i32) {
        if let Ok(mut s) = self.state.lock() {
            let cur = s.curve_capacity as i32;
            let next = (cur + delta).clamp(10, 300) as usize;
            s.set_curve_capacity(next);
        }
    }

    fn send_command(&mut self) {
        let text = self.input.trim_end_matches(['\r', '\n']).to_string();
        if text.is_empty() {
            return;
        }
        let mut bytes = text.clone().into_bytes();
        bytes.extend_from_slice(self.line_ending.bytes());

        match self.cmd_tx.send(TxCommand::Bytes(bytes)) {
            Ok(_) => {
                if let Ok(mut s) = self.state.lock() {
                    s.push_log(LogKind::Tx, text);
                }
            }
            Err(_) => {
                if let Ok(mut s) = self.state.lock() {
                    s.push_log(LogKind::Error, "发送失败：串口通道已关闭".to_string());
                }
            }
        }
        self.input.clear();
    }

    /// 发送 DTR 复位脉冲（类似 picocom 的 pulse DTR），用于复位下位机。
    fn dtr_reset(&mut self) {
        match self.cmd_tx.send(TxCommand::DtrReset {
            low_ms: DTR_RESET_LOW_MS,
        }) {
            Ok(_) => {
                if let Ok(mut s) = self.state.lock() {
                    s.push_log(LogKind::Tx, format!("DTR 复位脉冲 ({DTR_RESET_LOW_MS}ms)"));
                }
            }
            Err(_) => {
                if let Ok(mut s) = self.state.lock() {
                    s.push_log(LogKind::Error, "DTR 复位失败：串口通道已关闭".to_string());
                }
            }
        }
    }

    fn quit(&mut self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ext::EventObserver;
    use crate::model::AppState;
    use crate::parser::ParseEvent;
    use chrono::Local;
    use ratatui::Frame;
    use ratatui::crossterm::event::KeyModifiers;
    use ratatui::layout::Rect;
    use std::sync::mpsc;

    /// 最小扩展实现：仅记录收到的按键，验证接缝可注入、可路由。
    #[derive(Default)]
    struct DummyExt {
        keys: Arc<Mutex<Vec<KeyCode>>>,
        ticks: Arc<Mutex<usize>>,
    }

    impl UiExt for DummyExt {
        fn on_key(&mut self, key: KeyEvent, _tx: &Sender<TxCommand>) {
            self.keys.lock().unwrap().push(key.code);
        }
        fn tick(&mut self, _tx: &Sender<TxCommand>) {
            *self.ticks.lock().unwrap() += 1;
        }
        fn render(&self, _frame: &mut Frame, _area: Rect, _state: &AppState) {}
        fn header_status(&self) -> Option<String> {
            Some("dummy".into())
        }
        fn title(&self) -> &str {
            "Dummy"
        }
    }

    /// 最小观察者实现：仅计数，验证管线接缝可注入。
    struct DummyObserver {
        count: Arc<Mutex<usize>>,
    }

    impl EventObserver for DummyObserver {
        fn observe(&self, _ev: &ParseEvent, _pc_time: chrono::DateTime<Local>, _state: &AppState) {
            *self.count.lock().unwrap() += 1;
        }
    }

    fn make_app(ext: Option<Box<dyn UiExt>>) -> App {
        let state = Arc::new(Mutex::new(AppState::new("test".into(), 60)));
        let (cmd_tx, _rx) = mpsc::channel();
        App::new(state, cmd_tx, Arc::new(AtomicBool::new(true))).with_ext(ext)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn tab_toggles_screen_only_when_ext_present() {
        // 无扩展：Tab 不切屏，仍停留主屏。
        let mut app = make_app(None);
        assert_eq!(app.screen, Screen::Main);
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.screen, Screen::Main);

        // 有扩展：Tab 在主屏/扩展屏间切换。
        let mut app = make_app(Some(Box::new(DummyExt::default())));
        assert_eq!(app.screen, Screen::Main);
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.screen, Screen::Ext);
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.screen, Screen::Main);
    }

    #[test]
    fn keys_route_to_ext_on_ext_screen() {
        let keys = Arc::new(Mutex::new(Vec::new()));
        let ext = DummyExt {
            keys: keys.clone(),
            ..Default::default()
        };
        let mut app = make_app(Some(Box::new(ext)));

        // 主屏时按键不进扩展。
        app.on_key(key(KeyCode::Char('x')));
        assert!(keys.lock().unwrap().is_empty());

        // 切到扩展屏后，按键全部交给扩展。
        app.on_key(key(KeyCode::Tab));
        app.on_key(key(KeyCode::Char('a')));
        app.on_key(key(KeyCode::Char('b')));
        assert_eq!(*keys.lock().unwrap(), vec![KeyCode::Char('a'), KeyCode::Char('b')]);
    }

    #[test]
    fn tick_seam_is_callable_with_tx() {
        // 验证 tick 接缝可注入并能通过 tx 通道发命令（心跳驱动状态机的基础）。
        let ticks = Arc::new(Mutex::new(0usize));
        let ext = DummyExt {
            keys: Arc::new(Mutex::new(Vec::new())),
            ticks: ticks.clone(),
        };
        let mut app = make_app(Some(Box::new(ext)));
        if let Some(e) = &mut app.ext {
            e.tick(&app.cmd_tx);
            e.tick(&app.cmd_tx);
        }
        assert_eq!(*ticks.lock().unwrap(), 2);
    }

    #[test]
    fn observer_seam_is_callable() {
        let count = Arc::new(Mutex::new(0usize));
        let obs = DummyObserver { count: count.clone() };
        let state = AppState::new("test".into(), 60);
        obs.observe(&ParseEvent::EpochTick, Local::now(), &state);
        assert_eq!(*count.lock().unwrap(), 1);
    }
}
