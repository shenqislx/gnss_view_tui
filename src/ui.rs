//! TUI 渲染：仪表盘（CN0 / PVT / DOP / 曲线）+ 控制台。

use chrono::Local;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Axis, Block, Borders, Chart, Dataset, Gauge, GraphType, List, ListItem, Paragraph, Wrap,
};

use crate::app::{App, Mode, Overlay};
use crate::ext::Screen;
use crate::model::{AppState, GnssSystem, LogKind};

/// 渲染整个界面。
pub fn render(frame: &mut Frame, app: &App, state: &AppState) {
    // 顶层扩展屏：独占整屏交给扩展渲染。
    if app.screen == Screen::Ext
        && let Some(ext) = &app.ext
    {
        frame.render_widget(ratatui::widgets::Clear, frame.area());
        ext.render(frame, frame.area(), state);
        return;
    }

    // 覆盖面板独占前台：清屏后只绘制该面板。
    match app.overlay {
        Overlay::Skyplot => {
            frame.render_widget(ratatui::widgets::Clear, frame.area());
            render_skyplot(frame, frame.area(), state);
            return;
        }
        Overlay::Trajectory => {
            frame.render_widget(ratatui::widgets::Clear, frame.area());
            render_trajectory(frame, frame.area(), state);
            return;
        }
        Overlay::PosBias => {
            frame.render_widget(ratatui::widgets::Clear, frame.area());
            render_pos_bias(frame, frame.area(), state);
            return;
        }
        Overlay::None => {}
    }

    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // 顶部状态栏
            Constraint::Min(8),    // 主体：左栏 + CN0 网格（占据更大区域）
            Constraint::Length(8), // 滑动曲线（整行）
            Constraint::Length(8), // 控制台
            Constraint::Length(1), // 输入/快捷键栏
        ])
        .split(frame.area());

    render_header(frame, root[0], state);
    render_body(frame, root[1], state);
    render_curve(frame, root[2], state);
    render_console(frame, root[3], app, state);
    render_input(frame, root[4], app);

    if app.show_help {
        render_help(frame, frame.area());
    }
}

fn render_header(frame: &mut Frame, area: Rect, state: &AppState) {
    let st = &state.stats;
    let uptime = (Local::now() - st.started).num_seconds().max(0);
    let conn = if st.connected {
        Span::styled(" ● 已连接 ", Style::default().fg(Color::Black).bg(Color::Green))
    } else {
        Span::styled(" ○ 未连接 ", Style::default().fg(Color::White).bg(Color::Red))
    };
    let rec = match &st.recording_path {
        Some(_) => Span::styled(" ⏺ 落盘中 ", Style::default().fg(Color::Black).bg(Color::Yellow)),
        None => Span::styled(" 无落盘 ", Style::default().fg(Color::Gray)),
    };

    let line = Line::from(vec![
        Span::styled(" GNSS 监控 ", Style::default().fg(Color::Black).bg(Color::Cyan).bold()),
        Span::raw(" "),
        conn,
        Span::raw(" "),
        rec,
        Span::raw("  "),
        Span::styled(format!("源: {}", st.source_label), Style::default().fg(Color::White)),
        Span::raw("  "),
        Span::styled(
            format!(
                "收: {}  报文: {}  错误: {}  运行: {}s",
                human_bytes(st.bytes_total),
                st.sentences_ok,
                st.parse_errors,
                uptime
            ),
            Style::default().fg(Color::Gray),
        ),
    ]);

    let p = Paragraph::new(line).block(Block::default().borders(Borders::ALL));
    frame.render_widget(p, area);
}

fn render_body(frame: &mut Frame, area: Rect, state: &AppState) {
    // 左栏固定宽度放置 PVT/DOP/星座统计，剩余宽度全部给 CN0 网格。
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(34), Constraint::Min(20)])
        .split(area);

    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(9), Constraint::Length(8), Constraint::Min(3)])
        .split(cols[0]);

    render_pvt(frame, left[0], state);
    render_dop(frame, left[1], state);
    render_sat_summary(frame, left[2], state);

    render_cn0(frame, cols[1], state);
}

fn render_pvt(frame: &mut Frame, area: Rect, state: &AppState) {
    let pvt = &state.pvt;
    let fmt_opt = |v: &Option<f64>, p: usize, unit: &str| match v {
        Some(x) => format!("{x:.*}{unit}", p),
        None => "—".to_string(),
    };
    let lat = match pvt.latitude {
        Some(v) => format!("{:.6}° {}", v.abs(), if v >= 0.0 { "N" } else { "S" }),
        None => "—".to_string(),
    };
    let lon = match pvt.longitude {
        Some(v) => format!("{:.6}° {}", v.abs(), if v >= 0.0 { "E" } else { "W" }),
        None => "—".to_string(),
    };
    let time = match (&pvt.date, &pvt.utc) {
        (Some(d), Some(t)) => format!("{d} {t} UTC"),
        (None, Some(t)) => format!("{t} UTC"),
        _ => "—".to_string(),
    };

    let mut lines = vec![
        kv("时间", &time),
        kv("纬度", &lat),
        kv("经度", &lon),
    ];
    // 高度：差距为零时合并展示；否则分别展示海拔高与椭球高。
    if pvt.has_separation() {
        lines.push(kv("海拔", &fmt_opt(&pvt.altitude, 1, " m")));
        lines.push(kv2(
            "椭球",
            &fmt_opt(&pvt.ellipsoid_height(), 1, " m"),
            "差距",
            &fmt_opt(&pvt.geoid_sep, 1, " m"),
        ));
    } else {
        lines.push(kv("高度", &fmt_opt(&pvt.altitude, 1, " m")));
    }
    lines.push(kv("地速", &fmt_opt(&pvt.speed_mps(), 2, " m/s")));
    lines.push(kv2(
        "航向",
        &fmt_opt(&pvt.course_deg, 1, "°"),
        "状态",
        pvt.fix_quality_text(),
    ));

    let title = format!(" 定位 PVT  [{}] ", if pvt.valid { "有效" } else { "无效" });
    let p = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .title_style(Style::default().fg(Color::Cyan).bold()),
    );
    frame.render_widget(p, area);
}

fn render_dop(frame: &mut Frame, area: Rect, state: &AppState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" 精度因子 DOP ")
        .title_style(Style::default().fg(Color::Cyan).bold());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1); 4])
        .split(inner);

    dop_gauge(frame, rows[0], "PDOP", state.dop.pdop);
    dop_gauge(frame, rows[1], "HDOP", state.dop.hdop);
    dop_gauge(frame, rows[2], "VDOP", state.dop.vdop);

    let used = state.used_count();
    let vis = state.visible_count();
    let info = Paragraph::new(Line::from(vec![
        Span::styled("  在用/可见: ", Style::default().fg(Color::Gray)),
        Span::styled(
            format!("{used}/{vis}"),
            Style::default().fg(Color::Green).bold(),
        ),
        Span::styled("  定位: ", Style::default().fg(Color::Gray)),
        Span::styled(state.pvt.fix_type_text(), Style::default().fg(Color::Yellow)),
    ]));
    frame.render_widget(info, rows[3]);
}

fn dop_gauge(frame: &mut Frame, area: Rect, label: &str, value: Option<f64>) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(7), Constraint::Min(5)])
        .split(area);
    frame.render_widget(
        Paragraph::new(format!(" {label}")).style(Style::default().fg(Color::Gray)),
        cols[0],
    );
    match value {
        Some(v) => {
            let ratio = (v / 6.0).clamp(0.0, 1.0);
            let color = if v < 2.0 {
                Color::Green
            } else if v < 5.0 {
                Color::Yellow
            } else {
                Color::Red
            };
            let g = Gauge::default()
                .gauge_style(Style::default().fg(color))
                .ratio(ratio)
                .label(format!("{v:.2}"));
            frame.render_widget(g, cols[1]);
        }
        None => {
            frame.render_widget(
                Paragraph::new("—").style(Style::default().fg(Color::DarkGray)),
                cols[1],
            );
        }
    }
}

fn render_sat_summary(frame: &mut Frame, area: Rect, state: &AppState) {
    // 各星座统计
    let mut by_sys: std::collections::BTreeMap<GnssSystem, (usize, usize)> = Default::default();
    for sat in state.satellites.values() {
        let sys = sat.system.unwrap_or(GnssSystem::Unknown);
        let e = by_sys.entry(sys).or_default();
        if sat.cn0.is_some() {
            e.0 += 1;
        }
        if sat.used_in_fix {
            e.1 += 1;
        }
    }
    let mut items: Vec<ListItem> = Vec::new();
    for (sys, (vis, used)) in by_sys {
        items.push(ListItem::new(Line::from(vec![
            Span::styled(
                format!(" {:<8}", sys.name()),
                Style::default().fg(system_color(sys)).bold(),
            ),
            Span::styled(format!("可见 {vis:>2}  ", ), Style::default().fg(Color::Gray)),
            Span::styled(format!("在用 {used:>2}"), Style::default().fg(Color::Green)),
        ])));
    }
    if items.is_empty() {
        items.push(ListItem::new("  （暂无卫星数据）").style(Style::default().fg(Color::DarkGray)));
    }
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" 星座统计 ")
            .title_style(Style::default().fg(Color::Cyan).bold()),
    );
    frame.render_widget(list, area);
}

/// CN0 能量条：自适应**多行网格**，充分利用面板宽×高以容纳大量卫星
/// （多星座可见可达 40+ 颗）。每格柱顶显示数值、柱底显示 PRN/仰角/方位；
/// 在用卫星亮色显示且 PRN 绿色加粗，闲置卫星整体变暗。
/// 支持按星座筛选（`f`）与翻页（`←/→`）。
fn render_cn0(frame: &mut Frame, area: Rect, state: &AppState) {
    let sats = state.filtered_satellites();
    let total = sats.len();

    let inner_w = area.width.saturating_sub(2);
    let inner_h = area.height.saturating_sub(2);

    // 标签行数随可用高度自适应。
    let label_lines = if inner_h >= 9 {
        3u16
    } else if inner_h >= 7 {
        2
    } else {
        1
    };

    // 柱高：相比旧版加倍。先用“最小单元高”确定行数，再把剩余高度均摊回柱体，
    // 让柱体尽量高；卫星放不下时不再压缩列宽硬塞，而是分页（←/→ 翻页）。
    let min_bar = 4u16;
    let min_cell = 1 + label_lines + min_bar;
    let rows = (inner_h / min_cell).max(1) as usize;
    let cell_h = (inner_h / rows as u16).clamp(1, inner_h.max(1));
    let bar_rows = cell_h.saturating_sub(1 + label_lines).max(1);

    // 固定舒适列宽（窄终端再收窄），不为塞下全部卫星而压缩。
    let slot_w = if inner_w >= 4 {
        inner_w.min(6)
    } else {
        inner_w.max(1)
    };
    let cols = (inner_w / slot_w).max(1) as usize;
    let capacity = (cols * rows).max(1);
    let pages = total.div_ceil(capacity).max(1);
    let page = state.cn0_page.min(pages - 1);
    state.cn0_max_page.set(pages - 1);

    let title = format!(
        " CN0 dB-Hz │ 滤:{} {}颗 第{}/{}页 │ f:星座 ←→翻页 ",
        state.sat_filter.label(),
        total,
        page + 1,
        pages
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_style(Style::default().fg(Color::Cyan).bold());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if total == 0 {
        let p = Paragraph::new("\n   该筛选下暂无卫星，按 f 切换星座…")
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(p, inner);
        return;
    }
    if inner.width < 3 || inner.height < 3 {
        return;
    }

    let start = page * capacity;
    let end = (start + capacity).min(total);
    let shown = &sats[start..end];

    let buf = frame.buffer_mut();
    for (k, s) in shown.iter().enumerate() {
        let col = (k % cols) as u16;
        let row = (k / cols) as u16;
        let x0 = inner.x + col * slot_w;
        let y0 = inner.y + row * cell_h;
        if x0 + slot_w > inner.x + inner.width || y0 + cell_h > inner.y + inner.height {
            continue;
        }
        draw_cn0_cell(buf, x0, y0, slot_w, bar_rows, label_lines, s);
    }
}

/// 在 `(x0, y0)` 处绘制单颗卫星的 CN0 格：数值 / 柱体 / PRN·仰角·方位。
fn draw_cn0_cell(
    buf: &mut Buffer,
    x0: u16,
    y0: u16,
    slot_w: u16,
    bar_rows: u16,
    label_lines: u16,
    s: &crate::model::Satellite,
) {
    const LOWER: [char; 8] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇'];
    const MAX_CN0: f64 = 55.0;

    let sys = s.system.unwrap_or(GnssSystem::Unknown);
    let cn0 = s.cn0.unwrap_or(0);
    let color = cn0_color(cn0);
    let used = s.used_in_fix;
    let inner_bar_w = slot_w.saturating_sub(1).max(1);

    // 在用柱亮色加粗，闲置柱 DIM 变暗。
    let bar_style = if used {
        Style::default().fg(color).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(color).add_modifier(Modifier::DIM)
    };

    let value_y = y0;
    let bar_bottom_y = y0 + bar_rows;
    let label_y = bar_bottom_y + 1;

    // 顶部数值
    put_str_center(buf, x0, slot_w, value_y, &format!("{cn0}"), bar_style);

    // 柱体（自底向上填充）
    let frac = (cn0 as f64 / MAX_CN0).clamp(0.0, 1.0);
    let total_e = (frac * bar_rows as f64 * 8.0).round() as u32;
    let full = (total_e / 8) as u16;
    let rem = (total_e % 8) as usize;
    for r_idx in 0..bar_rows {
        let y = bar_bottom_y - r_idx;
        let ch = if r_idx < full {
            '█'
        } else if r_idx == full && rem > 0 {
            LOWER[rem]
        } else {
            continue;
        };
        for c in 0..inner_bar_w {
            put_ch(buf, x0 + c, y, ch, bar_style);
        }
    }

    // 底部标签：PRN / 仰角 / 方位。在用卫星 PRN 绿色加粗高亮。
    let prn_style = if used {
        Style::default()
            .fg(Color::LightGreen)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let info_style = if used {
        Style::default().fg(Color::Gray)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let elev = s
        .elevation
        .map(|e| format!("{e}"))
        .unwrap_or_else(|| "--".into());
    let azi = s
        .azimuth
        .map(|a| format!("{a}"))
        .unwrap_or_else(|| "--".into());
    let labels: [(String, Style); 3] = [
        (format!("{}{:02}", sys.tag(), s.prn), prn_style),
        (elev, info_style),
        (azi, info_style),
    ];
    for (li, (txt, st)) in labels.iter().enumerate().take(label_lines as usize) {
        put_str_center(buf, x0, slot_w, label_y + li as u16, txt, *st);
    }
}

/// 在缓冲区指定位置写入单个字符。
fn put_ch(buf: &mut Buffer, x: u16, y: u16, ch: char, style: Style) {
    if let Some(cell) = buf.cell_mut((x, y)) {
        cell.set_char(ch).set_style(style);
    }
}

/// 在 `[x0, x0+width)` 区间内居中写入字符串（超出部分截断）。
fn put_str_center(buf: &mut Buffer, x0: u16, width: u16, y: u16, text: &str, style: Style) {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len() as u16;
    let off = if len < width { (width - len) / 2 } else { 0 };
    for (i, ch) in chars.into_iter().enumerate() {
        let x = x0 + off + i as u16;
        if x >= x0 + width {
            break;
        }
        put_ch(buf, x, y, ch, style);
    }
}

fn render_curve(frame: &mut Frame, area: Rect, state: &AppState) {
    let prof = &state.profile;

    // 标题即图例：窗口信息 + 各模块名（按其折线颜色着色）。
    let mut title_spans = vec![Span::styled(
        format!(" CPU Load: Time Cost (ms) 窗口{} +/- │", state.curve_capacity),
        Style::default().fg(Color::Cyan).bold(),
    )];
    for (i, name) in prof.names.iter().enumerate() {
        title_spans.push(Span::raw(" "));
        title_spans.push(Span::styled(
            name.clone(),
            Style::default().fg(module_color(name, i)).bold(),
        ));
    }
    title_spans.push(Span::raw(" "));
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(title_spans));

    if prof.len() < 2 {
        let p = Paragraph::new("\n   收集 INFO->PROF(us) 数据中…")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(p, area);
        return;
    }

    // 各模块的折线点（x = 历元序号，y = 毫秒）。
    let series_points: Vec<Vec<(f64, f64)>> = prof
        .series
        .iter()
        .map(|s| {
            s.iter()
                .enumerate()
                .map(|(i, v)| (i as f64, *v))
                .collect()
        })
        .collect();

    let x_max = (prof.len() - 1) as f64;
    // Y 轴自适应：上界取数据最大值向上取 nice 值，封顶 1000ms。
    let ymax = 1000.0_f64;

    let datasets: Vec<Dataset> = series_points
        .iter()
        .enumerate()
        .map(|(i, pts)| {
            let color = module_color(&prof.names[i], i);
            Dataset::default()
                .name(prof.names[i].clone())
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(color))
                .data(pts)
        })
        .collect();

    // Y 轴刻度（自适应单位）。
    let y_labels = vec![
        Span::raw(fmt_ms(0.0)),
        Span::raw(fmt_ms(ymax * 0.25)),
        Span::raw(fmt_ms(ymax * 0.5)),
        Span::raw(fmt_ms(ymax * 0.75)),
        Span::raw(fmt_ms(ymax)),
    ];

    let chart = Chart::new(datasets)
        .block(block)
        .x_axis(
            Axis::default()
                .style(Style::default().fg(Color::DarkGray))
                .bounds([0.0, x_max]),
        )
        .y_axis(
            Axis::default()
                .style(Style::default().fg(Color::DarkGray))
                .bounds([0.0, ymax])
                .labels(y_labels),
        );
    frame.render_widget(chart, area);
}

/// 位置偏差 ENU 时间序列（覆盖式前台面板）：相对地面真值的 东/北/天 误差（米）。
/// 纵坐标以 0 为中心、刻度自适应；窗口与 CPU Load 同步（`+`/`-`）。
fn render_pos_bias(frame: &mut Frame, area: Rect, state: &AppState) {
    const E_COLOR: Color = Color::LightRed;
    const N_COLOR: Color = Color::LightGreen;
    const U_COLOR: Color = Color::LightBlue;

    let pb = &state.pos_bias;

    // 标题：窗口 + GT 有效性 + 彩色图例（E/N/U）。
    let gt_span = match state.ground_truth {
        Some(gt) if gt.valid => Span::styled(
            "GT:有效(配置)",
            Style::default().fg(Color::LightGreen).bold(),
        ),
        Some(_) => Span::styled(
            "GT:无效→首点",
            Style::default().fg(Color::Yellow).bold(),
        ),
        None => Span::styled("GT:待定", Style::default().fg(Color::DarkGray)),
    };
    let title_spans = vec![
        Span::styled(
            format!(" 位置偏差 ENU (m) 窗口{} │ ", state.curve_capacity),
            Style::default().fg(Color::Cyan).bold(),
        ),
        gt_span,
        Span::raw(" │ "),
        Span::styled("E", Style::default().fg(E_COLOR).bold()),
        Span::raw(" "),
        Span::styled("N", Style::default().fg(N_COLOR).bold()),
        Span::raw(" "),
        Span::styled("U", Style::default().fg(U_COLOR).bold()),
        Span::styled(
            " │ +/- 窗口 s/t/p 切换 Esc 关闭 ",
            Style::default().fg(Color::DarkGray),
        ),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(title_spans));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // 底部预留一行：实时刷新当前 E/N/U 误差读数（米）。
    let (plot, info) = if inner.height >= 4 {
        (
            Rect::new(inner.x, inner.y, inner.width, inner.height - 1),
            Some(Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1)),
        )
    } else {
        (inner, None)
    };

    // 当前读数：序列尾部（最新历元）。
    let current = match (pb.east.back(), pb.north.back(), pb.up.back()) {
        (Some(&e), Some(&n), Some(&u)) => Some((e, n, u)),
        _ => None,
    };
    if let Some(area) = info {
        frame.render_widget(Paragraph::new(pos_bias_readout(current, E_COLOR, N_COLOR, U_COLOR)), area);
    }

    if pb.len() < 2 {
        let p = Paragraph::new("   等待定位数据计算误差…")
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(p, plot);
        return;
    }

    let mk_points = |dq: &std::collections::VecDeque<f64>| -> Vec<(f64, f64)> {
        dq.iter().enumerate().map(|(i, v)| (i as f64, *v)).collect()
    };
    let east = mk_points(&pb.east);
    let north = mk_points(&pb.north);
    let up = mk_points(&pb.up);

    let x_max = (pb.len() - 1) as f64;
    // Y 轴以 0 为中心，上下界取数据绝对值最大者向上取“整齐”刻度（最小 ±1m）。
    let ymax = crate::model::nice_span_m(pb.max_abs().max(0.5));

    let datasets = vec![
        Dataset::default()
            .name("E")
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(E_COLOR))
            .data(&east),
        Dataset::default()
            .name("N")
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(N_COLOR))
            .data(&north),
        Dataset::default()
            .name("U")
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(U_COLOR))
            .data(&up),
    ];

    let y_labels = vec![
        Span::raw(fmt_signed_m(-ymax)),
        Span::raw(fmt_signed_m(-ymax * 0.5)),
        Span::raw(fmt_signed_m(0.0)),
        Span::raw(fmt_signed_m(ymax * 0.5)),
        Span::raw(fmt_signed_m(ymax)),
    ];

    let chart = Chart::new(datasets)
        .x_axis(
            Axis::default()
                .style(Style::default().fg(Color::DarkGray))
                .bounds([0.0, x_max]),
        )
        .y_axis(
            Axis::default()
                .style(Style::default().fg(Color::DarkGray))
                .bounds([-ymax, ymax])
                .labels(y_labels),
        );
    frame.render_widget(chart, plot);
}

/// 构造位置偏差实时读数行：当前 E/N/U（米）+ 平面/3D 合成误差。
fn pos_bias_readout(
    current: Option<(f64, f64, f64)>,
    e_color: Color,
    n_color: Color,
    u_color: Color,
) -> Line<'static> {
    let Some((e, n, u)) = current else {
        return Line::from(Span::styled(
            " 当前 E/N/U: —  等待定位…",
            Style::default().fg(Color::DarkGray),
        ));
    };
    let h = (e * e + n * n).sqrt();
    let d3 = (e * e + n * n + u * u).sqrt();
    let val = Style::default().fg(Color::White).bold();
    let lbl = |c: Color| Style::default().fg(c).bold();
    Line::from(vec![
        Span::styled(" 当前 ", Style::default().fg(Color::Gray)),
        Span::styled("E ", lbl(e_color)),
        Span::styled(format!("{e:+.3} m  "), val),
        Span::styled("N ", lbl(n_color)),
        Span::styled(format!("{n:+.3} m  "), val),
        Span::styled("U ", lbl(u_color)),
        Span::styled(format!("{u:+.3} m  "), val),
        Span::styled("│ 平面 ", Style::default().fg(Color::Gray)),
        Span::styled(format!("{h:.3} m  "), Style::default().fg(Color::LightCyan).bold()),
        Span::styled("3D ", Style::default().fg(Color::Gray)),
        Span::styled(format!("{d3:.3} m"), Style::default().fg(Color::LightCyan).bold()),
    ])
}

fn render_console(frame: &mut Frame, area: Rect, app: &App, state: &AppState) {
    let height = area.height.saturating_sub(2) as usize;
    let total = state.console.len();
    let scroll = app.console_scroll.min(total.saturating_sub(1));
    // 从底部向上取，console_scroll 表示向上偏移的行数。
    let end = total.saturating_sub(scroll);
    let start = end.saturating_sub(height);

    let items: Vec<ListItem> = state
        .console
        .iter()
        .skip(start)
        .take(end - start)
        .map(|l| {
            let (tag, color) = match l.kind {
                LogKind::Rx => ("RX", Color::Gray),
                LogKind::Tx => ("TX", Color::LightCyan),
                LogKind::Info => ("**", Color::Green),
                LogKind::Error => ("!!", Color::Red),
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{} ", l.time.format("%H:%M:%S")),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(format!("{tag} "), Style::default().fg(color).bold()),
                Span::styled(l.text.clone(), Style::default().fg(color)),
            ]))
        })
        .collect();

    let scroll_hint = if scroll > 0 {
        format!(" 控制台 (上滚 {scroll} 行) ")
    } else {
        " 控制台 ".to_string()
    };
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(scroll_hint)
            .title_style(Style::default().fg(Color::Cyan).bold()),
    );
    frame.render_widget(list, area);
}

fn render_input(frame: &mut Frame, area: Rect, app: &App) {
    let line = match app.mode {
        Mode::Insert => Line::from(vec![
            Span::styled(" 输入 ", Style::default().fg(Color::Black).bg(Color::LightCyan).bold()),
            Span::styled(format!(" TX[{}]> ", app.line_ending.label()), Style::default().fg(Color::LightCyan)),
            Span::raw(&app.input),
            Span::styled("▏", Style::default().fg(Color::White).add_modifier(Modifier::SLOW_BLINK)),
        ]),
        Mode::Normal => Line::from(vec![
            Span::styled(" 命令 ", Style::default().fg(Color::Black).bg(Color::Gray).bold()),
            Span::styled(
                " i:发送  s:天空图  t:轨迹  p:位置偏差  d:DTR  f:星座  ←→:翻页  +/-:窗口  c:清屏  ?:帮助  q:退出",
                Style::default().fg(Color::Gray),
            ),
        ]),
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn render_help(frame: &mut Frame, area: Rect) {
    let w = 58u16.min(area.width.saturating_sub(4));
    let h = 22u16.min(area.height.saturating_sub(2));
    let x = area.x + (area.width - w) / 2;
    let y = area.y + (area.height - h) / 2;
    let popup = Rect::new(x, y, w, h);

    frame.render_widget(ratatui::widgets::Clear, popup);
    let text = vec![
        Line::from(Span::styled("  GNSS 监控 TUI — 帮助", Style::default().fg(Color::Cyan).bold())),
        Line::raw(""),
        Line::raw("  q / Ctrl+C     退出程序"),
        Line::raw("  i              进入输入模式（发送 TX 命令）"),
        Line::raw("  s              天空图 Skyplot（覆盖前台, Esc 关闭）"),
        Line::raw("  t              轨迹图 Trajectory（覆盖前台, Esc 关闭）"),
        Line::raw("  p              位置偏差 ENU（覆盖前台, Esc 关闭）"),
        Line::raw("  Esc            退出输入模式 / 关闭覆盖面板"),
        Line::raw("  Enter          发送当前命令到串口"),
        Line::raw("  d              发送 DTR 复位脉冲 (复位下位机)"),
        Line::raw("  f              CN0 星座筛选 (全部/各星座 循环)"),
        Line::raw("  ← / →          CN0 卫星过多时上一页/下一页"),
        Line::raw("  ↑ / ↓          控制台逐行滚动"),
        Line::raw("  PgUp / PgDn    控制台翻页滚动"),
        Line::raw("  End            回到控制台底部"),
        Line::raw("  + / -          增减窗口 (CPU Load 与位置偏差同步, 10~300)"),
        Line::raw("  c              清空控制台"),
        Line::raw("  l              切换 TX 换行符 (CRLF/LF/无)"),
        Line::raw("  ?              显示/隐藏本帮助"),
        Line::raw(""),
        Line::from(Span::styled("  按任意键关闭", Style::default().fg(Color::DarkGray))),
    ];
    let p = Paragraph::new(text)
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" 帮助 "),
        );
    frame.render_widget(p, popup);
}

// —— 覆盖面板：天空图 ——

/// 卫星天空图：极坐标方位/仰角。圆心=天顶(90°)，外圈=地平线(0°)；
/// 正北朝上、顺时针为方位增大。已跟踪卫星（CN0>0）高亮显示，
/// 并保留少量星轨（每颗星最近变化的若干点）。
fn render_skyplot(frame: &mut Frame, area: Rect, state: &AppState) {
    let tracked = state
        .satellites
        .values()
        .filter(|s| s.cn0.map(|c| c > 0).unwrap_or(false))
        .count();
    let visible = state.visible_count();
    let title = format!(
        " 天空图 Skyplot │ 跟踪 {tracked}/{visible} │ N↑ 顺时针方位 圈=仰角 │ s/t/p 切换 Esc 关闭 "
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_style(Style::default().fg(Color::Cyan).bold());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < 9 || inner.height < 5 {
        return;
    }

    // 圆心与半径：终端字符高约为宽的两倍，故水平半径取垂直半径的 2 倍以接近正圆。
    let ry = ((inner.height.saturating_sub(1)) / 2) as f64;
    let rx_avail = ((inner.width.saturating_sub(1)) / 2) as f64;
    let r = ry.min(rx_avail / 2.0).max(2.0);
    let (rx, ry) = (r * 2.0, r);
    let cx = inner.x as f64 + inner.width as f64 / 2.0;
    let cy = inner.y as f64 + inner.height as f64 / 2.0;

    // (仰角, 方位) -> 屏幕坐标。
    let to_xy = |elev: f64, azi: f64| -> (i32, i32) {
        let factor = ((90.0 - elev.clamp(0.0, 90.0)) / 90.0).clamp(0.0, 1.0);
        let th = azi.to_radians();
        let x = cx + rx * factor * th.sin();
        let y = cy - ry * factor * th.cos();
        (x.round() as i32, y.round() as i32)
    };

    let buf = frame.buffer_mut();

    // 仰角圈：0°(地平线) / 30° / 60°。
    let grid_style = Style::default().fg(Color::DarkGray);
    for ele in [0.0_f64, 30.0, 60.0] {
        let factor = (90.0 - ele) / 90.0;
        let mut deg = 0;
        while deg < 360 {
            let th = (deg as f64).to_radians();
            let x = (cx + rx * factor * th.sin()).round() as i32;
            let y = (cy - ry * factor * th.cos()).round() as i32;
            put_ch_checked(buf, inner, x, y, '·', grid_style);
            deg += 4;
        }
    }
    // 天顶 + 方位基准线（N/E/S/W 方向的引导点）。
    put_ch_checked(buf, inner, cx.round() as i32, cy.round() as i32, '+', grid_style);

    // 方位标签 N/E/S/W。
    let label_style = Style::default().fg(Color::Gray).bold();
    let (nx, ny) = to_xy(0.0, 0.0);
    put_ch_checked(buf, inner, nx, ny, 'N', label_style);
    let (ex, ey) = to_xy(0.0, 90.0);
    put_ch_checked(buf, inner, ex, ey, 'E', label_style);
    let (sx, sy) = to_xy(0.0, 180.0);
    put_ch_checked(buf, inner, sx, sy, 'S', label_style);
    let (wx, wy) = to_xy(0.0, 270.0);
    put_ch_checked(buf, inner, wx, wy, 'W', label_style);

    // 逐颗卫星：先画星轨，再画当前标记。
    for sat in state.satellites.values() {
        let (Some(el), Some(az)) = (sat.elevation, sat.azimuth) else {
            continue;
        };
        let sys = sat.system.unwrap_or(GnssSystem::Unknown);
        let tracked = sat.cn0.map(|c| c > 0).unwrap_or(false);
        let base = system_color(sys);

        // 星轨：最近变化的若干点（不含当前点），暗淡小点。
        let trail_len = sat.trail.len();
        if trail_len > 1 {
            for &(te, ta) in sat.trail.iter().take(trail_len - 1) {
                let (tx, ty) = to_xy(te as f64, ta as f64);
                put_ch_checked(
                    buf,
                    inner,
                    tx,
                    ty,
                    '·',
                    Style::default().fg(base).add_modifier(Modifier::DIM),
                );
            }
        }

        // 当前标记：跟踪中亮色加粗（星座标识字母），否则暗淡空心点。
        let (x, y) = to_xy(el as f64, az as f64);
        if tracked {
            let mstyle = Style::default().fg(base).add_modifier(Modifier::BOLD);
            put_str_checked(buf, inner, x, y, sys.tag(), mstyle);
            // PRN 紧随其后（空间允许时）。
            let prn = format!("{:02}", sat.prn);
            put_str_checked(
                buf,
                inner,
                x + 1,
                y,
                &prn,
                Style::default().fg(base),
            );
        } else {
            put_ch_checked(
                buf,
                inner,
                x,
                y,
                '∘',
                Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
            );
        }
    }
}

// —— 覆盖面板：轨迹图 ——

/// 定位轨迹图：依据经纬度绘制，自适应网格框与比例尺。
/// 起点居中、不跟随，定位点越界后才重新居中绘制；保留最近 600 个点。
fn render_trajectory(frame: &mut Frame, area: Rect, state: &AppState) {
    let span = state.traj_span_m;
    let full_w = span * 2.0;
    let title = format!(
        " 轨迹 Trajectory │ 点 {}/{} │ 全宽 {} 网格 {} │ s/t/p 切换 Esc 关闭 ",
        state.track.len(),
        crate::model::TRACK_MAX,
        fmt_dist(full_w),
        fmt_dist(span),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_style(Style::default().fg(Color::Cyan).bold());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < 9 || inner.height < 6 {
        return;
    }

    let Some((lat0, lon0)) = state.traj_center else {
        let p = Paragraph::new("\n   等待有效定位 (经纬度) …")
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(p, inner);
        return;
    };

    // 底部预留一行展示中心经纬度与比例尺（用 Paragraph 正确处理中文宽字符）。
    let info_area = Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);
    let plot = Rect::new(inner.x, inner.y, inner.width, inner.height - 1);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" 中心 ", Style::default().fg(Color::Gray)),
            Span::styled(
                format!("{lat0:.6}, {lon0:.6}"),
                Style::default().fg(Color::White),
            ),
            Span::styled("   比例尺 全宽 ", Style::default().fg(Color::Gray)),
            Span::styled(fmt_dist(full_w), Style::default().fg(Color::LightCyan).bold()),
            Span::styled("  ◉当前 S起点", Style::default().fg(Color::DarkGray)),
        ])),
        info_area,
    );

    // 等比例视图：水平半径取垂直半径 2 倍（字符宽高比≈1:2），使距离不失真。
    let ry = ((plot.height.saturating_sub(1)) / 2) as f64;
    let rx_avail = ((plot.width.saturating_sub(1)) / 2) as f64;
    let r = ry.min(rx_avail / 2.0).max(2.0);
    let (rx, ry) = (r * 2.0, r);
    let cx = plot.x as f64 + plot.width as f64 / 2.0;
    let cy = plot.y as f64 + plot.height as f64 / 2.0;

    let buf = frame.buffer_mut();

    // 网格：四分位点线（稀疏虚线，每 2 格一个点）+ 中心十字。
    let grid_style = Style::default().fg(Color::DarkGray);
    for q in [-1.0_f64, -0.5, 0.0, 0.5, 1.0] {
        let x = (cx + q * rx).round() as i32;
        let mut yy = (cy - ry).round() as i32;
        let y_end = (cy + ry).round() as i32;
        while yy <= y_end {
            put_ch_checked(buf, plot, x, yy, '·', grid_style);
            yy += 2;
        }
        let y = (cy + q * ry).round() as i32;
        let mut xx = (cx - rx).round() as i32;
        let x_end = (cx + rx).round() as i32;
        while xx <= x_end {
            put_ch_checked(buf, plot, xx, y, '·', grid_style);
            xx += 2;
        }
    }
    put_ch_checked(buf, plot, cx.round() as i32, cy.round() as i32, '+', Style::default().fg(Color::Gray));

    // 经纬度米偏移 -> 屏幕坐标。
    let to_xy = |lat: f64, lon: f64| -> (i32, i32) {
        let (dx, dy) = crate::model::enu_offset_m(lat0, lon0, lat, lon);
        let x = cx + (dx / span) * rx;
        let y = cy - (dy / span) * ry;
        (x.round() as i32, y.round() as i32)
    };

    // 轨迹点：越旧越暗，最新点高亮；起点单独标记。
    let n = state.track.len();
    for (i, &(lat, lon)) in state.track.iter().enumerate() {
        let (x, y) = to_xy(lat, lon);
        let is_last = i + 1 == n;
        let is_first = i == 0;
        if is_last {
            put_ch_checked(
                buf,
                plot,
                x,
                y,
                '◉',
                Style::default().fg(Color::LightGreen).add_modifier(Modifier::BOLD),
            );
        } else if is_first {
            put_ch_checked(
                buf,
                plot,
                x,
                y,
                'S',
                Style::default().fg(Color::LightYellow).bold(),
            );
        } else {
            // 越接近当前点越亮。
            let recent = n.saturating_sub(i) <= 30;
            let style = if recent {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::Blue).add_modifier(Modifier::DIM)
            };
            put_ch_checked(buf, plot, x, y, '•', style);
        }
    }
}

/// 距离文本：≥1000m 用 km，否则米。
fn fmt_dist(m: f64) -> String {
    if m >= 1000.0 {
        format!("{:.2}km", m / 1000.0)
    } else if m >= 1.0 {
        format!("{m:.0}m")
    } else {
        format!("{m:.1}m")
    }
}

/// 在 `inner` 范围内写字符（带边界裁剪，坐标用 i32 以便越界判断）。
fn put_ch_checked(buf: &mut Buffer, inner: Rect, x: i32, y: i32, ch: char, style: Style) {
    if x < inner.x as i32
        || y < inner.y as i32
        || x >= (inner.x + inner.width) as i32
        || y >= (inner.y + inner.height) as i32
    {
        return;
    }
    put_ch(buf, x as u16, y as u16, ch, style);
}

/// 在 `inner` 范围内从 `(x, y)` 向右写字符串（逐字符裁剪）。
fn put_str_checked(buf: &mut Buffer, inner: Rect, x: i32, y: i32, text: &str, style: Style) {
    for (i, ch) in text.chars().enumerate() {
        put_ch_checked(buf, inner, x + i as i32, y, ch, style);
    }
}

// —— 辅助函数 ——

fn kv(key: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!(" {key:<5}", ), Style::default().fg(Color::Gray)),
        Span::styled(value.to_string(), Style::default().fg(Color::White).bold()),
    ])
}

fn kv2(k1: &str, v1: &str, k2: &str, v2: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!(" {k1:<5}", ), Style::default().fg(Color::Gray)),
        Span::styled(format!("{v1:<12}"), Style::default().fg(Color::White).bold()),
        Span::styled(format!(" {k2:<5}", ), Style::default().fg(Color::Gray)),
        Span::styled(v2.to_string(), Style::default().fg(Color::Yellow).bold()),
    ])
}

fn system_color(sys: GnssSystem) -> Color {
    match sys {
        GnssSystem::Gps => Color::LightGreen,
        GnssSystem::Glonass => Color::LightMagenta,
        GnssSystem::Galileo => Color::LightBlue,
        GnssSystem::BeiDou => Color::LightCyan,
        GnssSystem::Qzss => Color::LightYellow,
        GnssSystem::Navic => Color::LightRed,
        GnssSystem::Unknown => Color::Gray,
    }
}

fn cn0_color(cn0: u16) -> Color {
    if cn0 >= 40 {
        Color::LightGreen
    } else if cn0 >= 30 {
        Color::LightYellow
    } else if cn0 >= 20 {
        Color::Yellow
    } else {
        Color::Red
    }
}

/// 模块折线配色：`PE` 固定红色，其余按出现顺序从调色板分配。
fn module_color(name: &str, index: usize) -> Color {
    if name == "PE" {
        return Color::Red;
    }
    const PALETTE: [Color; 8] = [
        Color::LightGreen,
        Color::LightBlue,
        Color::LightYellow,
        Color::LightMagenta,
        Color::LightCyan,
        Color::Green,
        Color::Blue,
        Color::Magenta,
    ];
    PALETTE[index % PALETTE.len()]
}

/// 毫秒刻度文本：根据量级自适应小数位。
fn fmt_ms(v: f64) -> String {
    if v >= 10.0 {
        format!("{v:.0}")
    } else if v >= 1.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    }
}

/// 带符号的米刻度文本（位置偏差 Y 轴），按量级自适应小数位。
fn fmt_signed_m(v: f64) -> String {
    let a = v.abs();
    if a >= 10.0 {
        format!("{v:.0}")
    } else if a >= 1.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    }
}

fn human_bytes(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::model::Satellite;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};

    /// 是否存在“绿色加粗的数字”单元：用于校验在用卫星的 PRN 高亮（如 G01 的 0/1）。
    fn has_green_bold_digit(terminal: &Terminal<TestBackend>) -> bool {
        let buf = terminal.backend().buffer();
        let area = *buf.area();
        for y in 0..area.height {
            for x in 0..area.width {
                let cell = &buf[(x, y)];
                let st = cell.style();
                let is_digit = cell.symbol().chars().all(|c| c.is_ascii_digit())
                    && !cell.symbol().is_empty();
                if is_digit
                    && st.fg == Some(Color::LightGreen)
                    && st.add_modifier.contains(Modifier::BOLD)
                {
                    return true;
                }
            }
        }
        false
    }

    /// 把渲染缓冲区拼成纯文本，便于断言字符是否出现。
    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        let area = *buf.area();
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn sat(sys: GnssSystem, prn: u16, cn0: u16, elev: u16, az: u16, used: bool) -> Satellite {
        Satellite {
            system: Some(sys),
            prn,
            elevation: Some(elev),
            azimuth: Some(az),
            cn0: Some(cn0),
            used_in_fix: used,
            last_seen: Some(Local::now()),
            trail: Default::default(),
        }
    }

    #[test]
    fn renders_cn0_bars_labels_and_used_highlight() {
        let mut state = AppState::new("test".into(), 60);
        for s in [
            sat(GnssSystem::Gps, 1, 46, 45, 83, true),
            sat(GnssSystem::Gps, 3, 38, 67, 123, false),
            sat(GnssSystem::BeiDou, 7, 30, 12, 30, true),
        ] {
            state.upsert_satellite(s);
        }
        // upsert 不改动 used_in_fix（真实流程由 GSA 设置），测试里手动标记。
        for s in state.satellites.values_mut() {
            if s.prn == 1 || s.prn == 7 {
                s.used_in_fix = true;
            }
        }
        state.pvt.altitude = Some(12.0);
        state.pvt.geoid_sep = Some(8.5);
        state.pvt.speed_kn = Some(2.0);
        state.pvt.valid = true;

        let state = Arc::new(Mutex::new(state));
        let (cmd_tx, _cmd_rx) = mpsc::channel();
        let app = App::new(Arc::clone(&state), cmd_tx, Arc::new(AtomicBool::new(true)));

        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let guard = state.lock().unwrap();
        terminal.draw(|f| render(f, &app, &guard)).unwrap();
        drop(guard);

        let text = buffer_text(&terminal);
        // CN0 能量条
        assert!(text.contains('█'), "应绘制能量条");
        // 在用卫星：PRN 标签以绿色加粗高亮（用样式校验，避免与其它绿色冲突）
        assert!(
            has_green_bold_digit(&terminal),
            "在用卫星的 PRN 标签应为绿色加粗"
        );
        // 仰角/方位标签（ASCII 数字）
        assert!(text.contains("123"), "应显示方位角标签");
        // PVT 高度分开展示与速度单位（CJK 宽字符在缓冲区后跟空格，按单字符校验）
        assert!(text.contains('椭') && text.contains('差'), "差距非零应分开展示椭球高");
        assert!(text.contains("m/s"), "速度应换算为 m/s");
    }

    #[test]
    fn cn0_grid_pages_when_many_satellites() {
        use crate::model::SatFilter;
        let mut st = AppState::new("test".into(), 60);
        // 30 颗卫星：GPS 1..=18，北斗 1..=12。
        for prn in 1..=18 {
            st.upsert_satellite(sat(GnssSystem::Gps, prn, 35, 40, 100, false));
        }
        for prn in 1..=12 {
            st.upsert_satellite(sat(GnssSystem::BeiDou, prn, 35, 40, 100, false));
        }
        assert_eq!(st.filtered_satellites().len(), 30);

        let state = Arc::new(Mutex::new(st));
        let (cmd_tx, _rx) = mpsc::channel();
        let app = App::new(Arc::clone(&state), cmd_tx, Arc::new(AtomicBool::new(true)));

        // 小终端：放不下 → 多页。
        {
            let mut term = Terminal::new(TestBackend::new(60, 30)).unwrap();
            let g = state.lock().unwrap();
            term.draw(|f| render(f, &app, &g)).unwrap();
            assert!(g.cn0_max_page.get() >= 1, "小面板应分页");
        }
        // 大终端：一页放下全部。
        {
            let mut term = Terminal::new(TestBackend::new(170, 50)).unwrap();
            let g = state.lock().unwrap();
            term.draw(|f| render(f, &app, &g)).unwrap();
            assert_eq!(g.cn0_max_page.get(), 0, "大面板应单页显示全部");
        }

        // 筛选循环：全部 → GPS → 北斗 → 全部。
        let mut g = state.lock().unwrap();
        assert!(matches!(g.sat_filter, SatFilter::All));
        g.cycle_filter();
        assert!(matches!(g.sat_filter, SatFilter::Only(GnssSystem::Gps)));
        assert_eq!(g.filtered_satellites().len(), 18);
        g.cycle_filter();
        assert!(matches!(g.sat_filter, SatFilter::Only(GnssSystem::BeiDou)));
        assert_eq!(g.filtered_satellites().len(), 12);
        g.cycle_filter();
        assert!(matches!(g.sat_filter, SatFilter::All));
    }

    #[test]
    fn renders_skyplot_overlay_with_tracked_marker() {
        let mut state = AppState::new("test".into(), 60);
        // 一颗跟踪中 (CN0>0) + 一颗未跟踪 (CN0=0)。
        state.upsert_satellite(sat(GnssSystem::Gps, 1, 45, 30, 90, true));
        state.upsert_satellite(sat(GnssSystem::BeiDou, 7, 0, 10, 200, false));

        let state = Arc::new(Mutex::new(state));
        let (cmd_tx, _rx) = mpsc::channel();
        let mut app = App::new(Arc::clone(&state), cmd_tx, Arc::new(AtomicBool::new(true)));
        app.overlay = Overlay::Skyplot;

        let mut term = Terminal::new(TestBackend::new(80, 30)).unwrap();
        let g = state.lock().unwrap();
        term.draw(|f| render(f, &app, &g)).unwrap();
        drop(g);

        let text = buffer_text(&term);
        assert!(text.contains("Skyplot"), "应显示天空图标题");
        assert!(text.contains('N') && text.contains('E'), "应显示方位标签");
        // 跟踪中卫星以星座标识 G 高亮标记。
        assert!(text.contains('G'), "跟踪中卫星应有标记");
    }

    #[test]
    fn renders_trajectory_overlay_with_points() {
        let mut state = AppState::new("test".into(), 60);
        state.push_track_point(31.2304, 121.4737);
        state.push_track_point(31.23045, 121.47375);

        let state = Arc::new(Mutex::new(state));
        let (cmd_tx, _rx) = mpsc::channel();
        let mut app = App::new(Arc::clone(&state), cmd_tx, Arc::new(AtomicBool::new(true)));
        app.overlay = Overlay::Trajectory;

        let mut term = Terminal::new(TestBackend::new(80, 30)).unwrap();
        let g = state.lock().unwrap();
        term.draw(|f| render(f, &app, &g)).unwrap();
        drop(g);

        let text = buffer_text(&term);
        assert!(text.contains("Trajectory"), "应显示轨迹图标题");
        assert!(text.contains('S'), "应标记起点 S");
        assert!(text.contains('◉'), "应高亮当前点");
    }

    #[test]
    fn overlays_render_end_to_end_from_demo_stream() {
        use crate::parser::{Parser, nmea::NmeaParser};
        let mut state = AppState::new("demo".into(), 60);
        let mut parser = NmeaParser::new();
        for i in 0..40 {
            let data = crate::demo::generate_epoch(i as f64 * 3.0);
            let mut out = Vec::new();
            parser.feed(data.as_bytes(), &mut out);
            for ev in out {
                crate::pipeline::apply_event_for_test(&mut state, ev);
            }
        }
        // 每个 RMC 历元应记录一个轨迹点；星轨应被采集。
        assert_eq!(state.track.len(), 40, "应按历元累计轨迹点");
        assert!(state.traj_center.is_some(), "应已设定视图中心");
        assert!(
            state.satellites.values().any(|s| s.trail.len() > 1),
            "运动卫星应累计星轨"
        );

        let state = Arc::new(Mutex::new(state));
        let (cmd_tx, _rx) = mpsc::channel();
        let mut app = App::new(Arc::clone(&state), cmd_tx, Arc::new(AtomicBool::new(true)));

        app.overlay = Overlay::Skyplot;
        let mut term = Terminal::new(TestBackend::new(90, 32)).unwrap();
        {
            let g = state.lock().unwrap();
            term.draw(|f| render(f, &app, &g)).unwrap();
        }
        assert!(buffer_text(&term).contains("Skyplot"));

        app.overlay = Overlay::Trajectory;
        let mut term = Terminal::new(TestBackend::new(90, 32)).unwrap();
        {
            let g = state.lock().unwrap();
            term.draw(|f| render(f, &app, &g)).unwrap();
        }
        let text = buffer_text(&term);
        assert!(text.contains("Trajectory") && text.contains('◉'));
    }

    #[test]
    fn renders_pos_bias_series_with_gt_validity() {
        use crate::model::GroundTruth;
        let mut state = AppState::new("test".into(), 60);
        state.ground_truth = Some(GroundTruth {
            lat: 31.23,
            lon: 121.47,
            alt: 10.0,
            valid: true,
        });
        // 多个历元的偏差采样，使曲线可绘制。
        for i in 0..10 {
            let d = i as f64 * 1e-6;
            state.push_pos_bias(31.23 + d, 121.47 + d, Some(10.0 + i as f64 * 0.1));
        }

        let state = Arc::new(Mutex::new(state));
        let (cmd_tx, _rx) = mpsc::channel();
        let mut app = App::new(Arc::clone(&state), cmd_tx, Arc::new(AtomicBool::new(true)));
        app.overlay = Overlay::PosBias;
        let mut term = Terminal::new(TestBackend::new(160, 40)).unwrap();
        let g = state.lock().unwrap();
        term.draw(|f| render(f, &app, &g)).unwrap();
        drop(g);

        let text = buffer_text(&term);
        // CJK 宽字符在测试缓冲区后跟空格，按单字符校验。
        assert!(text.contains('位') && text.contains('偏'), "应显示位置偏差标题");
        assert!(text.contains('有') && text.contains('效'), "有效配置 GT 应标注 valid");
        // 实时读数行：当前 E/N/U 数值 + 平面/3D 合成误差。
        assert!(text.contains('当') && text.contains('前'), "应显示实时读数行");
        assert!(text.contains('平') && text.contains('面'), "应显示平面合成误差");
        assert!(text.contains("3D"), "应显示 3D 合成误差");
    }

    #[test]
    fn renders_pos_bias_fallback_label_when_no_config_gt() {
        let mut state = AppState::new("test".into(), 60);
        // 无配置 GT：首点回退，标题应注明无效。
        for i in 0..5 {
            let d = i as f64 * 1e-6;
            state.push_pos_bias(31.23 + d, 121.47 + d, Some(10.0));
        }

        let state = Arc::new(Mutex::new(state));
        let (cmd_tx, _rx) = mpsc::channel();
        let mut app = App::new(Arc::clone(&state), cmd_tx, Arc::new(AtomicBool::new(true)));
        app.overlay = Overlay::PosBias;
        let mut term = Terminal::new(TestBackend::new(160, 40)).unwrap();
        let g = state.lock().unwrap();
        term.draw(|f| render(f, &app, &g)).unwrap();
        drop(g);

        let text = buffer_text(&term);
        assert!(text.contains('首') && text.contains('点'), "回退 GT 应标注 首点");
    }

    #[test]
    fn renders_single_height_when_separation_zero() {
        let mut state = AppState::new("test".into(), 60);
        state.upsert_satellite(sat(GnssSystem::Gps, 1, 40, 45, 83, true));
        state.pvt.altitude = Some(12.0);
        state.pvt.geoid_sep = Some(0.0);
        state.pvt.valid = true;

        let state = Arc::new(Mutex::new(state));
        let (cmd_tx, _cmd_rx) = mpsc::channel();
        let app = App::new(Arc::clone(&state), cmd_tx, Arc::new(AtomicBool::new(true)));
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        let guard = state.lock().unwrap();
        terminal.draw(|f| render(f, &app, &guard)).unwrap();
        drop(guard);

        let text = buffer_text(&terminal);
        assert!(text.contains('高') && text.contains('度'), "差距为零应合并为单一高度");
        assert!(!text.contains('椭'), "差距为零不应出现椭球高行");
    }
}
