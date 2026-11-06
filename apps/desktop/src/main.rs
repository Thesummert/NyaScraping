mod acquisition;
mod background;
mod file_picker;
mod firmware_browser;
mod firmware_loading;
mod presenter;
mod project;
mod source_font;
mod system_log;
mod tree_sitter;
mod watch_list;
mod waveform_view;

use nya_application::{Application, Command};
use nya_debug_adapter::DebugAdapter;
use nya_domain::{
    firmware::FirmwareBackend, DebugAction, TargetProfile, TextBackend, ToolKind, ToolRegistry,
    WatchRequest,
};
use nya_text_backend::NativeTextBackend;
use slint::ComponentHandle;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc::{self, SyncSender},
    Arc, Mutex,
};

slint::include_modules!();

fn submit(ui: &AppWindow, sender: &SyncSender<Command>, command: Command) {
    if ui.get_busy() || ui.get_file_dialog_open() {
        return;
    }
    let message = match &command {
        Command::RefreshTools => Some("正在扫描调试后端…".to_owned()),
        Command::RefreshTargets => Some("正在加载可选目标…".into()),
        Command::AddPack(_) => Some("正在检查并添加芯片包…".into()),
        Command::RemovePack(_) => Some("正在移除芯片包引用…".into()),
        Command::InstallPack(_) => Some("正在在线安装芯片包，请稍候…".into()),
        Command::InstallTool { tool, version } => Some(format!(
            "正在安装 {} · {}…",
            tool.name(),
            if version.trim().is_empty() {
                "最新稳定版"
            } else {
                version.trim()
            }
        )),
        Command::SelectTool { tool, .. } => Some(format!("正在切换到 {}…", tool.name())),
        Command::UninstallTool { tool, version } => {
            Some(format!("正在卸载 {} · {version}…", tool.name()))
        }
        Command::AddLocalTool { tool, .. } => Some(format!("正在添加本地 {}…", tool.name())),
        Command::RemoveLocalTool { tool, .. } => {
            Some(format!("正在移除本地 {} 引用…", tool.name()))
        }
        _ => None,
    };
    let parsing_firmware = if let Command::OpenFirmware(path) = &command {
        ui.invoke_begin_firmware_parse(path.to_string_lossy().as_ref().into());
        true
    } else {
        false
    };
    match sender.try_send(command) {
        Ok(()) => {
            ui.set_busy(true);
            if let Some(message) = message {
                ui.set_tool_message(message.as_str().into());
                ui.set_status(message.into());
            } else {
                ui.set_status("正在处理…".into());
            }
        }
        Err(error) => {
            if parsing_firmware {
                firmware_loading::finish(ui);
            }
            let message = format!("无法提交任务：{error}");
            ui.invoke_record_system_error(message.as_str().into());
            ui.set_status(message.into());
        }
    }
}

fn tool_kind(index: i32) -> Option<ToolKind> {
    match index {
        0 => Some(ToolKind::OpenOcd),
        1 => Some(ToolKind::PyOcd),
        2 => Some(ToolKind::ProbeRs),
        _ => None,
    }
}

fn target_profile(ui: &AppWindow, standalone_flash: bool) -> Result<TargetProfile, String> {
    let port = if standalone_flash {
        3333
    } else {
        ui.get_debug_port()
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|p| *p != 0)
            .ok_or("GDB 端口应为 1–65535")?
    };
    if ui.get_firmware_path().trim().is_empty() {
        return Err("请选择 ELF/AXF 调试文件".into());
    }
    let interface = ui.get_debug_interface().trim().to_owned();
    if !standalone_flash && ui.get_gdb_path().trim().is_empty() {
        return Err("请填写 GDB 路径".into());
    }
    if standalone_flash || !ui.get_external_server() {
        if tool_kind(ui.get_backend_kind()).is_none() {
            return Err("请先在选择后端页面选择调试后端".into());
        }
        if ui.get_debug_target().trim().is_empty() {
            return Err("请选择调试目标".into());
        }
        if ui.get_backend_kind() == 0 && interface.is_empty() {
            return Err("请选择 OpenOCD Probe 配置".into());
        }
    }
    Ok(TargetProfile {
        name: "桌面调试目标".into(),
        tool: tool_kind(ui.get_backend_kind()).unwrap_or(ToolKind::OpenOcd),
        target: ui.get_debug_target().trim().into(),
        interface: if interface.is_empty() || ui.get_backend_kind() != 0 {
            None
        } else {
            Some(interface)
        },
        transport: if ui.get_backend_kind() == 0 {
            match ui.get_debug_transport_index() {
                0 => None,
                1 => Some("swd".into()),
                2 => Some("jtag".into()),
                3 => Some("hla_swd".into()),
                4 => Some("hla_jtag".into()),
                _ => return Err("请选择有效的 OpenOCD 传输方式".into()),
            }
        } else {
            None
        },
        gdb_port: port,
        gdb_executable: ui.get_gdb_path().trim().into(),
        firmware: ui.get_firmware_path().trim().into(),
    })
}

type WatchUpdates = Arc<Mutex<Option<Vec<WatchRequest>>>>;

fn sync_watches(updates: &WatchUpdates, presenter: &presenter::Presenter) {
    *updates.lock().expect("watch updates lock") = Some(presenter.watch_requests());
}

struct PreparedProject {
    project: project::Project,
    path: std::path::PathBuf,
    firmware: Option<presenter::PreparedEvent>,
    source: Option<nya_domain::Result<nya_domain::Document>>,
}

fn prepare_project(
    path: &std::path::Path,
    backend: &firmware_loading::ProgressBackend,
) -> Result<PreparedProject, String> {
    let project = project::Project::read(path)?;
    let firmware = if project.firmware.is_empty() {
        None
    } else {
        Some(
            backend
                .load(std::path::Path::new(&project.firmware))
                .map_err(|e| format!("无法打开项目固件：{e}"))?,
        )
    };
    backend.preparing_view();
    let firmware = firmware.map(|image| {
        presenter::PreparedEvent::from(nya_application::Event::FirmwareOpened(Box::new(image)))
    });
    let browser = firmware.as_ref().and_then(|event| match event {
        presenter::PreparedEvent::Firmware(view) => Some(&view.browser),
        _ => None,
    });
    let mut checked = watch_list::WatchList::default();
    let missing = checked.restore(&project.watches, browser);
    if missing != 0 {
        return Err(format!(
            "{missing} 个变量无法在当前固件中唯一匹配或恢复波形，请检查项目使用的固件"
        ));
    }
    if project.backend_kind >= 0 && !project.backend_version.is_empty() {
        let tool = tool_kind(project.backend_kind).ok_or("项目中的后端类型无效")?;
        DebugAdapter::new()
            .map_err(|e| e.to_string())?
            .select(tool, &project.backend_version)
            .map_err(|e| format!("无法选择项目后端：{e}"))?;
    }
    let source = (!project.source.is_empty())
        .then(|| NativeTextBackend.load(std::path::Path::new(&project.source)));
    Ok(PreparedProject {
        project,
        path: path.into(),
        firmware,
        source,
    })
}

fn apply_project(
    ui: &AppWindow,
    prepared: PreparedProject,
    presenter: &mut presenter::Presenter,
    updates: &WatchUpdates,
) {
    let PreparedProject {
        project,
        path,
        firmware,
        source,
    } = prepared;
    ui.set_backend_kind(project.backend_kind);
    ui.set_firmware_path(project.firmware.clone().into());
    ui.set_source_path(project.source.clone().into());
    ui.set_gdb_path(project.gdb.into());
    ui.set_debug_target(project.target.into());
    ui.set_debug_interface(project.interface.into());
    ui.set_debug_transport_index(project.transport_index);
    ui.set_debug_port(project.port.into());
    ui.set_external_server(project.external_server);
    ui.set_acquisition_endpoint(project.acquisition_endpoint.into());
    ui.set_refresh_rate_index(if (0..=5).contains(&project.refresh_rate_index) {
        project.refresh_rate_index
    } else {
        0
    });
    ui.invoke_refresh_rate_changed();
    ui.set_live_interval_index(project.live_interval_index);
    ui.set_live_custom_hz(project.live_custom_hz.into());
    if firmware.is_none() {
        presenter.clear_firmware(ui);
    }
    if let Some(firmware) = firmware {
        presenter.apply(ui, firmware);
    }
    let missing = presenter.restore_watches(ui, &project.watches);
    sync_watches(updates, presenter);
    let mut warning = if missing > 0 {
        format!("；{missing} 个变量未能在固件中唯一匹配")
    } else {
        String::new()
    };
    presenter.clear_source_tabs(ui);
    if let Some(source) = source {
        match source {
            Ok(document) => presenter.apply(
                ui,
                presenter::PreparedEvent::from(nya_application::Event::SourceOpened(document)),
            ),
            Err(error) => warning.push_str(&format!("；源码打开失败：{error}")),
        }
    }
    ui.set_project_path(path.to_string_lossy().as_ref().into());
    ui.set_project_dialog_open(false);
    if let Err(error) = project::remember(&path) {
        warning.push_str(&format!("；历史记录保存失败：{error}"));
    }
    ui.set_status(format!("项目已打开{warning}").into());
}

fn save_project(ui: &AppWindow, path: &std::path::Path, presenter: &presenter::Presenter) {
    let project = project::Project::capture(ui, presenter.saved_watches());
    match project.write(path) {
        Ok(()) => {
            ui.set_project_path(path.to_string_lossy().as_ref().into());
            let message = match project::remember(path) {
                Ok(()) => "项目已保存".to_owned(),
                Err(error) => format!("项目已保存，但历史记录保存失败：{error}"),
            };
            ui.set_status(message.into());
        }
        Err(error) => ui.set_status(format!("项目保存失败：{error}").into()),
    }
}

use acquisition::sampling_interval_micros;

#[cfg(target_os = "macos")]
fn set_application_icon() {
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    if let Some(main_thread) = MainThreadMarker::new() {
        let data = NSData::with_bytes(include_bytes!("../assets/icon.png"));
        if let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) {
            // AppKit retains the image and this runs on the main thread.
            unsafe {
                NSApplication::sharedApplication(main_thread).setApplicationIconImage(Some(&image))
            };
        }
    }
}

fn sync_waveform_window(ui: &AppWindow, window: &WaveformWindow) {
    window.set_channels(ui.get_waveform_channels());
    window.set_charts(ui.get_waveform_charts());
    window.set_selected_chart(ui.get_waveform_selected_chart());
    window.set_paths(ui.get_waveform_paths());
    window.set_axis_channels(ui.get_waveform_axis_channels());
    window.set_axis_index(ui.get_waveform_axis_index());
    window.set_y_ticks(ui.get_waveform_y_ticks());
    window.set_time_ticks(ui.get_waveform_time_ticks());
    window.set_zoom_label(ui.get_waveform_zoom_label());
    window.set_active(ui.get_acquisition_active());
    window.set_status(ui.get_acquisition_status());
    window.set_statistics(ui.get_acquisition_statistics());
    if window.get_endpoint() != ui.get_acquisition_endpoint() {
        window.set_endpoint(ui.get_acquisition_endpoint());
    }
    window.set_source_label(ui.get_acquisition_source_label());
    window.set_use_session(ui.get_acquisition_uses_session());
    window.set_session_ready(ui.get_debug_connected());
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let hardware_rendering = project::hardware_rendering_enabled();
    let backend_override = std::env::var_os("SLINT_BACKEND");
    // An explicit environment override also provides a recovery path if GPU startup fails.
    if backend_override.is_none() {
        slint::BackendSelector::new()
            .backend_name("winit".into())
            .renderer_name(
                if hardware_rendering {
                    "skia-wgpu"
                } else {
                    "software"
                }
                .into(),
            )
            .select()?;
    }
    let fps_enabled = project::fps_enabled();
    if fps_enabled && std::env::var_os("SLINT_DEBUG_PERFORMANCE").is_none() {
        std::env::set_var("SLINT_DEBUG_PERFORMANCE", "refresh_lazy,overlay");
    }
    let ui = AppWindow::new()?;
    background::setup(&ui);
    let font = source_font::load();
    ui.set_source_font_family(font.family.into());
    ui.set_source_font_size(font.size);
    let font_save_timer = std::rc::Rc::new(slint::Timer::default());
    let timer = font_save_timer.clone();
    let weak = ui.as_weak();
    ui.on_source_font_changed(move || {
        let weak = weak.clone();
        timer.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_millis(500),
            move || {
                if let Some(ui) = weak.upgrade() {
                    let font = source_font::SourceFont {
                        family: ui.get_source_font_family().to_string(),
                        size: ui.get_source_font_size(),
                    };
                    if let Err(error) = source_font::save(&font) {
                        ui.set_status(format!("无法保存源码字体设置：{error}").into());
                    }
                }
            },
        );
    });
    let weak = ui.as_weak();
    ui.on_apply_source_font(move || {
        if let Some(ui) = weak.upgrade() {
            let font = source_font::SourceFont {
                family: ui.get_source_font_draft().to_string(),
                size: ui.get_source_font_size_draft(),
            }
            .normalized();
            ui.set_source_font_family(font.family.as_str().into());
            ui.set_source_font_size(font.size);
            ui.set_source_font_draft(font.family.into());
            ui.set_source_font_size_draft(font.size);
            ui.invoke_source_font_changed();
            ui.set_settings_message("源码字体已应用".into());
        }
    });
    {
        use slint::winit_030::{
            winit::event::{MouseScrollDelta, WindowEvent},
            EventResult, WinitWindowAccessor,
        };
        let weak = ui.as_weak();
        let mut control = false;
        let mut cursor: Option<(f64, f64)> = None;
        let mut wheel_accumulator = 0.0_f64;
        ui.window().on_winit_window_event(move |window, event| {
            match event {
                WindowEvent::ModifiersChanged(modifiers) => {
                    control = modifiers.state().control_key();
                    if !control {
                        wheel_accumulator = 0.0;
                    }
                }
                WindowEvent::CursorMoved { position, .. } => {
                    let scale = window.scale_factor() as f64;
                    cursor = Some((position.x / scale, position.y / scale));
                }
                WindowEvent::CursorLeft { .. } => {
                    cursor = None;
                    wheel_accumulator = 0.0;
                }
                WindowEvent::Focused(false) => {
                    cursor = None;
                    control = false;
                    wheel_accumulator = 0.0;
                }
                WindowEvent::MouseWheel { delta, .. } if control => {
                    if let (Some(ui), Some(point)) = (weak.upgrade(), cursor) {
                        let rect = (
                            ui.get_source_editor_x(),
                            ui.get_source_editor_y(),
                            ui.get_source_editor_width(),
                            ui.get_source_editor_height(),
                        );
                        if ui.get_current_tab() == 0
                            && !ui.get_settings_open()
                            && !ui.get_project_dialog_open()
                            && !ui.get_file_dialog_open()
                            && source_font::contains_point(point, rect)
                        {
                            let amount = match delta {
                                MouseScrollDelta::LineDelta(_, y) => *y as f64,
                                MouseScrollDelta::PixelDelta(pos) => pos.y / 40.0,
                            };
                            if amount != 0.0 {
                                let steps =
                                    source_font::wheel_steps(&mut wheel_accumulator, amount);
                                if steps != 0 {
                                    let size = (ui.get_source_font_size() + steps)
                                        .clamp(source_font::MIN_SIZE, source_font::MAX_SIZE);
                                    if size != ui.get_source_font_size() {
                                        ui.set_source_font_size(size);
                                        ui.invoke_source_font_changed();
                                    }
                                }
                                return EventResult::PreventDefault;
                            }
                        }
                    }
                    wheel_accumulator = 0.0;
                }
                _ => {}
            }
            EventResult::Propagate
        });
    }
    let tree_settings = tree_sitter::load();
    ui.set_tree_sitter_executable(tree_settings.executable.into());
    ui.set_tree_sitter_config(tree_settings.config.into());
    let highlight_generation = Arc::new(AtomicU64::new(0));
    let weak = ui.as_weak();
    ui.on_highlight_source(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let generation = highlight_generation.fetch_add(1, Ordering::Relaxed) + 1;
        ui.set_source_highlights(Default::default());
        ui.set_source_highlight_status("".into());
        if ui.get_source_selected_tab() < 0 {
            return;
        }
        let path = ui.get_source_title().to_string();
        let text = ui.get_source_text().to_string();
        let settings = tree_sitter::Settings {
            executable: ui.get_tree_sitter_executable().to_string(),
            config: ui.get_tree_sitter_config().to_string(),
        };
        let weak = ui.as_weak();
        let current = highlight_generation.clone();
        std::thread::spawn(move || {
            let result = tree_sitter::highlight(&settings, std::path::Path::new(&path), &text);
            let _ = weak.upgrade_in_event_loop(move |ui| {
                if current.load(Ordering::Relaxed) != generation {
                    return;
                }
                match result {
                    Ok(lines) => {
                        ui.invoke_tree_sitter_event(
                            format!("高亮完成：{path}（{} 行）", lines.len()).into(),
                        );
                        let mut width = 0usize;
                        let rows: Vec<_> = lines
                            .into_iter()
                            .map(|line| {
                                let mut column = 0usize;
                                let spans: Vec<_> = line
                                    .into_iter()
                                    .map(|span| {
                                        let mut text = String::new();
                                        for ch in span.text.chars() {
                                            if ch == '\t' {
                                                let n = 4 - column % 4;
                                                text.push_str(&" ".repeat(n));
                                                column += n;
                                            } else {
                                                text.push(ch);
                                                column += 1;
                                            }
                                        }
                                        let rgb = match span.class.split('.').next().unwrap_or("") {
                                            "comment" => (130, 150, 135),
                                            "keyword" | "operator" => (198, 146, 234),
                                            "string" | "character" => (166, 206, 145),
                                            "number" | "constant" | "boolean" => (226, 172, 120),
                                            "function" | "method" | "constructor" => {
                                                (130, 184, 230)
                                            }
                                            "type" | "namespace" => (121, 202, 192),
                                            "property" | "field" => (226, 202, 139),
                                            _ => (220, 225, 232),
                                        };
                                        HighlightSpan {
                                            text: text.into(),
                                            color: slint::Color::from_rgb_u8(rgb.0, rgb.1, rgb.2),
                                        }
                                    })
                                    .collect();
                                width = width.max(column);
                                HighlightLine {
                                    spans: slint::ModelRc::new(slint::VecModel::from(spans)),
                                }
                            })
                            .collect();
                        ui.set_source_highlight_width(width.min(100_000) as i32);
                        ui.set_source_highlights(slint::ModelRc::new(slint::VecModel::from(rows)));
                        ui.set_source_highlight_status("Tree-sitter 高亮".into());
                    }
                    Err(error) => {
                        ui.invoke_tree_sitter_event(format!("高亮失败：{path}：{error}").into());
                        ui.set_source_highlight_status(format!("普通文本：{error}").into());
                    }
                }
            });
        });
    });
    let weak = ui.as_weak();
    ui.on_tree_sitter_save(move || {
        if let Some(ui) = weak.upgrade() {
            let settings = tree_sitter::Settings {
                executable: ui.get_tree_sitter_executable().to_string(),
                config: ui.get_tree_sitter_config().to_string(),
            };
            let message = match tree_sitter::save(&settings) {
                Ok(()) => {
                    ui.invoke_highlight_source();
                    "Tree-sitter 设置已保存".to_owned()
                }
                Err(e) => format!("无法保存设置：{e}"),
            };
            ui.set_tree_sitter_message(message.into());
        }
    });
    let weak = ui.as_weak();
    ui.on_tree_sitter_detect(move || {
        if let Some(ui) = weak.upgrade() {
            let settings = tree_sitter::Settings {
                executable: ui.get_tree_sitter_executable().to_string(),
                config: ui.get_tree_sitter_config().to_string(),
            };
            ui.set_tree_sitter_busy(true);
            ui.set_tree_sitter_message("正在检测…".into());
            let weak = ui.as_weak();
            std::thread::spawn(move || {
                let result =
                    tree_sitter::detect(&settings).unwrap_or_else(|e| format!("检测失败：{e}"));
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_tree_sitter_busy(false);
                    ui.set_tree_sitter_message(result.into());
                });
            });
        }
    });
    let weak = ui.as_weak();
    ui.on_tree_sitter_languages(move || {
        if let Some(ui) = weak.upgrade() {
            let settings = tree_sitter::Settings {
                executable: ui.get_tree_sitter_executable().to_string(),
                config: ui.get_tree_sitter_config().to_string(),
            };
            ui.set_tree_sitter_busy(true);
            ui.set_tree_sitter_message("正在扫描语言解析器…".into());
            let weak = ui.as_weak();
            std::thread::spawn(move || {
                let result = tree_sitter::languages(&settings).unwrap_or_else(|e| e);
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_tree_sitter_busy(false);
                    ui.set_tree_sitter_message(result.into());
                });
            });
        }
    });
    let weak = ui.as_weak();
    ui.on_tree_sitter_install(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_tree_sitter_busy(true);
            ui.set_tree_sitter_message("正在通过 Cargo 安装，可能需要数分钟…".into());
            let weak = ui.as_weak();
            std::thread::spawn(move || {
                let result = tree_sitter::install().unwrap_or_else(|e| e);
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_tree_sitter_busy(false);
                    ui.set_tree_sitter_message(result.into());
                    ui.invoke_highlight_source();
                });
            });
        }
    });
    ui.set_show_fps(fps_enabled);
    ui.set_hardware_rendering(hardware_rendering);
    let weak_renderer_ui = ui.as_weak();
    ui.on_hardware_rendering_changed(move |enabled| {
        if let Some(ui) = weak_renderer_ui.upgrade() {
            match project::set_hardware_rendering_enabled(enabled) {
                Ok(()) => {
                    let mode = if enabled {
                        "硬件渲染"
                    } else {
                        "软件渲染"
                    };
                    ui.invoke_record_system_error(
                        format!("渲染设置已更改为{mode}，重启后生效").into(),
                    );
                    ui.set_settings_message("渲染方式将在重新启动应用后生效".into());
                }
                Err(error) => {
                    ui.set_hardware_rendering(!enabled);
                    ui.set_settings_message(format!("无法保存渲染设置：{error}").into());
                }
            }
        }
    });
    let weak_fps_ui = ui.as_weak();
    ui.on_show_fps_changed(move |enabled| {
        if let Some(ui) = weak_fps_ui.upgrade() {
            match project::set_fps_enabled(enabled) {
                Ok(()) => ui.set_settings_message("FPS 设置将在重新启动应用后生效".into()),
                Err(error) => {
                    ui.set_show_fps(!enabled);
                    ui.set_settings_message(format!("无法保存 FPS 设置：{error}").into());
                }
            }
        }
    });
    let waveform_window = WaveformWindow::new()?;
    let breakpoint_window = BreakpointWindow::new()?;
    let log_window = SystemLogWindow::new()?;
    let install_window = InstallProgressWindow::new()?;
    let download_window = InstallProgressWindow::new()?;
    {
        let weak_ui = ui.as_weak();
        let weak_waveform = waveform_window.as_weak();
        let weak_breakpoints = breakpoint_window.as_weak();
        let weak_log = log_window.as_weak();
        let weak_install = install_window.as_weak();
        let weak_download = download_window.as_weak();
        ui.on_background_changed(move || {
            if let Some(ui) = weak_ui.upgrade() {
                macro_rules! update_background {
                    ($weak:expr) => {
                        if let Some(window) = $weak.upgrade() {
                            window.set_background_image(ui.get_background_image());
                            window.set_background_opacity(ui.get_background_opacity());
                            window.set_background_active(ui.get_background_active());
                        }
                    };
                }
                update_background!(weak_waveform);
                update_background!(weak_breakpoints);
                update_background!(weak_log);
                update_background!(weak_install);
                update_background!(weak_download);
            }
        });
        ui.invoke_background_changed();
    }
    download_window.set_window_title("固件下载".into());
    download_window.set_message("正在下载固件，请稍候…".into());
    download_window.set_can_cancel(false);
    download_window
        .window()
        .on_close_requested(|| slint::CloseRequestResponse::KeepWindowShown);
    let install_cancel = Arc::new(AtomicBool::new(false));
    let install_progress: nya_debug_adapter::SharedInstallProgress = Default::default();
    let cancel = install_cancel.clone();
    let weak_install = install_window.as_weak();
    install_window.on_cancel(move || {
        cancel.store(true, Ordering::SeqCst);
        if let Some(window) = weak_install.upgrade() {
            window.set_cancelling(true);
        }
    });
    let weak_install = install_window.as_weak();
    install_window.window().on_close_requested(move || {
        if let Some(window) = weak_install.upgrade() {
            window.invoke_cancel();
        }
        slint::CloseRequestResponse::KeepWindowShown
    });
    let progress_timer = slint::Timer::default();
    let weak_install = install_window.as_weak();
    let progress = install_progress.clone();
    let weak_download = download_window.as_weak();
    progress_timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(40),
        move || {
            if let Some(window) = weak_install.upgrade() {
                window.set_phase((window.get_phase() + 0.015) % 1.0);
                if let Ok(value) = progress.lock() {
                    window.set_stage(value.stage.as_str().into());
                    window.set_determinate(value.percent.is_some());
                    window.set_progress(value.percent.unwrap_or(0.0) / 100.0);
                    let size = |n: u64| -> String {
                        if n >= 1024 * 1024 {
                            format!("{:.1} MB", n as f64 / 1048576.0)
                        } else if n >= 1024 {
                            format!("{:.1} KB", n as f64 / 1024.0)
                        } else {
                            format!("{n} B")
                        }
                    };
                    let amount = match (value.downloaded, value.total) {
                        (Some(done), Some(total)) => format!(
                            "{} / {} · {:.0}%",
                            size(done),
                            size(total),
                            value.percent.unwrap_or(0.0)
                        ),
                        (Some(done), None) => format!("已下载 {}", size(done)),
                        _ => value
                            .percent
                            .map(|percent| format!("进度：{percent:.0}%"))
                            .unwrap_or_else(|| "进度：未知".into()),
                    };
                    let speed = value
                        .bytes_per_second
                        .map(|n| format!("{}/s", size(n)))
                        .unwrap_or_else(|| "速度：未知".into());
                    let eta = value
                        .remaining_seconds
                        .map(|n| {
                            if n >= 3600 {
                                format!("约 {} 小时 {} 分钟后", n / 3600, n % 3600 / 60)
                            } else if n >= 60 {
                                format!("约 {} 分 {} 秒后", n / 60, n % 60)
                            } else {
                                format!("约 {n} 秒后")
                            }
                        })
                        .unwrap_or_else(|| "未知".into());
                    let details = format!("{amount} · {speed} · 预计完成：{eta}");
                    window.set_details(details.as_str().into());
                    if let Some(download) = weak_download.upgrade() {
                        download.set_phase(window.get_phase());
                        download.set_stage(value.stage.as_str().into());
                        download.set_determinate(value.percent.is_some());
                        download.set_progress(value.percent.unwrap_or(0.0) / 100.0);
                        download.set_details(details.into());
                    }
                }
            }
        },
    );
    let system_log = std::rc::Rc::new(std::cell::RefCell::new(system_log::SystemLog::default()));
    system_log.borrow_mut().record("应用", &ui.get_status());
    let renderer_message =
        match backend_override {
            Some(value) => {
                format!(
            "渲染方式由 SLINT_BACKEND={} 指定；已保存的设置为{}。实际渲染器取决于后端选择。",
            value.to_string_lossy(),
            if hardware_rendering { "硬件渲染" } else { "软件渲染" }
        )
            }
            None if hardware_rendering => {
                "启动时选择硬件渲染（Skia / WGPU）；等待图形接口初始化。".into()
            }
            None => "启动时选择软件渲染。".into(),
        };
    system_log.borrow_mut().record("渲染", &renderer_message);
    let render_log = system_log.clone();
    let weak_render_log_window = log_window.as_weak();
    if ui
        .window()
        .set_rendering_notifier(move |state, api| {
            if matches!(state, slint::RenderingState::RenderingSetup) {
                let message = format!(
                    "图形接口已初始化：{api:?}；是否使用物理 GPU 取决于系统选择的图形设备。"
                );
                let log = render_log.clone();
                let weak_window = weak_render_log_window.clone();
                // Defer UI updates until the render callback has returned.
                slint::Timer::single_shot(std::time::Duration::ZERO, move || {
                    let mut log = log.borrow_mut();
                    log.record("渲染", &message);
                    if let Some(window) = weak_window.upgrade() {
                        window.set_events(log.text().into());
                    }
                });
            }
        })
        .is_err()
    {
        system_log
            .borrow_mut()
            .record("渲染", "当前渲染器不提供图形接口状态通知。");
    }
    log_window.set_events(system_log.borrow().text().into());
    let log = system_log.clone();
    let weak_log = log_window.as_weak();
    ui.on_status_message_changed(move |message| {
        let mut log = log.borrow_mut();
        log.record("应用", &message);
        if let Some(window) = weak_log.upgrade() {
            window.set_events(log.text().into());
        }
    });
    let log = system_log.clone();
    let weak_log = log_window.as_weak();
    ui.on_record_system_error(move |message| {
        let mut log = log.borrow_mut();
        log.record("应用", &message);
        if let Some(window) = weak_log.upgrade() {
            window.set_events(log.text().into());
        }
    });
    let log = system_log.clone();
    let weak_log = log_window.as_weak();
    ui.on_tree_sitter_event(move |message| {
        let mut log = log.borrow_mut();
        log.record("Tree-sitter", &message);
        if let Some(window) = weak_log.upgrade() {
            window.set_events(log.text().into());
        }
    });
    let log = system_log.clone();
    let weak_log = log_window.as_weak();
    ui.on_acquisition_message_changed(move |message| {
        let mut log = log.borrow_mut();
        log.record("采集", &message);
        if let Some(window) = weak_log.upgrade() {
            window.set_events(log.text().into());
        }
    });
    let weak_log = log_window.as_weak();
    ui.on_debug_output_changed(move |output| {
        if let Some(window) = weak_log.upgrade() {
            window.set_debug_output(output);
        }
    });
    let weak = ui.as_weak();
    let weak_log = log_window.as_weak();
    let log = system_log.clone();
    ui.on_show_system_log(move || {
        if let (Some(ui), Some(window)) = (weak.upgrade(), weak_log.upgrade()) {
            window.set_events(log.borrow().text().into());
            window.set_debug_output(ui.get_debug_log());
            if let Err(error) = window.show() {
                ui.set_status(format!("无法打开系统日志窗口：{error}").into());
            }
        }
    });
    let log = system_log.clone();
    let weak_log = log_window.as_weak();
    log_window.on_clear_events(move || {
        log.borrow_mut().clear();
        if let Some(window) = weak_log.upgrade() {
            window.set_events("".into());
        }
    });
    log_window
        .window()
        .on_close_requested(|| slint::CloseRequestResponse::HideWindow);
    #[cfg(target_os = "macos")]
    set_application_icon();
    let mut adapter = DebugAdapter::new()?;
    adapter.set_install_cancel(install_cancel.clone());
    adapter.set_install_progress(install_progress.clone());
    let (firmware_backend, firmware_progress_timer) = firmware_loading::setup(&ui);
    let mut application = Application::new(adapter, NativeTextBackend, firmware_backend.clone());
    let shutdown = Arc::new(AtomicBool::new(false));
    let worker_shutdown = shutdown.clone();
    // A bounded queue prevents repeated UI actions from accumulating indefinitely.
    let (sender, receiver) = mpsc::sync_channel(8);
    let presenter = Arc::new(Mutex::new(presenter::Presenter::default()));
    let acquisition = presenter
        .lock()
        .expect("presenter lock")
        .acquisition
        .clone();
    acquisition.set_command_sender(sender.clone());
    let sampler = acquisition.clone();
    let acquisition_worker = std::thread::spawn(move || sampler.run());
    let worker_presenter = presenter.clone();
    let watch_updates: WatchUpdates = Arc::new(Mutex::new(None));
    ui.set_project_history(slint::ModelRc::new(slint::VecModel::from(
        project::history()
            .iter()
            .map(|path| slint::SharedString::from(path.to_string_lossy().as_ref()))
            .collect::<Vec<_>>(),
    )));
    let weak = ui.as_weak();
    ui.on_new_project(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_project_dialog_open(false);
        }
    });
    let weak = ui.as_weak();
    let project_presenter = presenter.clone();
    let project_updates = watch_updates.clone();
    let project_backend = firmware_backend.clone();
    ui.on_open_project(move |path| {
        if let Some(ui) = weak.upgrade() {
            if ui.get_debug_connected() || ui.get_busy() {
                return;
            }
            ui.invoke_begin_firmware_parse(path.clone());
            let weak = ui.as_weak();
            let presenter = project_presenter.clone();
            let updates = project_updates.clone();
            let backend = project_backend.clone();
            let path = std::path::PathBuf::from(path.as_str());
            if let Err(error) = std::thread::Builder::new()
                .name("open-project".into())
                .spawn(move || {
                    let result = prepare_project(&path, &backend);
                    let _ = weak.upgrade_in_event_loop(move |ui| match result {
                        Ok(project) => {
                            apply_project(
                                &ui,
                                project,
                                &mut presenter.lock().expect("presenter lock"),
                                &updates,
                            );
                            firmware_loading::finish(&ui);
                            ui.invoke_refresh_tools();
                        }
                        Err(error) => {
                            firmware_loading::finish(&ui);
                            ui.set_status(format!("项目打开失败：{error}").into());
                        }
                    });
                })
            {
                firmware_loading::finish(&ui);
                ui.set_status(format!("无法创建项目加载任务：{error}").into());
            }
        }
    });
    let weak = ui.as_weak();
    ui.on_browse_project(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if ui.get_busy() || ui.get_file_dialog_open() || ui.get_debug_connected() {
            return;
        }
        ui.set_file_dialog_open(true);
        let weak = ui.as_weak();
        if let Err(error) = slint::spawn_local(async move {
            let selected = rfd::AsyncFileDialog::new()
                .set_title("打开项目")
                .add_filter("NyaScraping 项目", &["nyaproj"])
                .pick_file()
                .await;
            if let Some(ui) = weak.upgrade() {
                ui.set_file_dialog_open(false);
                if let Some(file) = selected {
                    if let Some(path) = file.path().to_str() {
                        ui.invoke_open_project(path.into());
                    } else {
                        ui.set_status("项目路径包含无法显示的字符".into());
                    }
                }
            }
        }) {
            ui.set_file_dialog_open(false);
            ui.set_status(format!("无法打开文件选择窗口：{error}").into());
        }
    });
    let weak = ui.as_weak();
    let project_presenter = presenter.clone();
    ui.on_save_project(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let path = ui.get_project_path();
        if !path.is_empty() {
            save_project(
                &ui,
                std::path::Path::new(path.as_str()),
                &project_presenter.lock().expect("presenter lock"),
            );
            return;
        }
        ui.set_file_dialog_open(true);
        let weak = ui.as_weak();
        let project_presenter = project_presenter.clone();
        if let Err(error) = slint::spawn_local(async move {
            let selected = rfd::AsyncFileDialog::new()
                .set_title("保存项目")
                .set_file_name("project.nyaproj")
                .add_filter("NyaScraping 项目", &["nyaproj"])
                .save_file()
                .await;
            if let Some(ui) = weak.upgrade() {
                ui.set_file_dialog_open(false);
                if let Some(file) = selected {
                    save_project(
                        &ui,
                        file.path(),
                        &project_presenter.lock().expect("presenter lock"),
                    );
                }
            }
        }) {
            ui.set_file_dialog_open(false);
            ui.set_status(format!("无法打开文件选择窗口：{error}").into());
        }
    });
    let worker_updates = watch_updates.clone();
    // Intervals use microseconds; zero disables sampling. Settings bypass the command queue.
    let live_interval = Arc::new(AtomicU64::new(0));
    let worker_interval = live_interval.clone();
    let weak = ui.as_weak();
    let weak_install = install_window.as_weak();
    let weak_download = download_window.as_weak();
    let worker = std::thread::spawn(move || {
        let mut last_sample = std::time::Instant::now();
        while !worker_shutdown.load(Ordering::Relaxed) {
            let pending = worker_updates.lock().expect("watch updates lock").take();
            let mut finished_install = false;
            let mut finished_download = false;
            let mut finished_firmware = false;
            let event = if let Some(watches) = pending {
                Some(application.execute(Command::UpdateWatches(watches)))
            } else {
                let interval = worker_interval.load(Ordering::Relaxed);
                let poll_interval = std::time::Duration::from_millis(60);
                // Wake at the sampling deadline while still accepting queued controls first.
                let wait = if interval == 0 {
                    poll_interval
                } else {
                    std::time::Duration::from_micros(interval)
                        .saturating_sub(last_sample.elapsed())
                        .min(poll_interval)
                };
                match receiver.recv_timeout(wait) {
                    Ok(command) => {
                        finished_install = matches!(&command, Command::InstallTool { .. });
                        finished_download = matches!(
                            &command,
                            Command::Debug(DebugAction::Download | DebugAction::Flash { .. })
                        );
                        finished_firmware = matches!(&command, Command::OpenFirmware(_));
                        Some(application.execute(command))
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        let event = application.poll();
                        let interval = worker_interval.load(Ordering::Relaxed);
                        // Deliver state changes before any potentially slow watch reads.
                        if event.is_none()
                            && interval != 0
                            && last_sample.elapsed() >= std::time::Duration::from_micros(interval)
                        {
                            let sample = application.sample_watches();
                            // Schedule from completion, avoiding catch-up bursts on slow probes.
                            last_sample = std::time::Instant::now();
                            sample
                        } else {
                            event
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            };
            let Some(mut event) = event else {
                continue;
            };
            if matches!(&event, nya_application::Event::MemoryRead) {
                // Sampling commands never create per-point UI events. Preserve
                // async breakpoint/disconnect notifications during continuous reads.
                match application.poll() {
                    Some(update) => event = update,
                    None => continue,
                }
            }
            if matches!(&event, nya_application::Event::DebugUpdated { snapshot, .. } if snapshot.watch_sample_error.is_some())
            {
                worker_interval.store(0, Ordering::Relaxed);
            }
            if finished_install {
                let _ = weak_install.upgrade_in_event_loop(|window| {
                    let _ = window.hide();
                });
            }
            if finished_download {
                let _ = weak_download.upgrade_in_event_loop(|window| {
                    let _ = window.hide();
                });
            }
            if finished_firmware {
                firmware_backend.preparing_view();
            }
            let event = presenter::PreparedEvent::from(event);
            let sync = matches!(&event, presenter::PreparedEvent::Firmware(_));
            let updates = worker_updates.clone();
            if weak
                .upgrade_in_event_loop({
                    let presenter = worker_presenter.clone();
                    move |ui| {
                        let mut presenter = presenter.lock().expect("presenter lock");
                        presenter.apply(&ui, event);
                        if sync {
                            sync_watches(&updates, &presenter);
                        }
                        if finished_firmware {
                            ui.set_firmware_parse_progress(1.0);
                            firmware_loading::finish(&ui);
                        }
                    }
                })
                .is_err()
            {
                break;
            }
        }
    });

    let weak = ui.as_weak();
    let tool_sender = sender.clone();
    ui.on_refresh_tools(move || {
        if let Some(ui) = weak.upgrade() {
            submit(&ui, &tool_sender, Command::RefreshTools);
        }
    });

    let weak = ui.as_weak();
    let tool_sender = sender.clone();
    let weak_install = install_window.as_weak();
    let cancel = install_cancel.clone();
    let progress = install_progress.clone();
    ui.on_install_tool(move |kind, version| {
        if let (Some(ui), Some(tool), Some(window)) =
            (weak.upgrade(), tool_kind(kind), weak_install.upgrade())
        {
            if ui.get_busy() || ui.get_file_dialog_open() {
                return;
            }
            cancel.store(false, Ordering::SeqCst);
            if let Ok(mut current) = progress.lock() {
                *current = nya_debug_adapter::InstallProgress {
                    stage: "正在准备安装…".into(),
                    ..Default::default()
                };
            }
            window.set_cancelling(false);
            window.set_message(
                format!(
                    "正在安装 {} · {}…",
                    tool.name(),
                    if version.trim().is_empty() {
                        "最新稳定版"
                    } else {
                        version.trim()
                    }
                )
                .into(),
            );
            submit(
                &ui,
                &tool_sender,
                Command::InstallTool {
                    tool,
                    version: version.to_string(),
                },
            );
            if ui.get_busy() {
                if let Err(error) = window.show() {
                    cancel.store(true, Ordering::SeqCst);
                    ui.set_status(format!("无法打开安装进度窗口：{error}").into());
                }
            }
        }
    });

    let weak = ui.as_weak();
    let tool_sender = sender.clone();
    ui.on_select_tool(move |kind, version| {
        if let (Some(ui), Some(tool)) = (weak.upgrade(), tool_kind(kind)) {
            submit(
                &ui,
                &tool_sender,
                Command::SelectTool {
                    tool,
                    version: version.to_string(),
                },
            );
        }
    });

    let weak = ui.as_weak();
    let tool_sender = sender.clone();
    ui.on_uninstall_tool(move |kind, version| {
        if let (Some(ui), Some(tool)) = (weak.upgrade(), tool_kind(kind)) {
            submit(
                &ui,
                &tool_sender,
                Command::UninstallTool {
                    tool,
                    version: version.to_string(),
                },
            );
        }
    });

    let weak = ui.as_weak();
    let tool_sender = sender.clone();
    ui.on_add_local_tool(move |kind, name, directory| {
        if let (Some(ui), Some(tool)) = (weak.upgrade(), tool_kind(kind)) {
            if !ui.get_debug_connected() {
                submit(
                    &ui,
                    &tool_sender,
                    Command::AddLocalTool {
                        tool,
                        name: name.to_string(),
                        directory: directory.as_str().trim().into(),
                    },
                );
            }
        }
    });
    let weak = ui.as_weak();
    let tool_sender = sender.clone();
    ui.on_remove_local_tool(move |kind, version| {
        if let (Some(ui), Some(tool)) = (weak.upgrade(), tool_kind(kind)) {
            if !ui.get_debug_connected() {
                if let Some(name) = version.as_str().strip_prefix("local:") {
                    submit(
                        &ui,
                        &tool_sender,
                        Command::RemoveLocalTool {
                            tool,
                            name: name.into(),
                        },
                    );
                }
            }
        }
    });
    let weak = ui.as_weak();
    ui.on_browse_local_tool(move || {
        if let Some(ui) = weak.upgrade() {
            file_picker::browse_local_tool(&ui);
        }
    });

    let weak = ui.as_weak();
    let debug_sender = sender.clone();
    let weak_download = download_window.as_weak();
    let download_progress = install_progress.clone();
    ui.on_debug_action(move |kind| {
        if let Some(ui) = weak.upgrade() {
            let action = match kind {
                0 | 8 if kind == 0 || !ui.get_debug_connected() => {
                    match target_profile(&ui, kind == 8) {
                        Ok(profile) => {
                            if kind == 8 {
                                DebugAction::Flash { profile }
                            } else {
                                DebugAction::Connect {
                                    profile,
                                    external_server: ui.get_external_server(),
                                }
                            }
                        }
                        Err(error) => {
                            ui.invoke_record_system_error(error.as_str().into());
                            ui.set_status(error.as_str().into());
                            ui.set_settings_message(error.into());
                            ui.set_settings_tab(2);
                            ui.set_settings_open(true);
                            return;
                        }
                    }
                }
                1 => DebugAction::Disconnect,
                2 => DebugAction::Continue,
                3 => DebugAction::Pause,
                4 => DebugAction::StepOver,
                5 => DebugAction::StepInto,
                6 => DebugAction::StepOut,
                7 => DebugAction::Reset,
                8 => DebugAction::Download,
                9 => DebugAction::Refresh,
                _ => return,
            };
            if kind == 8 {
                if ui.get_busy() || ui.get_file_dialog_open() {
                    return;
                }
                if let Ok(mut progress) = download_progress.lock() {
                    *progress = nya_debug_adapter::InstallProgress {
                        stage: "正在准备下载…".into(),
                        ..Default::default()
                    };
                }
            }
            submit(&ui, &debug_sender, Command::Debug(action));
            if kind == 8 && ui.get_busy() {
                if let Some(window) = weak_download.upgrade() {
                    if let Err(error) = window.show() {
                        ui.set_status(format!("无法打开固件下载窗口：{error}").into());
                    }
                }
            }
        }
    });
    let weak = ui.as_weak();
    let debug_sender = sender.clone();
    ui.on_add_breakpoint(move |location| {
        if let Some(ui) = weak.upgrade() {
            submit(
                &ui,
                &debug_sender,
                Command::Debug(DebugAction::AddBreakpoint(location.to_string())),
            );
        }
    });
    let weak = ui.as_weak();
    let debug_sender = sender.clone();
    ui.on_remove_breakpoint(move |id| {
        if let Some(ui) = weak.upgrade() {
            submit(
                &ui,
                &debug_sender,
                Command::Debug(DebugAction::RemoveBreakpoint(id.to_string())),
            );
        }
    });
    let weak = ui.as_weak();
    let source_presenter = presenter.clone();
    let debug_sender = sender.clone();
    ui.on_toggle_source_breakpoint(move |line| {
        if let Some(ui) = weak.upgrade() {
            let action = source_presenter
                .lock()
                .expect("presenter lock")
                .source_breakpoint_action(line);
            if let Some(action) = action {
                submit(&ui, &debug_sender, Command::Debug(action));
            }
        }
    });
    let weak = ui.as_weak();
    let weak_window = breakpoint_window.as_weak();
    ui.on_show_breakpoints(move || {
        if let (Some(ui), Some(window)) = (weak.upgrade(), weak_window.upgrade()) {
            window.set_breakpoints(ui.get_breakpoints());
            window
                .set_can_edit((ui.get_debug_halted() || ui.get_debug_running()) && !ui.get_busy());
            window.set_can_edit_pending(!ui.get_busy());
            if let Err(error) = window.show() {
                ui.set_status(format!("无法打开断点记录窗口：{error}").into());
            }
        }
    });
    let weak = ui.as_weak();
    let debug_sender = sender.clone();
    breakpoint_window.on_set_enabled(move |id, enabled| {
        if let Some(ui) = weak.upgrade() {
            submit(
                &ui,
                &debug_sender,
                Command::Debug(DebugAction::SetBreakpointEnabled {
                    id: id.to_string(),
                    enabled,
                }),
            );
        }
    });
    let weak = ui.as_weak();
    let debug_sender = sender.clone();
    breakpoint_window.on_remove(move |id| {
        if let Some(ui) = weak.upgrade() {
            submit(
                &ui,
                &debug_sender,
                Command::Debug(DebugAction::RemoveBreakpoint(id.to_string())),
            );
        }
    });
    breakpoint_window
        .window()
        .on_close_requested(|| slint::CloseRequestResponse::HideWindow);
    let breakpoint_timer = slint::Timer::default();
    let weak = ui.as_weak();
    let weak_window = breakpoint_window.as_weak();
    breakpoint_timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(100),
        move || {
            if let (Some(ui), Some(window)) = (weak.upgrade(), weak_window.upgrade()) {
                window.set_breakpoints(ui.get_breakpoints());
                window.set_can_edit(
                    (ui.get_debug_halted() || ui.get_debug_running()) && !ui.get_busy(),
                );
                window.set_can_edit_pending(!ui.get_busy());
            }
        },
    );
    let weak = ui.as_weak();
    let debug_sender = sender.clone();
    ui.on_select_frame(move |level| {
        if let (Some(ui), Ok(level)) = (weak.upgrade(), u32::try_from(level)) {
            submit(
                &ui,
                &debug_sender,
                Command::Debug(DebugAction::SelectFrame(level)),
            );
        }
    });

    let weak = ui.as_weak();
    let browse_sender = sender.clone();
    ui.on_browse_source(move || {
        if let Some(ui) = weak.upgrade() {
            file_picker::browse(&ui, browse_sender.clone(), file_picker::FileKind::Source);
        }
    });

    let weak = ui.as_weak();
    let target_sender = sender.clone();
    ui.on_refresh_targets(move || {
        if let Some(ui) = weak.upgrade() {
            if !ui.get_debug_connected() && !ui.get_external_server() {
                submit(&ui, &target_sender, Command::RefreshTargets);
            }
        }
    });
    let weak = ui.as_weak();
    let target_presenter = presenter.clone();
    ui.on_search_targets(move |query| {
        if let Some(ui) = weak.upgrade() {
            target_presenter
                .lock()
                .expect("presenter lock")
                .search_targets(&ui, &query);
        }
    });
    let weak = ui.as_weak();
    let probe_presenter = presenter.clone();
    ui.on_search_probes(move |query| {
        if let Some(ui) = weak.upgrade() {
            probe_presenter
                .lock()
                .expect("presenter lock")
                .search_probes(&ui, &query);
        }
    });
    let weak = ui.as_weak();
    let probe_sender = sender.clone();
    ui.on_browse_probe(move || {
        if let Some(ui) = weak.upgrade() {
            file_picker::browse(
                &ui,
                probe_sender.clone(),
                file_picker::FileKind::ProbeConfig,
            );
        }
    });
    let weak = ui.as_weak();
    let pack_sender = sender.clone();
    ui.on_add_pack(move || {
        if let Some(ui) = weak.upgrade() {
            file_picker::browse(&ui, pack_sender.clone(), file_picker::FileKind::Pack);
        }
    });
    let weak = ui.as_weak();
    let pack_sender = sender.clone();
    ui.on_remove_pack(move |path| {
        if let Some(ui) = weak.upgrade() {
            if !ui.get_debug_connected() && !ui.get_external_server() && ui.get_backend_kind() == 1
            {
                submit(&ui, &pack_sender, Command::RemovePack(path.as_str().into()));
            }
        }
    });
    let weak = ui.as_weak();
    let pack_sender = sender.clone();
    ui.on_install_pack(move |part| {
        if let Some(ui) = weak.upgrade() {
            if !ui.get_debug_connected() && !ui.get_external_server() && ui.get_backend_kind() == 1
            {
                submit(&ui, &pack_sender, Command::InstallPack(part.to_string()));
            }
        }
    });

    let weak = ui.as_weak();
    let browse_sender = sender.clone();
    ui.on_browse_firmware(move || {
        if let Some(ui) = weak.upgrade() {
            file_picker::browse(&ui, browse_sender.clone(), file_picker::FileKind::Firmware);
        }
    });

    let weak = ui.as_weak();
    let source_sender = sender.clone();
    ui.on_open_source(move || {
        if let Some(ui) = weak.upgrade() {
            let path = ui.get_source_path();
            if path.trim().is_empty() {
                ui.set_status("请输入源码文件路径".into());
                return;
            }
            submit(
                &ui,
                &source_sender,
                Command::OpenSource(path.as_str().into()),
            );
        }
    });

    let weak = ui.as_weak();
    let firmware_sender = sender.clone();
    ui.on_open_firmware(move || {
        if let Some(ui) = weak.upgrade() {
            let path = ui.get_firmware_path();
            if path.trim().is_empty() {
                ui.set_status("请输入编译产物文件路径".into());
                return;
            }
            submit(
                &ui,
                &firmware_sender,
                Command::OpenFirmware(path.as_str().into()),
            );
        }
    });

    let weak = ui.as_weak();
    let source_presenter = presenter.clone();
    ui.on_select_source_tab(move |index| {
        if let Some(ui) = weak.upgrade() {
            source_presenter
                .lock()
                .expect("presenter lock")
                .select_source_tab(&ui, index);
        }
    });
    let weak = ui.as_weak();
    let source_presenter = presenter.clone();
    ui.on_close_source_tab(move |index| {
        if let Some(ui) = weak.upgrade() {
            source_presenter
                .lock()
                .expect("presenter lock")
                .close_source_tab(&ui, index);
        }
    });
    let weak = ui.as_weak();
    let source_presenter = presenter.clone();
    let source_sender = sender.clone();
    ui.on_open_tree_source(move |path| {
        if let Some(ui) = weak.upgrade() {
            if ui.get_busy() || ui.get_file_dialog_open() {
                return;
            }
            ui.set_current_tab(0);
            if !source_presenter
                .lock()
                .expect("presenter lock")
                .activate_source_path(&ui, path.as_str())
            {
                submit(
                    &ui,
                    &source_sender,
                    Command::OpenSource(path.as_str().into()),
                );
            }
        }
    });
    let weak = ui.as_weak();
    let tree_presenter = presenter.clone();
    ui.on_select_firmware_file(move |index| {
        if let Some(ui) = weak.upgrade() {
            tree_presenter
                .lock()
                .expect("presenter lock")
                .select_firmware_file(&ui, index);
        }
    });
    let weak = ui.as_weak();
    let tree_presenter = presenter.clone();
    let callback_updates = watch_updates.clone();
    ui.on_select_firmware_node(move |index| {
        if let Some(ui) = weak.upgrade() {
            let mut presenter = tree_presenter.lock().expect("presenter lock");
            presenter.select_firmware(&ui, index);
            sync_watches(&callback_updates, &presenter);
        }
    });

    let weak = ui.as_weak();
    let tree_presenter = presenter.clone();
    let callback_updates = watch_updates.clone();
    ui.on_add_firmware_variable(move |index| {
        if let Some(ui) = weak.upgrade() {
            let mut presenter = tree_presenter.lock().expect("presenter lock");
            presenter.add_firmware_variable(&ui, index);
            sync_watches(&callback_updates, &presenter);
        }
    });

    let weak = ui.as_weak();
    let search_presenter = presenter.clone();
    ui.on_search_firmware(move |query| {
        if let Some(ui) = weak.upgrade() {
            search_presenter
                .lock()
                .expect("presenter lock")
                .search_firmware(&ui, &query);
        }
    });

    let weak = ui.as_weak();
    let write_presenter = presenter.clone();
    let write_sender = sender.clone();
    ui.on_write_watch(move |entry, node, value| {
        if let Some(ui) = weak.upgrade() {
            let request = write_presenter
                .lock()
                .expect("presenter lock")
                .write_request(entry, node);
            if let Some(watch) = request {
                submit(
                    &ui,
                    &write_sender,
                    Command::Debug(DebugAction::WriteWatch {
                        watch,
                        value: value.to_string(),
                    }),
                );
            }
        }
    });

    let weak = ui.as_weak();
    let watch_presenter = presenter.clone();
    let callback_updates = watch_updates.clone();
    ui.on_add_expression(move |expression| {
        if let Some(ui) = weak.upgrade() {
            let mut presenter = watch_presenter.lock().expect("presenter lock");
            presenter.add_expression(&ui, &expression);
            sync_watches(&callback_updates, &presenter);
        }
    });

    let weak = ui.as_weak();
    let tree_presenter = presenter.clone();
    ui.on_show_functions(move |show| {
        if let Some(ui) = weak.upgrade() {
            tree_presenter
                .lock()
                .expect("presenter lock")
                .show_functions(&ui, show);
        }
    });

    let weak = ui.as_weak();
    let watch_presenter = presenter.clone();
    let callback_updates = watch_updates.clone();
    ui.on_toggle_watch(move |entry, node| {
        if let Some(ui) = weak.upgrade() {
            let mut presenter = watch_presenter.lock().expect("presenter lock");
            presenter.toggle_watch(&ui, entry, node);
            sync_watches(&callback_updates, &presenter);
        }
    });

    let weak = ui.as_weak();
    let callback_updates = watch_updates.clone();
    let remove_presenter = presenter.clone();
    ui.on_remove_watch(move |index| {
        if let Some(ui) = weak.upgrade() {
            let mut presenter = remove_presenter.lock().expect("presenter lock");
            presenter.remove_watch(&ui, index);
            sync_watches(&callback_updates, &presenter);
        }
    });

    let weak = ui.as_weak();
    let live_presenter = presenter.clone();
    ui.on_live_refresh(move || {
        if let Some(ui) = weak.upgrade() {
            let configured =
                sampling_interval_micros(ui.get_live_interval_index(), &ui.get_live_custom_hz());
            let interval = match configured {
                Ok(interval) => {
                    ui.set_live_frequency_error("".into());
                    if ui.get_live_enabled() {
                        interval
                    } else {
                        0
                    }
                }
                Err(error) => {
                    ui.set_live_frequency_error(error.into());
                    ui.set_live_enabled(false);
                    0
                }
            };
            live_interval.store(interval, Ordering::Relaxed);
            live_presenter
                .lock()
                .expect("presenter lock")
                .live_refresh_changed(&ui);
            ui.set_watch_sample_status(if interval == 0 {
                "实时刷新已关闭".into()
            } else {
                "无暂停刷新：与波形共用当前后端采集".into()
            });
        }
    });

    let weak = ui.as_weak();
    let plot_presenter = presenter.clone();
    let callback_updates = watch_updates.clone();
    ui.on_plot_variable(move |index| {
        if let Some(ui) = weak.upgrade() {
            let mut presenter = plot_presenter.lock().expect("presenter lock");
            presenter.plot_variable(&ui, index);
            sync_watches(&callback_updates, &presenter);
        }
    });
    let weak = ui.as_weak();
    let plot_presenter = presenter.clone();
    let callback_updates = watch_updates.clone();
    ui.on_toggle_waveform(move |index, node| {
        if let Some(ui) = weak.upgrade() {
            let mut presenter = plot_presenter.lock().expect("presenter lock");
            presenter.toggle_waveform(&ui, index, node);
            sync_watches(&callback_updates, &presenter);
        }
    });
    let weak = ui.as_weak();
    let chart_presenter = presenter.clone();
    ui.on_add_waveform_chart(move || {
        if let Some(ui) = weak.upgrade() {
            chart_presenter
                .lock()
                .expect("presenter lock")
                .add_waveform_chart(&ui);
        }
    });
    let weak = ui.as_weak();
    let chart_presenter = presenter.clone();
    ui.on_select_waveform_chart(move |index| {
        if let Some(ui) = weak.upgrade() {
            chart_presenter
                .lock()
                .expect("presenter lock")
                .select_waveform_chart(&ui, index);
        }
    });
    let weak = ui.as_weak();
    let chart_presenter = presenter.clone();
    ui.on_move_waveform_channel(move |watch, node, chart| {
        if let Some(ui) = weak.upgrade() {
            chart_presenter
                .lock()
                .expect("presenter lock")
                .move_waveform_channel(&ui, watch, node, chart);
        }
    });
    let weak = ui.as_weak();
    let chart_presenter = presenter.clone();
    ui.on_clear_waveform_history(move || {
        if let Some(ui) = weak.upgrade() {
            chart_presenter
                .lock()
                .expect("presenter lock")
                .clear_waveform_history(&ui);
        }
    });
    let weak = ui.as_weak();
    let chart_presenter = presenter.clone();
    ui.on_export_waveform_csv(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let csv = chart_presenter
            .lock()
            .expect("presenter lock")
            .waveform_csv(&ui);
        ui.set_file_dialog_open(true);
        let weak = ui.as_weak();
        if let Err(error) = slint::spawn_local(async move {
            let selected = rfd::AsyncFileDialog::new()
                .set_title("导出当前图表")
                .set_file_name("waveform.csv")
                .add_filter("CSV", &["csv"])
                .save_file()
                .await;
            if let Some(ui) = weak.upgrade() {
                ui.set_file_dialog_open(false);
                if let Some(file) = selected {
                    match std::fs::write(file.path(), csv) {
                        Ok(()) => ui.set_acquisition_status("波形 CSV 已导出".into()),
                        Err(error) => {
                            ui.set_acquisition_status(format!("导出波形失败：{error}").into())
                        }
                    }
                }
            }
        }) {
            ui.set_file_dialog_open(false);
            ui.set_acquisition_status(format!("无法打开文件选择窗口：{error}").into());
        }
    });
    let weak = ui.as_weak();
    let chart_presenter = presenter.clone();
    ui.on_zoom_waveform(move |up, control, alt| {
        if let Some(ui) = weak.upgrade() {
            chart_presenter
                .lock()
                .expect("presenter lock")
                .zoom_waveform(&ui, up, control, alt);
        }
    });
    let weak = ui.as_weak();
    let chart_presenter = presenter.clone();
    ui.on_select_waveform_axis(move |index| {
        if let Some(ui) = weak.upgrade() {
            chart_presenter
                .lock()
                .expect("presenter lock")
                .select_waveform_axis(&ui, index);
        }
    });
    let weak = ui.as_weak();
    let plot_presenter = presenter.clone();
    ui.on_start_acquisition(move || {
        if let Some(ui) = weak.upgrade() {
            plot_presenter
                .lock()
                .expect("presenter lock")
                .start_acquisition(&ui);
            if ui.get_acquisition_active() {
                ui.invoke_live_refresh();
            }
        }
    });
    let weak = ui.as_weak();
    let plot_presenter = presenter.clone();
    ui.on_stop_acquisition(move || {
        if let Some(ui) = weak.upgrade() {
            plot_presenter
                .lock()
                .expect("presenter lock")
                .stop_acquisition(&ui);
        }
    });
    let weak_ui = ui.as_weak();
    let weak_window = waveform_window.as_weak();
    ui.on_toggle_waveform_window(move |detached| {
        if let (Some(ui), Some(window)) = (weak_ui.upgrade(), weak_window.upgrade()) {
            if detached {
                sync_waveform_window(&ui, &window);
                window.set_detached(true);
                match window.show() {
                    Ok(()) => ui.set_waveform_detached(true),
                    Err(error) => {
                        ui.set_waveform_detached(false);
                        ui.set_acquisition_status(format!("无法打开波形窗口：{error}").into())
                    }
                }
            } else {
                ui.set_waveform_detached(false);
                let _ = window.hide();
            }
        }
    });
    let weak = ui.as_weak();
    waveform_window.window().on_close_requested(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_waveform_detached(false);
        }
        slint::CloseRequestResponse::HideWindow
    });
    let weak_window = waveform_window.as_weak();
    let weak_breakpoint_window = breakpoint_window.as_weak();
    let weak_log_window = log_window.as_weak();
    let weak_ui_for_font = ui.as_weak();
    ui.window().on_close_requested(move || {
        if let Some(ui) = weak_ui_for_font.upgrade() {
            let font = source_font::SourceFont {
                family: ui.get_source_font_family().to_string(),
                size: ui.get_source_font_size(),
            };
            let _ = source_font::save(&font);
        }
        if let Some(window) = weak_window.upgrade() {
            let _ = window.hide();
        }
        if let Some(window) = weak_breakpoint_window.upgrade() {
            let _ = window.hide();
        }
        if let Some(window) = weak_log_window.upgrade() {
            let _ = window.hide();
        }
        slint::CloseRequestResponse::HideWindow
    });
    let weak = ui.as_weak();
    waveform_window.on_toggle_detached(move |value| {
        if let Some(ui) = weak.upgrade() {
            ui.invoke_toggle_waveform_window(value);
        }
    });
    let weak = ui.as_weak();
    waveform_window.on_zoom(move |up, control, alt| {
        if let Some(ui) = weak.upgrade() {
            ui.invoke_zoom_waveform(up, control, alt);
        }
    });
    let weak = ui.as_weak();
    waveform_window.on_select_axis(move |index| {
        if let Some(ui) = weak.upgrade() {
            ui.invoke_select_waveform_axis(index);
        }
    });
    let weak = ui.as_weak();
    waveform_window.on_add_chart(move || {
        if let Some(ui) = weak.upgrade() {
            ui.invoke_add_waveform_chart();
        }
    });
    let weak = ui.as_weak();
    waveform_window.on_select_chart(move |index| {
        if let Some(ui) = weak.upgrade() {
            ui.invoke_select_waveform_chart(index);
        }
    });
    let weak = ui.as_weak();
    waveform_window.on_move_channel(move |watch, node, chart| {
        if let Some(ui) = weak.upgrade() {
            ui.invoke_move_waveform_channel(watch, node, chart);
        }
    });
    let weak = ui.as_weak();
    waveform_window.on_export_csv(move || {
        if let Some(ui) = weak.upgrade() {
            ui.invoke_export_waveform_csv();
        }
    });
    let weak = ui.as_weak();
    waveform_window.on_clear_history(move || {
        if let Some(ui) = weak.upgrade() {
            ui.invoke_clear_waveform_history();
        }
    });
    let weak = ui.as_weak();
    waveform_window.on_start(move || {
        if let Some(ui) = weak.upgrade() {
            ui.invoke_start_acquisition();
        }
    });
    let weak = ui.as_weak();
    waveform_window.on_stop(move || {
        if let Some(ui) = weak.upgrade() {
            ui.invoke_stop_acquisition();
        }
    });
    let weak = ui.as_weak();
    waveform_window.on_remove(move |watch, node| {
        if let Some(ui) = weak.upgrade() {
            ui.invoke_toggle_waveform(watch, node);
        }
    });
    let weak = ui.as_weak();
    waveform_window.on_endpoint_edited(move |endpoint| {
        if let Some(ui) = weak.upgrade() {
            if ui.get_acquisition_endpoint() != endpoint {
                ui.set_acquisition_endpoint(endpoint);
            }
        }
    });
    let waveform_timer = std::rc::Rc::new(slint::Timer::default());
    let timer = waveform_timer.clone();
    let weak = ui.as_weak();
    let weak_window = waveform_window.as_weak();
    let refresh_presenter = presenter.clone();
    ui.on_refresh_rate_changed(move || {
        let Some(ui) = weak.upgrade() else { return };
        let micros = match ui.get_refresh_rate_index() {
            1 => 10_000,
            2 => 16_667,
            3 => 33_333,
            4 => 100_000,
            5 => 500_000,
            _ => 1_000,
        };
        let weak = weak.clone();
        let weak_window = weak_window.clone();
        let presenter = refresh_presenter.clone();
        timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_micros(micros),
            move || {
                if let Some(ui) = weak.upgrade() {
                    presenter
                        .lock()
                        .expect("presenter lock")
                        .refresh_acquisition_tick(&ui);
                    if ui.get_waveform_detached() {
                        if let Some(window) = weak_window.upgrade() {
                            sync_waveform_window(&ui, &window);
                        }
                    }
                }
            },
        );
    });
    ui.invoke_refresh_rate_changed();

    ui.invoke_refresh_tools();
    let result = ui.run();
    install_cancel.store(true, Ordering::SeqCst);
    progress_timer.stop();
    firmware_progress_timer.stop();
    waveform_timer.stop();
    breakpoint_timer.stop();
    acquisition.shutdown();
    acquisition_worker
        .join()
        .map_err(|_| std::io::Error::other("Acquisition worker panicked"))?;
    shutdown.store(true, Ordering::Relaxed);
    // Dropping the UI releases callback senders; closing the channel stops the worker.
    drop(ui);
    drop(sender);
    worker
        .join()
        .map_err(|_| std::io::Error::other("Application worker panicked"))?;
    result?;
    Ok(())
}

#[cfg(test)]
mod sampling_tests {
    use super::sampling_interval_micros;

    #[test]
    fn custom_frequency_preserves_precision_and_rejects_invalid_rates() {
        assert_eq!(sampling_interval_micros(5, "").unwrap(), 500_000);
        assert_eq!(sampling_interval_micros(10, " 3 ").unwrap(), 333_333);
        assert_eq!(sampling_interval_micros(10, "7.5").unwrap(), 133_333);
        assert_eq!(sampling_interval_micros(10, "0.1").unwrap(), 10_000_000);
        assert_eq!(sampling_interval_micros(10, "1000").unwrap(), 1000);
        for invalid in ["", "no", "0", "-1", "NaN", "inf", "0.01", "1001"] {
            assert!(sampling_interval_micros(10, invalid).is_err());
        }
        assert!(sampling_interval_micros(-1, "1").is_err());
        assert!(sampling_interval_micros(11, "1").is_err());
    }
}

#[cfg(test)]
mod project_loading_tests {
    use super::*;

    #[test]
    fn project_preparation_validates_firmware_and_saved_watches_before_applying() {
        let dir = tempfile::tempdir().unwrap();
        let firmware = dir.path().join("firmware.elf");
        std::fs::write(
            &firmware,
            include_bytes!("../../../crates/firmware-backend/tests/fixtures/arm-dwarf4.axf"),
        )
        .unwrap();
        let path = dir.path().join("project.nyaproj");
        let mut project = project::Project {
            format: 1,
            firmware: firmware.to_string_lossy().into_owned(),
            source: dir.path().join("missing.c").to_string_lossy().into_owned(),
            gdb: "gdb".into(),
            target: "".into(),
            interface: "".into(),
            transport_index: 0,
            port: "3333".into(),
            external_server: true,
            backend_kind: -1,
            backend_version: "".into(),
            acquisition_endpoint: "".into(),
            refresh_rate_index: 0,
            live_interval_index: 5,
            live_custom_hz: "10".into(),
            watches: Vec::new(),
        };
        project.write(&path).unwrap();
        let backend = firmware_loading::ProgressBackend::default();
        let prepared = prepare_project(&path, &backend).unwrap();
        assert!(matches!(
            prepared.firmware,
            Some(presenter::PreparedEvent::Firmware(_))
        ));
        assert!(
            prepared.source.unwrap().is_err(),
            "missing source is a recoverable warning"
        );

        project.watches.push(watch_list::SavedWatch {
            expression: "missing_variable".into(),
            scope: Some("missing_scope".into()),
            function: None,
            waveform: false,
            waveform_members: Vec::new(),
        });
        project.write(&path).unwrap();
        assert!(
            matches!(prepare_project(&path, &backend), Err(error) if error.contains("1 个变量无法"))
        );
        project.watches.clear();
        project.firmware = dir
            .path()
            .join("missing.elf")
            .to_string_lossy()
            .into_owned();
        project.write(&path).unwrap();
        assert!(
            matches!(prepare_project(&path, &backend), Err(error) if error.contains("无法打开项目固件"))
        );
    }
}
