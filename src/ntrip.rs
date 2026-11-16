//! 后台 NTRIP 客户端：连接播发源，把 RTCM 交给 UART1 写线程并落盘。
//!
//! 默认关闭。界面按 `n` 翻转 [`NtripSwitch`] 后，本线程才发起 TCP 连接。
//! UART0 只提供 NMEA/GGA，断开不会停掉 RTCM 转发。网络中断自动重连。

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Utc};

use crate::config::{GgaMode, NtripConfig};
use crate::model::{AppState, LastGga, LogKind, NtripStatus};
use crate::recorder::Recorder;

/// 用户开关。后台线程轮询它，默认关闭。
#[derive(Clone)]
pub struct NtripSwitch {
    enabled: Arc<AtomicBool>,
}

impl NtripSwitch {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(enabled)),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn set(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }
}

/// 可供 UI 扩展克隆的 NTRIP 控制与状态视图。开关与后台线程共用，状态读自 `AppState`。
#[derive(Clone)]
pub struct NtripControl {
    switch: NtripSwitch,
    state: Arc<Mutex<AppState>>,
}

/// 某一时刻的 NTRIP 健康快照。不包含账号或密码。
#[derive(Clone, Debug, Default)]
pub struct NtripSnapshot {
    pub enabled: bool,
    pub connected: bool,
    pub status: NtripStatus,
    pub bytes_total: u64,
    pub bps: f64,
    pub gga_sent: u64,
    pub detail: String,
}

impl NtripControl {
    pub fn new(switch: NtripSwitch, state: Arc<Mutex<AppState>>) -> Self {
        Self { switch, state }
    }

    pub fn set_enabled(&self, on: bool) {
        self.switch.set(on);
    }

    pub fn is_enabled(&self) -> bool {
        self.switch.is_enabled()
    }

    pub fn is_connected(&self) -> bool {
        self.snapshot().connected
    }

    pub fn snapshot(&self) -> NtripSnapshot {
        let Ok(state) = self.state.lock() else {
            return NtripSnapshot {
                enabled: self.is_enabled(),
                ..NtripSnapshot::default()
            };
        };
        NtripSnapshot {
            enabled: self.is_enabled(),
            connected: state.ntrip.status == NtripStatus::Connected,
            status: state.ntrip.status,
            bytes_total: state.ntrip.bytes_total,
            bps: state.ntrip.bps,
            gga_sent: state.ntrip.gga_sent,
            detail: state.ntrip.detail.clone(),
        }
    }
}

/// 后台线程句柄。丢弃时通知线程退出并等待其结束。
pub struct NtripHandle {
    switch: NtripSwitch,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl NtripHandle {
    pub fn spawn(
        cfg: NtripConfig,
        output_dir: PathBuf,
        no_record: bool,
        rtcm_tx: SyncSender<Vec<u8>>,
        state: Arc<Mutex<AppState>>,
        running: Arc<AtomicBool>,
    ) -> Self {
        let switch = NtripSwitch::new(false);
        let stop = Arc::new(AtomicBool::new(true));
        let enabled = Arc::clone(&switch.enabled);
        let stop_flag = Arc::clone(&stop);
        let join = thread::spawn(move || {
            Worker {
                cfg,
                rtcm_tx,
                state,
                running,
                enabled,
                stop: stop_flag,
                record: RtcmLog::new(output_dir, no_record),
                rtcm_drop_logged: false,
                linked: false,
            }
            .run();
        });
        Self {
            switch,
            stop,
            join: Some(join),
        }
    }

    pub fn switch(&self) -> NtripSwitch {
        self.switch.clone()
    }
}

impl Drop for NtripHandle {
    fn drop(&mut self) {
        self.switch.set(false);
        self.stop.store(false, Ordering::Relaxed);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

struct Worker {
    cfg: NtripConfig,
    rtcm_tx: SyncSender<Vec<u8>>,
    state: Arc<Mutex<AppState>>,
    running: Arc<AtomicBool>,
    enabled: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    record: RtcmLog,
    /// 本次会话是否已经因队列满丢过 RTCM，避免刷屏。
    rtcm_drop_logged: bool,
    /// 本次会话握手是否已经成功。成功后的掉线按短间隔重连。
    linked: bool,
}

enum SessionStop {
    Halt,
    Failed(String),
}

impl Worker {
    fn alive(&self) -> bool {
        self.running.load(Ordering::Relaxed) && self.stop.load(Ordering::Relaxed)
    }

    fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    fn run(mut self) {
        let mut armed = false;
        let mut backoff = Duration::from_secs(2);
        while self.alive() {
            if !self.enabled() {
                if armed {
                    self.record.close();
                    mark(&self.state, NtripStatus::Off, true, "");
                    armed = false;
                    backoff = Duration::from_secs(2);
                }
                if self.park(Duration::from_millis(150)) {
                    break;
                }
                continue;
            }
            if !armed {
                reset_counters(&self.state);
                armed = true;
            }
            mark(&self.state, NtripStatus::Connecting, true, "");
            console_log(
                &self.state,
                LogKind::Info,
                format!(
                    "正在连接 NTRIP {}:{}/{}",
                    self.cfg.host, self.cfg.port, self.cfg.mountpoint
                ),
            );
            match self.session() {
                SessionStop::Halt => {}
                SessionStop::Failed(err) => {
                    let wait = if self.linked {
                        backoff = Duration::from_secs(2);
                        Duration::from_secs(2)
                    } else {
                        let wait = retry_after(&err, backoff);
                        if !is_config_rejection(&err) {
                            backoff = (backoff * 2).min(Duration::from_secs(30));
                        }
                        wait
                    };
                    mark(&self.state, NtripStatus::Error, true, &err);
                    console_log(
                        &self.state,
                        LogKind::Error,
                        format!("NTRIP：{err}，{}s 后重试", wait.as_secs()),
                    );
                    let _ = self.park_while_enabled(wait);
                }
            }
        }
        self.record.close();
        mark(&self.state, NtripStatus::Off, true, "");
    }

    fn session(&mut self) -> SessionStop {
        self.linked = false;
        self.rtcm_drop_logged = false;
        let mut stream = match self.dial() {
            Ok(stream) => stream,
            Err(stop) => return stop,
        };
        if let Err(e) = stream.set_nodelay(true) {
            return SessionStop::Failed(format!("设置 TCP_NODELAY 失败: {e}"));
        }
        if let Err(e) = stream.set_read_timeout(Some(Duration::from_millis(200))) {
            return SessionStop::Failed(format!("设置读超时失败: {e}"));
        }
        if let Err(e) = stream.set_write_timeout(Some(Duration::from_secs(5))) {
            return SessionStop::Failed(format!("设置写超时失败: {e}"));
        }
        if let Err(e) = stream.write_all(build_request(&self.cfg).as_bytes()) {
            return SessionStop::Failed(format!("发送请求失败: {e}"));
        }

        let pending = match read_handshake(&mut stream, &self.running, &self.stop, &self.enabled) {
            Ok(body) => body,
            Err(stop) => return stop,
        };

        mark(&self.state, NtripStatus::Connected, false, "");
        self.linked = true;
        console_log(&self.state, LogKind::Info, self.connected_message());

        let mut rate = Throughput::default();
        if let Err(stop) = self.downlink(&mut rate, &pending) {
            return stop;
        }

        let mut next_gga = Instant::now();
        let mut gga_notice = None;
        let mut buf = [0u8; 8192];
        loop {
            if !self.alive() || !self.enabled() {
                return SessionStop::Halt;
            }
            if let Err(stop) = self.uplink_gga(&mut stream, &mut next_gga, &mut gga_notice) {
                return stop;
            }
            match stream.read(&mut buf) {
                Ok(0) => return SessionStop::Failed("服务器关闭连接".into()),
                Ok(n) => {
                    if let Err(stop) = self.downlink(&mut rate, &buf[..n]) {
                        return stop;
                    }
                }
                Err(e) if is_timeout(&e) => {
                    rate.observe(Instant::now(), 0);
                    publish_rate(&self.state, rate.bps());
                }
                Err(e) => return SessionStop::Failed(format!("接收失败: {e}")),
            }
        }
    }

    fn dial(&self) -> Result<TcpStream, SessionStop> {
        if !self.alive() || !self.enabled() {
            return Err(SessionStop::Halt);
        }
        let addr = format!("{}:{}", self.cfg.host, self.cfg.port);
        let sockets = match addr.to_socket_addrs() {
            Ok(sockets) => sockets,
            Err(e) => return Err(SessionStop::Failed(format!("解析地址失败: {e}"))),
        };
        let mut last = None;
        let mut tried = false;
        for sock in sockets {
            tried = true;
            if !self.alive() || !self.enabled() {
                return Err(SessionStop::Halt);
            }
            match TcpStream::connect_timeout(&sock, Duration::from_secs(5)) {
                Ok(stream) => return Ok(stream),
                Err(e) => last = Some(e),
            }
        }
        Err(SessionStop::Failed(if tried {
            match last {
                Some(e) => format!("连接 {addr} 失败: {e}"),
                None => format!("连接 {addr} 失败"),
            }
        } else {
            format!("连接 {addr} 失败: 无可用地址")
        }))
    }

    fn uplink_gga(
        &self,
        stream: &mut TcpStream,
        next_gga: &mut Instant,
        notice: &mut Option<&'static str>,
    ) -> Result<(), SessionStop> {
        if self.cfg.gga_interval_secs == 0 {
            return Ok(());
        }
        let now = Instant::now();
        if now < *next_gga {
            return Ok(());
        }
        let sentence = match self.cfg.gga_mode {
            GgaMode::Fixed => {
                let Some(pos) = self.cfg.fixed_position else {
                    *next_gga = now + Duration::from_secs(self.cfg.gga_interval_secs.max(1));
                    return Ok(());
                };
                format_fixed_gga(pos.latitude, pos.longitude, pos.altitude, Utc::now())
            }
            GgaMode::Serial => {
                let max_age = serial_gga_max_age_secs(self.cfg.gga_interval_secs);
                let held = match self.state.lock() {
                    Ok(guard) => {
                        classify_serial_gga(guard.last_gga.as_ref(), Local::now(), max_age)
                    }
                    Err(_) => HeldGga::Missing,
                };
                match held {
                    HeldGga::Ready(sentence) => ensure_crlf(&sentence),
                    HeldGga::Missing => {
                        note_gga(&self.state, notice, "尚无串口 GGA，等待定位后再上报");
                        *next_gga = now + Duration::from_secs(1);
                        return Ok(());
                    }
                    HeldGga::Stale => {
                        note_gga(&self.state, notice, "串口 GGA 已过期，暂停上报");
                        *next_gga = now + Duration::from_secs(1);
                        return Ok(());
                    }
                }
            }
        };
        stream
            .write_all(sentence.as_bytes())
            .map_err(|e| SessionStop::Failed(format!("上报 GGA 失败: {e}")))?;
        *notice = None;
        if let Ok(mut state) = self.state.lock() {
            state.ntrip.gga_sent += 1;
        }
        *next_gga = now + Duration::from_secs(self.cfg.gga_interval_secs.max(1));
        Ok(())
    }

    fn downlink(&mut self, rate: &mut Throughput, data: &[u8]) -> Result<(), SessionStop> {
        if data.is_empty() {
            return Ok(());
        }
        let state = Arc::clone(&self.state);
        self.record.write(&state, data);
        rate.observe(Instant::now(), data.len() as u64);
        publish_downlink(&self.state, data.len() as u64, rate.bps());
        match self.rtcm_tx.try_send(data.to_vec()) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                if !self.rtcm_drop_logged {
                    console_log(
                        &self.state,
                        LogKind::Info,
                        "串口发送队列已满，已丢弃超额 RTCM",
                    );
                    self.rtcm_drop_logged = true;
                }
            }
            Err(TrySendError::Disconnected(_)) => {
                return Err(SessionStop::Failed("串口通道已关闭".into()));
            }
        }
        Ok(())
    }

    fn connected_message(&self) -> String {
        let gga = if self.cfg.gga_interval_secs == 0 {
            "不上报 GGA".to_string()
        } else {
            let mode = match self.cfg.gga_mode {
                GgaMode::Serial => "串口",
                GgaMode::Fixed => "固定位置",
            };
            format!("GGA {}s（{mode}）", self.cfg.gga_interval_secs)
        };
        format!(
            "已连接 NTRIP {}:{}/{}，{gga}",
            self.cfg.host, self.cfg.port, self.cfg.mountpoint
        )
    }

    /// 返回 true 表示线程应当退出。
    fn park(&self, dur: Duration) -> bool {
        let step = Duration::from_millis(50);
        let mut waited = Duration::ZERO;
        while waited < dur {
            if !self.alive() {
                return true;
            }
            thread::sleep(step.min(dur - waited));
            waited += step;
        }
        !self.alive()
    }

    /// 返回 true 表示已关闭或线程应当退出。
    fn park_while_enabled(&self, dur: Duration) -> bool {
        let step = Duration::from_millis(50);
        let mut waited = Duration::ZERO;
        while waited < dur {
            if !self.alive() || !self.enabled() {
                return true;
            }
            thread::sleep(step.min(dur - waited));
            waited += step;
        }
        !self.alive() || !self.enabled()
    }
}

struct RtcmLog {
    dir: PathBuf,
    no_record: bool,
    broken: bool,
    recorder: Option<Recorder>,
    last_flush: Instant,
}

impl RtcmLog {
    fn new(dir: PathBuf, no_record: bool) -> Self {
        Self {
            dir,
            no_record,
            broken: false,
            recorder: None,
            last_flush: Instant::now(),
        }
    }

    fn write(&mut self, state: &Mutex<AppState>, data: &[u8]) {
        if self.no_record || self.broken || data.is_empty() {
            return;
        }
        if self.recorder.is_none() {
            match Recorder::new_rtcm(&self.dir) {
                Ok(rec) => {
                    let path = rec.path().display().to_string();
                    self.recorder = Some(rec);
                    self.last_flush = Instant::now();
                    console_log(state, LogKind::Info, format!("RTCM 落盘: {path}"));
                }
                Err(e) => {
                    self.broken = true;
                    console_log(state, LogKind::Error, format!("RTCM 落盘失败: {e}"));
                    return;
                }
            }
        }
        let Some(rec) = self.recorder.as_mut() else {
            return;
        };
        if let Err(e) = rec.write(data) {
            self.broken = true;
            console_log(state, LogKind::Error, format!("RTCM 写入失败: {e}"));
            return;
        }
        if self.last_flush.elapsed() >= Duration::from_secs(2) {
            if let Err(e) = rec.flush() {
                console_log(state, LogKind::Error, format!("RTCM 刷盘失败: {e}"));
            }
            self.last_flush = Instant::now();
        }
    }

    fn close(&mut self) {
        self.recorder = None;
        self.broken = false;
    }
}

fn console_log(state: &Mutex<AppState>, kind: LogKind, text: impl Into<String>) {
    if let Ok(mut guard) = state.lock() {
        guard.push_log(kind, text);
    }
}

fn note_gga(state: &Mutex<AppState>, notice: &mut Option<&'static str>, msg: &'static str) {
    if *notice == Some(msg) {
        return;
    }
    console_log(state, LogKind::Info, msg);
    *notice = Some(msg);
}

fn serial_gga_max_age_secs(interval_secs: u64) -> i64 {
    interval_secs.max(1).saturating_mul(2).clamp(5, 3600) as i64
}

enum HeldGga {
    Ready(String),
    Missing,
    Stale,
}

fn classify_serial_gga(gga: Option<&LastGga>, now: DateTime<Local>, max_age_secs: i64) -> HeldGga {
    let Some(gga) = gga else {
        return HeldGga::Missing;
    };
    let age = (now - gga.at).num_seconds();
    if age > max_age_secs {
        HeldGga::Stale
    } else {
        HeldGga::Ready(gga.sentence.clone())
    }
}

/// 鉴权或挂载点错误重试也救不回来，固定拉长间隔。其余错误沿用当前退避。
fn is_config_rejection(err: &str) -> bool {
    let upper = err.to_ascii_uppercase();
    upper.contains("401")
        || upper.contains("403")
        || upper.contains("SOURCETABLE")
        || err.contains("挂载点")
}

fn retry_after(err: &str, backoff: Duration) -> Duration {
    if is_config_rejection(err) {
        Duration::from_secs(30)
    } else {
        backoff
    }
}

fn mark(state: &Mutex<AppState>, status: NtripStatus, zero_bps: bool, detail: &str) {
    if let Ok(mut guard) = state.lock() {
        guard.ntrip.status = status;
        if status == NtripStatus::Error {
            guard.ntrip.detail = detail.to_string();
        } else {
            guard.ntrip.detail.clear();
        }
        if zero_bps {
            guard.ntrip.bps = 0.0;
        }
    }
}

fn reset_counters(state: &Mutex<AppState>) {
    if let Ok(mut guard) = state.lock() {
        guard.ntrip.status = NtripStatus::Connecting;
        guard.ntrip.bytes_total = 0;
        guard.ntrip.bps = 0.0;
        guard.ntrip.gga_sent = 0;
        guard.ntrip.detail.clear();
    }
}

fn publish_downlink(state: &Mutex<AppState>, bytes: u64, bps: f64) {
    if let Ok(mut guard) = state.lock() {
        guard.ntrip.status = NtripStatus::Connected;
        guard.ntrip.bytes_total += bytes;
        guard.ntrip.bps = bps;
        guard.ntrip.detail.clear();
    }
}

fn publish_rate(state: &Mutex<AppState>, bps: f64) {
    if let Ok(mut guard) = state.lock()
        && guard.ntrip.status == NtripStatus::Connected
    {
        guard.ntrip.bps = bps;
    }
}

fn ensure_crlf(sentence: &str) -> String {
    format!("{}\r\n", sentence.trim_end_matches(['\r', '\n']))
}

/// 用固定经纬高和当前 UTC 合成一条带校验和的 `$GPGGA`。
fn format_fixed_gga(lat: f64, lon: f64, alt: f64, utc: DateTime<Utc>) -> String {
    let (lat_dm, ns) = nmea_lat(lat);
    let (lon_dm, ew) = nmea_lon(lon);
    let tod = utc.format("%H%M%S");
    let body = format!("GPGGA,{tod}.00,{lat_dm},{ns},{lon_dm},{ew},1,08,1.0,{alt:.3},M,0.0,M,,");
    let cs = body.bytes().fold(0u8, |acc, b| acc ^ b);
    format!("${body}*{cs:02X}\r\n")
}

fn nmea_lat(deg: f64) -> (String, char) {
    let hemi = if deg >= 0.0 { 'N' } else { 'S' };
    (nmea_dm(deg.abs(), 2), hemi)
}

fn nmea_lon(deg: f64) -> (String, char) {
    let hemi = if deg >= 0.0 { 'E' } else { 'W' };
    (nmea_dm(deg.abs(), 3), hemi)
}

fn nmea_dm(abs_deg: f64, deg_width: usize) -> String {
    let mut deg = abs_deg.trunc() as u32;
    let mut minutes = (abs_deg - f64::from(deg)) * 60.0;
    if minutes >= 59.99995 {
        deg += 1;
        minutes = 0.0;
    }
    format!("{deg:0width$}{minutes:07.4}", width = deg_width)
}

fn build_request(cfg: &NtripConfig) -> String {
    let mount = cfg.mountpoint.trim().trim_start_matches('/');
    let mut req = format!(
        "GET /{mount} HTTP/1.0\r\n\
         User-Agent: NTRIP gnss_view_tui/0.1\r\n\
         Accept: */*\r\n\
         Host: {}:{}\r\n\
         Connection: close\r\n",
        cfg.host.trim(),
        cfg.port
    );
    if !cfg.username.is_empty() {
        let token = base64_encode(format!("{}:{}", cfg.username, cfg.password).as_bytes());
        req.push_str("Authorization: Basic ");
        req.push_str(&token);
        req.push_str("\r\n");
    }
    req.push_str("\r\n");
    req
}

fn read_handshake(
    stream: &mut TcpStream,
    running: &AtomicBool,
    stop: &AtomicBool,
    enabled: &AtomicBool,
) -> Result<Vec<u8>, SessionStop> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    let start = Instant::now();
    loop {
        if !running.load(Ordering::Relaxed)
            || !stop.load(Ordering::Relaxed)
            || !enabled.load(Ordering::Relaxed)
        {
            return Err(SessionStop::Halt);
        }
        if start.elapsed() > Duration::from_secs(10) {
            return Err(SessionStop::Failed("等待 NTRIP 响应超时".into()));
        }
        match inspect_handshake(&buf) {
            Handshake::Ready { body_start } => {
                let start_at = body_start.min(buf.len());
                return Ok(buf.split_off(start_at));
            }
            Handshake::Rejected(msg) => return Err(SessionStop::Failed(msg)),
            Handshake::NeedMore => {}
        }
        if buf.len() > 8192 {
            return Err(SessionStop::Failed("NTRIP 响应头过长".into()));
        }
        match stream.read(&mut tmp) {
            Ok(0) => return Err(SessionStop::Failed("握手时服务器关闭连接".into())),
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(e) if is_timeout(&e) => {
                if let Some(body_start) = finish_icy_on_idle(&buf) {
                    let start_at = body_start.min(buf.len());
                    return Ok(buf.split_off(start_at));
                }
            }
            Err(e) => return Err(SessionStop::Failed(format!("握手失败: {e}"))),
        }
    }
}

#[derive(Debug)]
enum Handshake {
    NeedMore,
    Rejected(String),
    Ready { body_start: usize },
}

/// 从已缓冲的响应里切出状态行与正文起点。
fn inspect_handshake(buf: &[u8]) -> Handshake {
    let Some((line_at, sep)) = line_break(buf) else {
        if buf.len() > 512 {
            return Handshake::Rejected("响应状态行过长".into());
        }
        return Handshake::NeedMore;
    };
    let status = String::from_utf8_lossy(&buf[..line_at]).trim().to_string();
    let upper = status.to_ascii_uppercase();
    if upper.starts_with("SOURCETABLE") {
        return Handshake::Rejected("服务器返回源列表（SOURCETABLE），请检查挂载点".into());
    }
    if !status_is_stream(&upper) {
        return Handshake::Rejected(status);
    }

    let mut pos = line_at + sep;
    let http = upper.starts_with("HTTP/");
    loop {
        if pos >= buf.len() {
            return Handshake::NeedMore;
        }
        if buf[pos..].starts_with(b"\r\n") {
            return Handshake::Ready {
                body_start: pos + 2,
            };
        }
        if buf[pos..].starts_with(b"\n") {
            return Handshake::Ready {
                body_start: pos + 1,
            };
        }
        match line_break(&buf[pos..]) {
            None => {
                if buf.len() > 8192 {
                    return Handshake::Rejected("NTRIP 响应头过长".into());
                }
                if !http && !could_be_header_prefix(&buf[pos..]) {
                    return Handshake::Ready { body_start: pos };
                }
                return Handshake::NeedMore;
            }
            Some((at, line_sep)) => {
                let line = &buf[pos..pos + at];
                if looks_like_header_line(line) {
                    pos += at + line_sep;
                    continue;
                }
                if http && could_be_header_prefix(line) {
                    return Handshake::NeedMore;
                }
                return Handshake::Ready { body_start: pos };
            }
        }
    }
}

/// ICY 200 且状态行后还没有后续字节时，读超时即可认为头已结束（caster 常在此时等待 GGA）。
fn finish_icy_on_idle(buf: &[u8]) -> Option<usize> {
    let (line_at, sep) = line_break(buf)?;
    let status = String::from_utf8_lossy(&buf[..line_at])
        .trim()
        .to_ascii_uppercase();
    if status.starts_with("ICY") && status_is_stream(&status) && buf.len() == line_at + sep {
        Some(line_at + sep)
    } else {
        None
    }
}

fn status_is_stream(upper: &str) -> bool {
    let mut parts = upper.split_whitespace();
    let proto = parts.next().unwrap_or("");
    let code = parts.next().unwrap_or("");
    code == "200" && (proto.starts_with("ICY") || proto.starts_with("HTTP/"))
}

fn line_break(buf: &[u8]) -> Option<(usize, usize)> {
    if let Some(i) = buf.windows(2).position(|w| w == b"\r\n") {
        Some((i, 2))
    } else {
        buf.iter().position(|&b| b == b'\n').map(|i| (i, 1))
    }
}

fn looks_like_header_line(line: &[u8]) -> bool {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let Some(colon) = line.iter().position(|&b| b == b':') else {
        return false;
    };
    colon > 0
        && colon <= 40
        && line.first().is_some_and(|b| b.is_ascii_alphabetic())
        && line[..colon]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
}

fn could_be_header_prefix(buf: &[u8]) -> bool {
    if buf.is_empty() {
        return true;
    }
    if buf.len() >= 48 || !buf.iter().copied().all(is_headerish) {
        return false;
    }
    if let Some(colon) = buf.iter().position(|&b| b == b':') {
        return colon > 0
            && colon <= 40
            && buf[0].is_ascii_alphabetic()
            && buf[..colon]
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || *b == b'-');
    }
    buf[0].is_ascii_alphabetic()
}

fn is_headerish(b: u8) -> bool {
    b.is_ascii_graphic() || b == b' ' || b == b'\t'
}

fn is_timeout(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    )
}

/// 最近 1 秒的下行字节数。
#[derive(Default)]
struct Throughput {
    buckets: std::collections::VecDeque<(Instant, u64)>,
    sum: u64,
}

impl Throughput {
    fn observe(&mut self, now: Instant, bytes: u64) {
        if bytes > 0 {
            self.buckets.push_back((now, bytes));
            self.sum += bytes;
        }
        while let Some(&(t, n)) = self.buckets.front() {
            if now.saturating_duration_since(t) >= Duration::from_secs(1) {
                self.buckets.pop_front();
                self.sum -= n;
            } else {
                break;
            }
        }
    }

    fn bps(&self) -> f64 {
        self.sum as f64
    }
}

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= data.len() {
        let n = (u32::from(data[i]) << 16) | (u32::from(data[i + 1]) << 8) | u32::from(data[i + 2]);
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 6) & 0x3f) as usize] as char);
        out.push(TABLE[(n & 0x3f) as usize] as char);
        i += 3;
    }
    match data.len() - i {
        1 => {
            let n = u32::from(data[i]) << 16;
            out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
            out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
            out.push('=');
            out.push('=');
        }
        2 => {
            let n = (u32::from(data[i]) << 16) | (u32::from(data[i + 1]) << 8);
            out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
            out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
            out.push(TABLE[((n >> 6) & 0x3f) as usize] as char);
            out.push('=');
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{FixedPosition, GgaMode};
    use std::sync::mpsc;

    fn ready_body(buf: &[u8]) -> Vec<u8> {
        match inspect_handshake(buf) {
            Handshake::Ready { body_start } => buf[body_start..].to_vec(),
            other => panic!("expected Ready, got {other:?} for {buf:?}"),
        }
    }

    #[test]
    fn base64_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"a"), "YQ==");
        assert_eq!(base64_encode(b"ab"), "YWI=");
        assert_eq!(base64_encode(b"user:pass"), "dXNlcjpwYXNz");
    }

    #[test]
    fn request_contains_mount_and_basic_auth() {
        let cfg = NtripConfig {
            host: "127.0.0.1".into(),
            port: 2101,
            mountpoint: "/AUTO".into(),
            username: "user".into(),
            password: "pass".into(),
            gga_interval_secs: 10,
            gga_mode: GgaMode::Serial,
            fixed_position: None,
            rtcm_port: "/dev/ttyUSB1".into(),
            rtcm_baud: 115_200,
        };
        let req = build_request(&cfg);
        assert!(req.starts_with("GET /AUTO HTTP/1.0\r\n"), "{req}");
        assert!(
            req.contains("Authorization: Basic dXNlcjpwYXNz\r\n"),
            "{req}"
        );
        assert!(req.ends_with("\r\n\r\n"));
    }

    #[test]
    fn handshake_splits_icy_http_and_errors() {
        assert_eq!(
            ready_body(b"ICY 200 OK\r\n\xD3\x00\r\n\r\nMORE"),
            b"\xD3\x00\r\n\r\nMORE"
        );
        assert_eq!(ready_body(b"ICY 200 OK\r\n\r\n\xD3"), b"\xD3");
        assert_eq!(
            ready_body(b"ICY 200 OK\r\nServer: NTRIP\r\n\r\n\xD3\x01"),
            b"\xD3\x01"
        );
        assert_eq!(
            ready_body(b"HTTP/1.1 200 OK\r\nContent-Type: gnss/data\r\n\r\n\xD3"),
            b"\xD3"
        );
        assert!(matches!(
            inspect_handshake(b"HTTP/1.0 401 Unauthorized\r\n\r\n"),
            Handshake::Rejected(_)
        ));
        assert!(matches!(
            inspect_handshake(b"SOURCETABLE 200 OK\r\n"),
            Handshake::Rejected(_)
        ));
        assert!(matches!(
            inspect_handshake(b"ICY 200 OK"),
            Handshake::NeedMore
        ));
        assert!(matches!(
            inspect_handshake(b"ICY 200 OK\r\nServer: NTR"),
            Handshake::NeedMore
        ));
        assert_eq!(finish_icy_on_idle(b"ICY 200 OK\r\n"), Some(12));
        assert_eq!(finish_icy_on_idle(b"ICY 200 OK\r\nServer"), None);
        assert!(matches!(
            inspect_handshake(b"HTTP/1.1 200 OK\r\nContent-Type: gnss/data\r\n"),
            Handshake::NeedMore
        ));
    }

    #[test]
    fn fixed_gga_coordinates_and_checksum() {
        let line = format_fixed_gga(31.160996718, 121.54950658, 62.338, Utc::now());
        assert!(
            line.contains(",3109.6598,N,12132.9704,E,1,08,1.0,62.338,M,0.0,M,,"),
            "{line}"
        );
        assert!(line.ends_with("\r\n"));
        let trimmed = line.trim().trim_start_matches('$');
        let (payload, cs) = trimmed.split_once('*').unwrap();
        let calc = payload.bytes().fold(0u8, |acc, b| acc ^ b);
        assert_eq!(cs, format!("{calc:02X}"));

        let south = format_fixed_gga(-31.16, -121.54, 1.0, Utc::now());
        assert!(south.contains(",3109.6000,S,12132.4000,W,"), "{south}");
    }

    #[test]
    fn throughput_window_expires_after_one_second() {
        let mut rate = Throughput::default();
        let t0 = Instant::now();
        rate.observe(t0, 100);
        assert_eq!(rate.bps(), 100.0);
        rate.observe(t0 + Duration::from_millis(1500), 5);
        assert_eq!(rate.bps(), 5.0);
    }

    #[test]
    fn retry_after_slows_auth_and_mount_errors() {
        assert_eq!(
            retry_after("HTTP/1.0 401 Unauthorized", Duration::from_secs(2)),
            Duration::from_secs(30)
        );
        assert_eq!(
            retry_after("HTTP/1.1 403 Forbidden", Duration::from_secs(4)),
            Duration::from_secs(30)
        );
        assert_eq!(
            retry_after("SOURCETABLE 200 OK", Duration::from_secs(2)),
            Duration::from_secs(30)
        );
        assert_eq!(
            retry_after("挂载点不存在", Duration::from_secs(2)),
            Duration::from_secs(30)
        );
        assert_eq!(
            retry_after("连接失败: timed out", Duration::from_secs(4)),
            Duration::from_secs(4)
        );
    }

    #[test]
    fn stale_serial_gga_is_not_ready() {
        let fresh = LastGga {
            sentence: "$GPGGA,a*00".into(),
            at: Local::now(),
        };
        assert!(matches!(
            classify_serial_gga(Some(&fresh), Local::now(), 20),
            HeldGga::Ready(_)
        ));
        let old = LastGga {
            sentence: "$GPGGA,a*00".into(),
            at: Local::now() - chrono::Duration::seconds(30),
        };
        assert!(matches!(
            classify_serial_gga(Some(&old), Local::now(), 20),
            HeldGga::Stale
        ));
        assert!(matches!(
            classify_serial_gga(None, Local::now(), 20),
            HeldGga::Missing
        ));
    }

    #[test]
    fn switch_toggles() {
        let sw = NtripSwitch::new(false);
        assert!(!sw.is_enabled());
        sw.set(true);
        assert!(sw.is_enabled());
        sw.set(false);
        assert!(!sw.is_enabled());
    }

    #[test]
    fn worker_forwards_rtcm_from_local_caster() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let start = Instant::now();
            let (mut sock, _) = loop {
                match listener.accept() {
                    Ok(pair) => break pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        if start.elapsed() > Duration::from_secs(3) {
                            panic!("客户端未连接");
                        }
                        thread::sleep(Duration::from_millis(20));
                    }
                    Err(e) => panic!("accept: {e}"),
                }
            };
            sock.set_nonblocking(false).unwrap();
            sock.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut buf = [0u8; 2048];
            let mut got = Vec::new();
            loop {
                let n = sock.read(&mut buf).expect("read request");
                if n == 0 {
                    panic!("client closed before request ended");
                }
                got.extend_from_slice(&buf[..n]);
                if got.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let text = String::from_utf8_lossy(&got);
            assert!(text.contains("GET /AUTO HTTP/1.0\r\n"), "{text}");
            assert!(
                text.contains("Authorization: Basic dXNlcjpwYXNz\r\n"),
                "{text}"
            );
            sock.write_all(b"ICY 200 OK\r\n").unwrap();
            got.clear();
            loop {
                let n = sock.read(&mut buf).expect("read gga");
                if n == 0 {
                    panic!("client closed before GGA");
                }
                got.extend_from_slice(&buf[..n]);
                if got.windows(2).any(|w| w == b"\r\n") {
                    break;
                }
            }
            let gga = String::from_utf8_lossy(&got);
            assert!(gga.contains("GPGGA"), "{gga}");
            assert!(gga.contains("3109.6000,N"), "{gga}");
            sock.write_all(&[0xD3, 0x00, 0x01, 0xAA]).unwrap();
            thread::sleep(Duration::from_millis(200));
        });

        let cfg = NtripConfig {
            host: "127.0.0.1".into(),
            port,
            mountpoint: "AUTO".into(),
            username: "user".into(),
            password: "pass".into(),
            gga_interval_secs: 10,
            gga_mode: GgaMode::Fixed,
            rtcm_port: "/dev/ttyUSB1".into(),
            rtcm_baud: 115_200,
            fixed_position: Some(FixedPosition {
                latitude: 31.16,
                longitude: 121.54,
                altitude: 10.0,
            }),
        };
        let (rtcm_tx, rtcm_rx) = mpsc::sync_channel(32);
        let state = Arc::new(Mutex::new(AppState::new("t".into(), 60)));
        state.lock().unwrap().stats.connected = true;
        let running = Arc::new(AtomicBool::new(true));
        let handle = NtripHandle::spawn(
            cfg,
            std::env::temp_dir(),
            true,
            rtcm_tx,
            Arc::clone(&state),
            Arc::clone(&running),
        );
        handle.switch().set(true);

        let forwarded = rtcm_rx.recv_timeout(Duration::from_secs(3));
        let stats = state.lock().unwrap().ntrip.clone();
        running.store(false, Ordering::Relaxed);
        handle.switch().set(false);
        drop(handle);
        let server_result = server.join();

        let bytes = forwarded.expect("应转发 RTCM");
        assert_eq!(bytes, vec![0xD3, 0x00, 0x01, 0xAA]);
        assert_eq!(stats.status, NtripStatus::Connected);
        assert!(stats.bytes_total >= 4);
        assert!(stats.gga_sent >= 1);
        assert!(stats.bps >= 4.0);
        server_result.expect("caster 线程不应 panic");
    }
}
