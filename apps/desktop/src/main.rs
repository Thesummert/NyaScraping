mod acquisition;
mod file_picker;
mod firmware_browser;
mod presenter;
mod project;
mod watch_list;
mod waveform_view;

use nya_application::{Application, Command};
use nya_debug_adapter::DebugAdapter;
use nya_domain::{
    firmware::FirmwareBackend, DebugAction, TargetProfile, TextBackend, ToolKind, ToolRegistry,
    WatchRequest,
};
use nya_firmware_backend::NativeFirmwareBackend;
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
        Err(error) => ui.set_status(format!("无法提交任务：{error}").into()),
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

fn target_profile(ui: &AppWindow) -> Result<TargetProfile, String> {
    let port = ui
        .get_debug_port()
        .trim()
        .parse::<u16>()
        .ok()
        .filter(|p| *p != 0)
        .ok_or("GDB 端口应为 1–65535")?;
    if ui.get_firmware_path().trim().is_empty() {
        return Err("请选择 ELF/AXF 调试文件".into());
    }
    let interface = ui.get_debug_interface().trim().to_owned();
    if ui.get_gdb_path().trim().is_empty() {
        return Err("请填写 GDB 路径".into());
    }
    if !ui.get_external_server() {
        if tool_kind(ui.get_backend_kind()).is_none() {
            return Err("请先在后端设置中选择调试后端".into());
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
        gdb_port: port,
        gdb_executable: ui.get_gdb_path().trim().into(),
        firmware: ui.get_firmware_path().trim().into(),
    })
}

type WatchUpdates = Arc<Mutex<Option<Vec<WatchRequest>>>>;

fn sync_watches(updates: &WatchUpdates, presenter: &presenter::Presenter) {
    *updates.lock().expect("watch updates lock") = Some(presenter.watch_requests());
}

fn open_project(
    ui: &AppWindow,
    path: &std::path::Path,
    presenter: &mut presenter::Presenter,
    updates: &WatchUpdates,
) -> Result<(), String> {
    let project = project::Project::read(path)?;
    let firmware = if project.firmware.is_empty() {
        None
    } else {
        Some(
            NativeFirmwareBackend
                .load(std::path::Path::new(&project.firmware))
                .map_err(|e| format!("无法打开项目固件：{e}"))?,
        )
    };
    let browser = firmware.as_ref().map(|image| {
        firmware_browser::FirmwareBrowser::new(
            &image.debug,
            image.little_endian,
            image.kind == "Executable",
        )
    });
    let mut checked = watch_list::WatchList::default();
    let missing = checked.restore(&project.watches, browser.as_ref());
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
    ui.set_backend_kind(project.backend_kind);
    ui.set_firmware_path(project.firmware.clone().into());
    ui.set_source_path(project.source.clone().into());
    ui.set_gdb_path(project.gdb.into());
    ui.set_debug_target(project.target.into());
    ui.set_debug_interface(project.interface.into());
    ui.set_debug_port(project.port.into());
    ui.set_external_server(project.external_server);
    ui.set_acquisition_endpoint(project.acquisition_endpoint.into());
    ui.set_live_interval_index(project.live_interval_index);
    ui.set_live_custom_hz(project.live_custom_hz.into());
    if firmware.is_none() {
        presenter.clear_firmware(ui);
    }
    if let Some(firmware) = firmware {
        presenter.apply(
            ui,
            presenter::PreparedEvent::from(nya_application::Event::FirmwareOpened(Box::new(
                firmware,
            ))),
        );
    }
    let missing = presenter.restore_watches(ui, &project.watches);
    sync_watches(updates, presenter);
    let mut warning = if missing > 0 {
        format!("；{missing} 个变量未能在固件中唯一匹配")
    } else {
        String::new()
    };
    ui.set_source_title("尚未打开文件".into());
    ui.set_source_meta("".into());
    ui.set_source_text("打开一个源码文件以开始浏览。".into());
    if !project.source.is_empty() {
        match NativeTextBackend.load(std::path::Path::new(&project.source)) {
            Ok(document) => presenter.apply(
                ui,
                presenter::PreparedEvent::from(nya_application::Event::SourceOpened(document)),
            ),
            Err(error) => warning.push_str(&format!("；源码打开失败：{error}")),
        }
    }
    ui.set_project_path(path.to_string_lossy().as_ref().into());
    ui.set_project_dialog_open(false);
    if let Err(error) = project::remember(path) {
        warning.push_str(&format!("；历史记录保存失败：{error}"));
    }
    ui.set_status(format!("项目已打开{warning}").into());
    Ok(())
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ui = AppWindow::new()?;
    let mut application = Application::new(
        DebugAdapter::new()?,
        NativeTextBackend,
        NativeFirmwareBackend,
    );
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
    ui.on_open_project(move |path| {
        if let Some(ui) = weak.upgrade() {
            if ui.get_debug_connected() || ui.get_busy() {
                return;
            }
            let mut presenter = project_presenter.lock().expect("presenter lock");
            match open_project(
                &ui,
                std::path::Path::new(path.as_str()),
                &mut presenter,
                &project_updates,
            ) {
                Ok(()) => ui.invoke_refresh_tools(),
                Err(error) => ui.set_status(format!("项目打开失败：{error}").into()),
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
    let worker = std::thread::spawn(move || {
        let mut last_sample = std::time::Instant::now();
        while !worker_shutdown.load(Ordering::Relaxed) {
            let pending = worker_updates.lock().expect("watch updates lock").take();
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
                    Ok(command) => Some(application.execute(command)),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        let event = application.poll();
                        let interval = worker_interval.load(Ordering::Relaxed);
                        if interval != 0
                            && last_sample.elapsed() >= std::time::Duration::from_micros(interval)
                        {
                            let mut sample = application.sample_watches();
                            if let (
                                Some(nya_application::Event::DebugUpdated {
                                    snapshot: sampled,
                                    ..
                                }),
                                Some(nya_application::Event::DebugUpdated {
                                    snapshot: polled, ..
                                }),
                            ) = (&mut sample, &event)
                            {
                                if sampled.state == polled.state && sampled.source.is_none() {
                                    sampled.source = polled.source.clone();
                                }
                            }
                            // Schedule from completion, avoiding catch-up bursts on slow probes.
                            last_sample = std::time::Instant::now();
                            sample.or(event)
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
    ui.on_install_tool(move |kind, version| {
        if let (Some(ui), Some(tool)) = (weak.upgrade(), tool_kind(kind)) {
            submit(
                &ui,
                &tool_sender,
                Command::InstallTool {
                    tool,
                    version: version.to_string(),
                },
            );
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
    ui.on_debug_action(move |kind| {
        if let Some(ui) = weak.upgrade() {
            let action = match kind {
                0 => match target_profile(&ui) {
                    Ok(profile) => DebugAction::Connect {
                        profile,
                        external_server: ui.get_external_server(),
                    },
                    Err(error) => {
                        ui.set_status(error.as_str().into());
                        ui.set_settings_message(error.into());
                        ui.set_settings_tab(1);
                        ui.set_settings_open(true);
                        return;
                    }
                },
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
            submit(&ui, &debug_sender, Command::Debug(action));
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
    ui.on_toggle_waveform(move |index| {
        if let Some(ui) = weak.upgrade() {
            let mut presenter = plot_presenter.lock().expect("presenter lock");
            presenter.toggle_waveform(&ui, index);
            sync_watches(&callback_updates, &presenter);
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
    let weak = ui.as_weak();
    let refresh_presenter = presenter.clone();
    let waveform_timer = slint::Timer::default();
    waveform_timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(33),
        move || {
            if let Some(ui) = weak.upgrade() {
                refresh_presenter
                    .lock()
                    .expect("presenter lock")
                    .refresh_acquisition(&ui);
            }
        },
    );

    ui.invoke_refresh_tools();
    let result = ui.run();
    waveform_timer.stop();
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
