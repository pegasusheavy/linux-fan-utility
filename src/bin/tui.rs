// Copyright (c) 2026 Pegasus Heavy Industries LLC
// Licensed under the MIT License

//! fanctl-tui: terminal UI client that connects to the fanctl daemon
//! over a Unix domain socket and provides live monitoring and control.

use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use linux_fan_utility::config::{self, FanAssignment};
use linux_fan_utility::curve::CurvePoint;
use linux_fan_utility::hwmon::{FanStatus, TempStatus};
use linux_fan_utility::protocol::{self, FanAssignmentInfo, Request, Response};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, Tabs,
    },
};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(name = "fanctl-tui", about = "Linux fan control TUI client")]
struct Cli {
    /// Path to the daemon socket.
    #[arg(short, long, default_value = config::DEFAULT_SOCKET_PATH)]
    socket: String,
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Dashboard,
    FanControl,
    CurveEditor,
    Config,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::Dashboard, Tab::FanControl, Tab::CurveEditor, Tab::Config];

    fn index(self) -> usize {
        match self {
            Tab::Dashboard => 0,
            Tab::FanControl => 1,
            Tab::CurveEditor => 2,
            Tab::Config => 3,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Tab::Dashboard => "Dashboard",
            Tab::FanControl => "Fan Control",
            Tab::CurveEditor => "Curve Editor",
            Tab::Config => "Config",
        }
    }
}

struct App {
    tab: Tab,
    running: bool,
    status_message: String,
    connection: Option<Connection>,

    // Dashboard
    fans: Vec<FanStatus>,
    temps: Vec<TempStatus>,
    assignments: Vec<FanAssignmentInfo>,

    // Fan control
    fan_list_state: ListState,
    selected_fan_pwm: u8,
    fan_mode_select: FanModeSelect,
    temp_sensor_select: usize,
    curve_select: usize,

    // Curve editor
    curves: Vec<CurveData>,
    curve_list_state: ListState,
    editing_curve: Option<CurveEditState>,

    // Config tab
    config_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FanModeSelect {
    Auto,
    Manual,
    Curve,
}

#[derive(Debug, Clone)]
struct CurveData {
    name: String,
    points: Vec<CurvePoint>,
}

#[derive(Debug, Clone)]
struct CurveEditState {
    name: String,
    points: Vec<CurvePoint>,
    selected_point: usize,
    editing_field: CurveField,
    is_new: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CurveField {
    Name,
    Temp,
    Pwm,
}

struct Connection {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Connection {
    fn connect(path: &str) -> io::Result<Self> {
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Self { stream, reader })
    }

    fn send_request(&mut self, req: &Request) -> io::Result<Response> {
        let encoded = protocol::encode(req).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, format!("Encode error: {e}"))
        })?;
        self.stream.write_all(encoded.as_bytes())?;
        self.stream.flush()?;

        let mut line = String::new();
        self.reader.read_line(&mut line)?;
        protocol::decode(&line).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, format!("Decode error: {e}"))
        })
    }
}

impl App {
    fn new(socket_path: &str) -> Self {
        let connection = match Connection::connect(socket_path) {
            Ok(c) => {
                log::info!("Connected to daemon at {socket_path}");
                Some(c)
            }
            Err(e) => {
                log::error!("Could not connect to daemon: {e}");
                None
            }
        };

        let mut app = Self {
            tab: Tab::Dashboard,
            running: true,
            status_message: String::new(),
            connection,
            fans: Vec::new(),
            temps: Vec::new(),
            assignments: Vec::new(),
            fan_list_state: ListState::default(),
            selected_fan_pwm: 128,
            fan_mode_select: FanModeSelect::Auto,
            temp_sensor_select: 0,
            curve_select: 0,
            curves: Vec::new(),
            curve_list_state: ListState::default(),
            editing_curve: None,
            config_path: config::DEFAULT_CONFIG_PATH.to_string(),
        };

        if app.connection.is_some() {
            app.refresh_status();
            app.refresh_curves();
        } else {
            app.status_message =
                "Not connected to daemon. Is fanctl-daemon running?".to_string();
        }

        app
    }

    fn refresh_status(&mut self) {
        if let Some(conn) = &mut self.connection {
            match conn.send_request(&Request::GetStatus) {
                Ok(Response::Status {
                    fans,
                    temps,
                    assignments,
                }) => {
                    self.fans = fans;
                    self.temps = temps;
                    self.assignments = assignments;
                }
                Ok(Response::Error { message }) => {
                    self.status_message = format!("Error: {message}");
                }
                Err(e) => {
                    self.status_message = format!("Connection error: {e}");
                    self.connection = None;
                }
                _ => {}
            }
        }
    }

    fn refresh_curves(&mut self) {
        if let Some(conn) = &mut self.connection {
            match conn.send_request(&Request::ListCurves) {
                Ok(Response::Curves { curves }) => {
                    self.curves = curves
                        .into_iter()
                        .map(|c| CurveData {
                            name: c.name,
                            points: c.points,
                        })
                        .collect();
                }
                Err(e) => {
                    self.status_message = format!("Connection error: {e}");
                    self.connection = None;
                }
                _ => {}
            }
        }
    }

    fn selected_fan(&self) -> Option<&FanStatus> {
        self.fan_list_state
            .selected()
            .and_then(|i| self.fans.get(i))
    }

    fn selected_fan_assignment(&self) -> Option<&FanAssignment> {
        let fan = self.selected_fan()?;
        self.assignments
            .iter()
            .find(|a| a.fan_id == fan.id)
            .map(|a| &a.assignment)
    }

    fn apply_fan_setting(&mut self) {
        let Some(fan) = self.selected_fan().cloned() else {
            return;
        };

        let req = match self.fan_mode_select {
            FanModeSelect::Auto => Request::SetAuto {
                fan_id: fan.id.clone(),
            },
            FanModeSelect::Manual => Request::SetManual {
                fan_id: fan.id.clone(),
                pwm: self.selected_fan_pwm,
            },
            FanModeSelect::Curve => {
                let curve_name = self
                    .curves
                    .get(self.curve_select)
                    .map(|c| c.name.clone())
                    .unwrap_or_default();
                let temp_sensor_id = self
                    .temps
                    .get(self.temp_sensor_select)
                    .map(|t| t.id.clone())
                    .unwrap_or_default();

                if curve_name.is_empty() || temp_sensor_id.is_empty() {
                    self.status_message = "Select a curve and temp sensor first".to_string();
                    return;
                }

                Request::SetCurve {
                    fan_id: fan.id.clone(),
                    curve_name,
                    temp_sensor_id,
                }
            }
        };

        if let Some(conn) = &mut self.connection {
            match conn.send_request(&req) {
                Ok(Response::Ok { message }) => {
                    self.status_message = message;
                }
                Ok(Response::Error { message }) => {
                    self.status_message = format!("Error: {message}");
                }
                Err(e) => {
                    self.status_message = format!("Connection error: {e}");
                    self.connection = None;
                }
                _ => {}
            }
        }
        self.refresh_status();
    }

    fn save_config(&mut self) {
        if let Some(conn) = &mut self.connection {
            match conn.send_request(&Request::SaveConfig) {
                Ok(Response::Ok { message }) => {
                    self.status_message = message;
                }
                Ok(Response::Error { message }) => {
                    self.status_message = format!("Error: {message}");
                }
                Err(e) => {
                    self.status_message = format!("Connection error: {e}");
                    self.connection = None;
                }
                _ => {}
            }
        }
    }

    fn save_curve(&mut self) {
        let Some(edit) = &self.editing_curve else {
            return;
        };
        let name = edit.name.clone();
        let points = edit.points.clone();

        if let Some(conn) = &mut self.connection {
            match conn.send_request(&Request::UpsertCurve { name, points }) {
                Ok(Response::Ok { message }) => {
                    self.status_message = message;
                    self.editing_curve = None;
                    self.refresh_curves();
                }
                Ok(Response::Error { message }) => {
                    self.status_message = format!("Error: {message}");
                }
                Err(e) => {
                    self.status_message = format!("Connection error: {e}");
                    self.connection = None;
                }
                _ => {}
            }
        }
    }

    fn delete_selected_curve(&mut self) {
        let Some(idx) = self.curve_list_state.selected() else {
            return;
        };
        let Some(curve) = self.curves.get(idx) else {
            return;
        };
        let name = curve.name.clone();

        if let Some(conn) = &mut self.connection {
            match conn.send_request(&Request::DeleteCurve { name }) {
                Ok(Response::Ok { message }) => {
                    self.status_message = message;
                    self.refresh_curves();
                }
                Ok(Response::Error { message }) => {
                    self.status_message = format!("Error: {message}");
                }
                Err(e) => {
                    self.status_message = format!("Connection error: {e}");
                    self.connection = None;
                }
                _ => {}
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let cli = Cli::parse();

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(&cli.socket);

    let result = run_app(&mut terminal, &mut app);

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> anyhow::Result<()> {
    let tick_rate = Duration::from_millis(500);

    while app.running {
        terminal.draw(|f| ui(f, app))?;

        if event::poll(tick_rate)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                handle_input(app, key.code, key.modifiers);
            }
        } else {
            // Periodic refresh
            app.refresh_status();
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Input handling
// ---------------------------------------------------------------------------

fn handle_input(app: &mut App, key: KeyCode, modifiers: KeyModifiers) {
    // Global keys
    match key {
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.running = false;
            return;
        }
        KeyCode::Char('q') if app.editing_curve.is_none() => {
            app.running = false;
            return;
        }
        _ => {}
    }

    // If editing a curve, handle curve editor keys
    if app.editing_curve.is_some() {
        handle_curve_edit_input(app, key);
        return;
    }

    // Tab switching
    match key {
        KeyCode::Char('1') => app.tab = Tab::Dashboard,
        KeyCode::Char('2') => app.tab = Tab::FanControl,
        KeyCode::Char('3') => app.tab = Tab::CurveEditor,
        KeyCode::Char('4') => app.tab = Tab::Config,
        KeyCode::Tab => {
            let idx = (app.tab.index() + 1) % Tab::ALL.len();
            app.tab = Tab::ALL[idx];
        }
        KeyCode::BackTab => {
            let idx = (app.tab.index() + Tab::ALL.len() - 1) % Tab::ALL.len();
            app.tab = Tab::ALL[idx];
        }
        _ => {}
    }

    // Tab-specific keys
    match app.tab {
        Tab::Dashboard => handle_dashboard_input(app, key),
        Tab::FanControl => handle_fan_control_input(app, key),
        Tab::CurveEditor => handle_curve_editor_input(app, key),
        Tab::Config => handle_config_input(app, key),
    }
}

fn handle_dashboard_input(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Char('r') => {
            app.refresh_status();
            app.status_message = "Refreshed".to_string();
        }
        _ => {}
    }
}

fn handle_fan_control_input(app: &mut App, key: KeyCode) {
    let fan_count = app.fans.len();
    match key {
        KeyCode::Up | KeyCode::Char('k') => {
            if fan_count > 0 {
                let i = app.fan_list_state.selected().unwrap_or(0);
                let new_i = if i == 0 { fan_count - 1 } else { i - 1 };
                app.fan_list_state.select(Some(new_i));
                load_fan_assignment(app);
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if fan_count > 0 {
                let i = app.fan_list_state.selected().unwrap_or(0);
                let new_i = (i + 1) % fan_count;
                app.fan_list_state.select(Some(new_i));
                load_fan_assignment(app);
            }
        }
        KeyCode::Char('m') => app.fan_mode_select = FanModeSelect::Manual,
        KeyCode::Char('a') => app.fan_mode_select = FanModeSelect::Auto,
        KeyCode::Char('c') => app.fan_mode_select = FanModeSelect::Curve,
        KeyCode::Left | KeyCode::Char('h') => {
            match app.fan_mode_select {
                FanModeSelect::Manual => {
                    app.selected_fan_pwm = app.selected_fan_pwm.saturating_sub(5);
                }
                FanModeSelect::Curve => {
                    if app.temp_sensor_select > 0 {
                        app.temp_sensor_select -= 1;
                    }
                }
                _ => {}
            }
        }
        KeyCode::Right | KeyCode::Char('l') => {
            match app.fan_mode_select {
                FanModeSelect::Manual => {
                    app.selected_fan_pwm = app.selected_fan_pwm.saturating_add(5);
                }
                FanModeSelect::Curve => {
                    if app.temp_sensor_select + 1 < app.temps.len() {
                        app.temp_sensor_select += 1;
                    }
                }
                _ => {}
            }
        }
        KeyCode::Char('[') => {
            if app.curve_select > 0 {
                app.curve_select -= 1;
            }
        }
        KeyCode::Char(']') => {
            if app.curve_select + 1 < app.curves.len() {
                app.curve_select += 1;
            }
        }
        KeyCode::Enter => {
            app.apply_fan_setting();
        }
        _ => {}
    }
}

fn load_fan_assignment(app: &mut App) {
    if let Some(assignment) = app.selected_fan_assignment().cloned() {
        match assignment {
            FanAssignment::Auto => {
                app.fan_mode_select = FanModeSelect::Auto;
            }
            FanAssignment::Manual { pwm } => {
                app.fan_mode_select = FanModeSelect::Manual;
                app.selected_fan_pwm = pwm;
            }
            FanAssignment::Curve {
                curve_name,
                temp_sensor_id,
            } => {
                app.fan_mode_select = FanModeSelect::Curve;
                if let Some(idx) = app.curves.iter().position(|c| c.name == curve_name) {
                    app.curve_select = idx;
                }
                if let Some(idx) = app.temps.iter().position(|t| t.id == temp_sensor_id) {
                    app.temp_sensor_select = idx;
                }
            }
        }
    } else {
        app.fan_mode_select = FanModeSelect::Auto;
    }
}

fn handle_curve_editor_input(app: &mut App, key: KeyCode) {
    let curve_count = app.curves.len();
    match key {
        KeyCode::Up | KeyCode::Char('k') => {
            if curve_count > 0 {
                let i = app.curve_list_state.selected().unwrap_or(0);
                let new_i = if i == 0 { curve_count - 1 } else { i - 1 };
                app.curve_list_state.select(Some(new_i));
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if curve_count > 0 {
                let i = app.curve_list_state.selected().unwrap_or(0);
                let new_i = (i + 1) % curve_count;
                app.curve_list_state.select(Some(new_i));
            }
        }
        KeyCode::Char('n') => {
            // New curve
            app.editing_curve = Some(CurveEditState {
                name: "new_curve".to_string(),
                points: vec![
                    CurvePoint {
                        temp_c: 30.0,
                        pwm: 0,
                    },
                    CurvePoint {
                        temp_c: 90.0,
                        pwm: 255,
                    },
                ],
                selected_point: 0,
                editing_field: CurveField::Name,
                is_new: true,
            });
        }
        KeyCode::Enter | KeyCode::Char('e') => {
            // Edit selected curve
            if let Some(idx) = app.curve_list_state.selected() {
                if let Some(curve) = app.curves.get(idx) {
                    app.editing_curve = Some(CurveEditState {
                        name: curve.name.clone(),
                        points: curve.points.clone(),
                        selected_point: 0,
                        editing_field: CurveField::Temp,
                        is_new: false,
                    });
                }
            }
        }
        KeyCode::Char('d') | KeyCode::Delete => {
            app.delete_selected_curve();
        }
        _ => {}
    }
}

fn handle_curve_edit_input(app: &mut App, key: KeyCode) {
    let Some(edit) = &mut app.editing_curve else {
        return;
    };

    match key {
        KeyCode::Esc => {
            app.editing_curve = None;
        }
        KeyCode::Tab => {
            edit.editing_field = match edit.editing_field {
                CurveField::Name => CurveField::Temp,
                CurveField::Temp => CurveField::Pwm,
                CurveField::Pwm => CurveField::Name,
            };
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if edit.selected_point > 0 {
                edit.selected_point -= 1;
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if edit.selected_point + 1 < edit.points.len() {
                edit.selected_point += 1;
            }
        }
        KeyCode::Left | KeyCode::Char('h') => {
            if let Some(point) = edit.points.get_mut(edit.selected_point) {
                match edit.editing_field {
                    CurveField::Temp => point.temp_c = (point.temp_c - 1.0).max(0.0),
                    CurveField::Pwm => point.pwm = point.pwm.saturating_sub(5),
                    CurveField::Name => {}
                }
            }
        }
        KeyCode::Right | KeyCode::Char('l') => {
            if let Some(point) = edit.points.get_mut(edit.selected_point) {
                match edit.editing_field {
                    CurveField::Temp => point.temp_c = (point.temp_c + 1.0).min(120.0),
                    CurveField::Pwm => point.pwm = point.pwm.saturating_add(5),
                    CurveField::Name => {}
                }
            }
        }
        KeyCode::Char('+') | KeyCode::Char('=') => {
            // Add a new point
            let new_temp = edit
                .points
                .last()
                .map(|p| p.temp_c + 10.0)
                .unwrap_or(50.0)
                .min(120.0);
            edit.points.push(CurvePoint {
                temp_c: new_temp,
                pwm: 128,
            });
            edit.selected_point = edit.points.len() - 1;
        }
        KeyCode::Char('-') => {
            // Remove selected point (keep at least 2)
            if edit.points.len() > 2 {
                edit.points.remove(edit.selected_point);
                if edit.selected_point >= edit.points.len() {
                    edit.selected_point = edit.points.len() - 1;
                }
            }
        }
        KeyCode::Backspace => {
            if edit.editing_field == CurveField::Name && !edit.name.is_empty() {
                edit.name.pop();
            }
        }
        KeyCode::Char(ch) => {
            if edit.editing_field == CurveField::Name {
                if ch.is_alphanumeric() || ch == '_' || ch == '-' {
                    edit.name.push(ch);
                }
            }
        }
        KeyCode::Enter => {
            app.save_curve();
        }
        _ => {}
    }
}

fn handle_config_input(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Char('s') => {
            app.save_config();
        }
        KeyCode::Char('r') => {
            if let Some(conn) = &mut app.connection {
                match conn.send_request(&Request::ReloadConfig) {
                    Ok(Response::Ok { message }) => {
                        app.status_message = message;
                        app.refresh_status();
                        app.refresh_curves();
                    }
                    Ok(Response::Error { message }) => {
                        app.status_message = format!("Error: {message}");
                    }
                    Err(e) => {
                        app.status_message = format!("Connection error: {e}");
                        app.connection = None;
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// UI rendering
// ---------------------------------------------------------------------------

fn ui(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // tab bar
            Constraint::Min(0),   // content
            Constraint::Length(3), // status bar
        ])
        .split(f.area());

    draw_tabs(f, app, chunks[0]);

    match app.tab {
        Tab::Dashboard => draw_dashboard(f, app, chunks[1]),
        Tab::FanControl => draw_fan_control(f, app, chunks[1]),
        Tab::CurveEditor => draw_curve_editor(f, app, chunks[1]),
        Tab::Config => draw_config(f, app, chunks[1]),
    }

    draw_status_bar(f, app, chunks[2]);

    // Curve edit overlay
    if app.editing_curve.is_some() {
        draw_curve_edit_overlay(f, app);
    }
}

fn draw_tabs(f: &mut Frame, app: &App, area: Rect) {
    let titles: Vec<Line> = Tab::ALL.iter().map(|t| Line::from(t.title())).collect();

    let tabs = Tabs::new(titles)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" fanctl "),
        )
        .select(app.tab.index())
        .style(Style::default().fg(Color::Gray))
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );

    f.render_widget(tabs, area);
}

fn draw_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let connected = if app.connection.is_some() {
        Span::styled(" CONNECTED ", Style::default().fg(Color::Green).bold())
    } else {
        Span::styled(" DISCONNECTED ", Style::default().fg(Color::Red).bold())
    };

    let msg = Span::raw(format!("  {}", app.status_message));

    let help = match app.tab {
        Tab::Dashboard => " [r]efresh  [q]uit ",
        Tab::FanControl => " [j/k]nav  [a]uto [m]anual [c]urve  [h/l]adjust  [Enter]apply  [q]uit ",
        Tab::CurveEditor => " [j/k]nav  [n]ew [e]dit [d]elete  [q]uit ",
        Tab::Config => " [s]ave  [r]eload  [q]uit ",
    };

    let status_line = Line::from(vec![connected, msg]);
    let help_line = Line::from(Span::styled(help, Style::default().fg(Color::DarkGray)));

    let paragraph = Paragraph::new(vec![status_line, help_line])
        .block(Block::default().borders(Borders::ALL));

    f.render_widget(paragraph, area);
}

fn draw_dashboard(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    // Fan table
    let fan_rows: Vec<Row> = app
        .fans
        .iter()
        .map(|fan| {
            let label = fan
                .label
                .as_deref()
                .unwrap_or("-");
            let rpm = fan
                .rpm
                .map(|r| format!("{r}"))
                .unwrap_or_else(|| "-".to_string());
            let pwm = fan
                .pwm
                .map(|p| format!("{p} ({:.0}%)", p as f64 / 255.0 * 100.0))
                .unwrap_or_else(|| "-".to_string());
            let mode = fan
                .pwm_enable
                .map(|e| match e {
                    0 => "Off",
                    1 => "Manual",
                    2 => "Auto",
                    _ => "?",
                })
                .unwrap_or("-");

            Row::new(vec![
                Cell::from(fan.id.clone()),
                Cell::from(label.to_string()),
                Cell::from(rpm),
                Cell::from(pwm),
                Cell::from(mode),
            ])
        })
        .collect();

    let fan_table = Table::new(
        fan_rows,
        [
            Constraint::Percentage(25),
            Constraint::Percentage(20),
            Constraint::Percentage(15),
            Constraint::Percentage(20),
            Constraint::Percentage(20),
        ],
    )
    .header(
        Row::new(vec!["Fan ID", "Label", "RPM", "PWM", "Mode"])
            .style(Style::default().fg(Color::Cyan).bold()),
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Fans "),
    );

    f.render_widget(fan_table, chunks[0]);

    // Temp table
    let temp_rows: Vec<Row> = app
        .temps
        .iter()
        .map(|temp| {
            let label = temp
                .label
                .as_deref()
                .unwrap_or("-");
            let value = temp
                .temp_c
                .map(|t| {
                    let color = if t >= 80.0 {
                        Color::Red
                    } else if t >= 60.0 {
                        Color::Yellow
                    } else {
                        Color::Green
                    };
                    Span::styled(format!("{t:.1}°C"), Style::default().fg(color))
                })
                .unwrap_or_else(|| Span::raw("-"));

            Row::new(vec![
                Cell::from(temp.id.clone()),
                Cell::from(label.to_string()),
                Cell::from(value),
                Cell::from(temp.hwmon_name.clone()),
            ])
        })
        .collect();

    let temp_table = Table::new(
        temp_rows,
        [
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
        ],
    )
    .header(
        Row::new(vec!["Sensor ID", "Label", "Temp", "Device"])
            .style(Style::default().fg(Color::Cyan).bold()),
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Temperatures "),
    );

    f.render_widget(temp_table, chunks[1]);
}

fn draw_fan_control(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(area);

    // Fan list
    let items: Vec<ListItem> = app
        .fans
        .iter()
        .map(|fan| {
            let label = fan.label.as_deref().unwrap_or(&fan.id);
            let rpm = fan.rpm.map(|r| format!(" ({r} RPM)")).unwrap_or_default();
            ListItem::new(format!("{label}{rpm}"))
        })
        .collect();

    let fan_list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Select Fan "),
        )
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    f.render_stateful_widget(fan_list, chunks[0], &mut app.fan_list_state.clone());

    // Control panel
    let control_area = chunks[1];
    let control_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),  // Mode selector
            Constraint::Length(5),  // Control value
            Constraint::Min(0),    // Info
        ])
        .split(control_area);

    // Mode selector
    let mode_text = vec![
        Line::from(vec![
            if app.fan_mode_select == FanModeSelect::Auto {
                Span::styled(" ● Auto ", Style::default().fg(Color::Green).bold())
            } else {
                Span::styled(" ○ Auto ", Style::default().fg(Color::Gray))
            },
            Span::raw("  "),
            if app.fan_mode_select == FanModeSelect::Manual {
                Span::styled(" ● Manual ", Style::default().fg(Color::Yellow).bold())
            } else {
                Span::styled(" ○ Manual ", Style::default().fg(Color::Gray))
            },
            Span::raw("  "),
            if app.fan_mode_select == FanModeSelect::Curve {
                Span::styled(" ● Curve ", Style::default().fg(Color::Magenta).bold())
            } else {
                Span::styled(" ○ Curve ", Style::default().fg(Color::Gray))
            },
        ]),
        Line::from(""),
        Line::from(Span::styled(
            " Press [a]uto [m]anual [c]urve to switch mode",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let mode_widget = Paragraph::new(mode_text).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Mode "),
    );
    f.render_widget(mode_widget, control_chunks[0]);

    // Control value area
    let control_text = match app.fan_mode_select {
        FanModeSelect::Auto => {
            vec![
                Line::from("Fan controlled by BIOS/firmware"),
                Line::from(Span::styled(
                    "Press [Enter] to apply",
                    Style::default().fg(Color::DarkGray),
                )),
            ]
        }
        FanModeSelect::Manual => {
            let pct = app.selected_fan_pwm as f64 / 255.0 * 100.0;
            let bar_width = 30;
            let filled = (pct / 100.0 * bar_width as f64) as usize;
            let bar = format!(
                "[{}{}] {:.0}% (PWM {})",
                "█".repeat(filled),
                "░".repeat(bar_width - filled),
                pct,
                app.selected_fan_pwm
            );
            vec![
                Line::from(bar),
                Line::from(Span::styled(
                    "Use [h/l] or [←/→] to adjust, [Enter] to apply",
                    Style::default().fg(Color::DarkGray),
                )),
            ]
        }
        FanModeSelect::Curve => {
            let curve_name = app
                .curves
                .get(app.curve_select)
                .map(|c| c.name.as_str())
                .unwrap_or("(none)");
            let sensor_name = app
                .temps
                .get(app.temp_sensor_select)
                .map(|t| {
                    t.label
                        .as_deref()
                        .unwrap_or(&t.id)
                })
                .unwrap_or("(none)");

            vec![
                Line::from(format!("Curve: {curve_name}  (use [/] to cycle)")),
                Line::from(format!("Sensor: {sensor_name}  (use [h/l] to cycle)")),
                Line::from(Span::styled(
                    "[Enter] to apply",
                    Style::default().fg(Color::DarkGray),
                )),
            ]
        }
    };

    let control_widget = Paragraph::new(control_text).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Value "),
    );
    f.render_widget(control_widget, control_chunks[1]);

    // Current assignment info
    let info = if let Some(fan) = app.selected_fan() {
        let assignment = app
            .assignments
            .iter()
            .find(|a| a.fan_id == fan.id);
        let assign_str = match assignment.map(|a| &a.assignment) {
            Some(FanAssignment::Auto) => "Automatic (BIOS)".to_string(),
            Some(FanAssignment::Manual { pwm }) => {
                format!("Manual: PWM {pwm} ({:.0}%)", *pwm as f64 / 255.0 * 100.0)
            }
            Some(FanAssignment::Curve {
                curve_name,
                temp_sensor_id,
            }) => format!("Curve: {curve_name} tracking {temp_sensor_id}"),
            None => "No assignment (automatic)".to_string(),
        };

        vec![
            Line::from(format!("Fan: {}", fan.id)),
            Line::from(format!(
                "RPM: {}",
                fan.rpm.map(|r| r.to_string()).unwrap_or("-".to_string())
            )),
            Line::from(format!("Current assignment: {assign_str}")),
        ]
    } else {
        vec![Line::from("Select a fan from the list")]
    };

    let info_widget = Paragraph::new(info).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Current Status "),
    );
    f.render_widget(info_widget, control_chunks[2]);
}

fn draw_curve_editor(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(area);

    // Curve list
    let items: Vec<ListItem> = app
        .curves
        .iter()
        .map(|c| {
            let points_str = c
                .points
                .iter()
                .map(|p| format!("{:.0}°→{}", p.temp_c, p.pwm))
                .collect::<Vec<_>>()
                .join(", ");
            ListItem::new(format!("{}: {points_str}", c.name))
        })
        .collect();

    let curve_list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Curves "),
        )
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    f.render_stateful_widget(curve_list, chunks[0], &mut app.curve_list_state.clone());

    // Curve preview (ASCII graph)
    let preview = if let Some(idx) = app.curve_list_state.selected() {
        if let Some(curve) = app.curves.get(idx) {
            render_curve_graph(curve)
        } else {
            vec![Line::from("No curve selected")]
        }
    } else {
        vec![Line::from("Select a curve or press [n] to create one")]
    };

    let preview_widget = Paragraph::new(preview).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Curve Preview "),
    );
    f.render_widget(preview_widget, chunks[1]);
}

fn render_curve_graph(curve: &CurveData) -> Vec<Line<'static>> {
    let graph_height = 12usize;
    let graph_width = 50usize;

    let mut lines = Vec::new();
    lines.push(Line::from(format!("  Curve: {}", curve.name)));
    lines.push(Line::from(""));

    // Build a simple ASCII graph
    let mut grid = vec![vec![' '; graph_width]; graph_height];

    // Map temp range and PWM range to graph coordinates
    let min_temp = curve.points.first().map(|p| p.temp_c).unwrap_or(0.0);
    let max_temp = curve.points.last().map(|p| p.temp_c).unwrap_or(100.0);
    let temp_range = (max_temp - min_temp).max(1.0);

    for x in 0..graph_width {
        let temp = min_temp + (x as f64 / graph_width as f64) * temp_range;
        // Simple interpolation
        let pwm = interpolate_points(&curve.points, temp);
        let y = ((pwm as f64 / 255.0) * (graph_height - 1) as f64).round() as usize;
        let y = y.min(graph_height - 1);
        let row = graph_height - 1 - y; // Invert for display
        grid[row][x] = '█';
    }

    // Draw with axis labels
    for (i, row) in grid.iter().enumerate() {
        let pwm_label = 255 - (i * 255 / (graph_height - 1));
        let row_str: String = row.iter().collect();
        lines.push(Line::from(format!("  {pwm_label:>3} │{row_str}")));
    }

    let axis = format!("      └{}", "─".repeat(graph_width));
    lines.push(Line::from(axis));
    lines.push(Line::from(format!(
        "       {min_temp:.0}°C{:>width$}{max_temp:.0}°C",
        "",
        width = graph_width - 8
    )));

    // Point details
    lines.push(Line::from(""));
    lines.push(Line::from("  Points:"));
    for p in &curve.points {
        let pct = p.pwm as f64 / 255.0 * 100.0;
        lines.push(Line::from(format!(
            "    {:.0}°C → PWM {} ({pct:.0}%)",
            p.temp_c, p.pwm
        )));
    }

    lines
}

fn interpolate_points(points: &[CurvePoint], temp: f64) -> u8 {
    if points.is_empty() {
        return 0;
    }
    if temp <= points[0].temp_c {
        return points[0].pwm;
    }
    let last = &points[points.len() - 1];
    if temp >= last.temp_c {
        return last.pwm;
    }
    for window in points.windows(2) {
        let lo = &window[0];
        let hi = &window[1];
        if temp >= lo.temp_c && temp <= hi.temp_c {
            let range = hi.temp_c - lo.temp_c;
            if range == 0.0 {
                return lo.pwm;
            }
            let frac = (temp - lo.temp_c) / range;
            let pwm = lo.pwm as f64 + frac * (hi.pwm as f64 - lo.pwm as f64);
            return pwm.round().clamp(0.0, 255.0) as u8;
        }
    }
    last.pwm
}

fn draw_curve_edit_overlay(f: &mut Frame, app: &App) {
    let Some(edit) = &app.editing_curve else {
        return;
    };

    let area = centered_rect(60, 70, f.area());
    f.render_widget(Clear, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Name
            Constraint::Min(0),   // Points table
            Constraint::Length(3), // Help
        ])
        .margin(1)
        .split(area);

    let title = if edit.is_new {
        " New Curve "
    } else {
        " Edit Curve "
    };

    let outer_block = Block::default().borders(Borders::ALL).title(title);
    f.render_widget(outer_block, area);

    // Name field
    let name_style = if edit.editing_field == CurveField::Name {
        Style::default().fg(Color::Cyan).bold()
    } else {
        Style::default()
    };
    let name_widget = Paragraph::new(format!("Name: {}", edit.name))
        .style(name_style)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Name [Tab to switch field] "),
        );
    f.render_widget(name_widget, chunks[0]);

    // Points table
    let point_rows: Vec<Row> = edit
        .points
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let pct = p.pwm as f64 / 255.0 * 100.0;
            let style = if i == edit.selected_point {
                Style::default().fg(Color::Cyan).bold()
            } else {
                Style::default()
            };

            let temp_style = if i == edit.selected_point && edit.editing_field == CurveField::Temp {
                Style::default().fg(Color::Yellow).bold()
            } else {
                style
            };
            let pwm_style = if i == edit.selected_point && edit.editing_field == CurveField::Pwm {
                Style::default().fg(Color::Yellow).bold()
            } else {
                style
            };

            Row::new(vec![
                Cell::from(format!("{}", i + 1)).style(style),
                Cell::from(format!("{:.0}°C", p.temp_c)).style(temp_style),
                Cell::from(format!("{} ({pct:.0}%)", p.pwm)).style(pwm_style),
            ])
        })
        .collect();

    let points_table = Table::new(
        point_rows,
        [
            Constraint::Length(4),
            Constraint::Percentage(40),
            Constraint::Percentage(50),
        ],
    )
    .header(
        Row::new(vec!["#", "Temp", "PWM"])
            .style(Style::default().fg(Color::Cyan).bold()),
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Points "),
    );

    f.render_widget(points_table, chunks[1]);

    // Help
    let help = Paragraph::new(
        " [j/k]select  [h/l]adjust  [+]add  [-]remove  [Tab]field  [Enter]save  [Esc]cancel ",
    )
    .style(Style::default().fg(Color::DarkGray))
    .block(Block::default().borders(Borders::ALL));

    f.render_widget(help, chunks[2]);
}

fn draw_config(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),  // Config info
            Constraint::Min(0),     // Current assignments
        ])
        .split(area);

    let config_info = vec![
        Line::from(format!("Config path: {}", app.config_path)),
        Line::from(""),
        Line::from(vec![
            Span::styled("[s]", Style::default().fg(Color::Cyan).bold()),
            Span::raw(" Save current configuration"),
        ]),
        Line::from(vec![
            Span::styled("[r]", Style::default().fg(Color::Cyan).bold()),
            Span::raw(" Reload configuration from disk"),
        ]),
    ];

    let config_widget = Paragraph::new(config_info).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Configuration "),
    );
    f.render_widget(config_widget, chunks[0]);

    // Current assignments
    let assignment_rows: Vec<Row> = app
        .assignments
        .iter()
        .map(|a| {
            let mode = match &a.assignment {
                FanAssignment::Auto => "Auto".to_string(),
                FanAssignment::Manual { pwm } => {
                    format!("Manual (PWM {pwm})")
                }
                FanAssignment::Curve {
                    curve_name,
                    temp_sensor_id,
                } => format!("Curve: {curve_name} → {temp_sensor_id}"),
            };
            Row::new(vec![
                Cell::from(a.fan_id.clone()),
                Cell::from(mode),
            ])
        })
        .collect();

    let assignment_table = Table::new(
        assignment_rows,
        [Constraint::Percentage(40), Constraint::Percentage(60)],
    )
    .header(
        Row::new(vec!["Fan", "Assignment"])
            .style(Style::default().fg(Color::Cyan).bold()),
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Current Fan Assignments "),
    );

    f.render_widget(assignment_table, chunks[1]);
}

/// Utility: create a centered rect.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
