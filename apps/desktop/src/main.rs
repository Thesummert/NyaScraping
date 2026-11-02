mod firmware_browser;
mod presenter;
mod watch_list;

use nya_application::{Application, Command};
use nya_debug_adapter::DebugAdapter;
use nya_domain::{DebugAction, TargetProfile, ToolKind, WatchRequest};
use nya_firmware_backend::NativeFirmwareBackend;
use nya_text_backend::NativeTextBackend;
use slint::ComponentHandle;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, SyncSender},
    Arc, Mutex,
};

slint::include_modules!();

fn submit(ui: &AppWindow, sender: &SyncSender<Command>, command: Command) {
    if ui.get_busy() {
        return;
    }
    let message = match &command {
        Command::RefreshTools => Some("正在扫描调试后端…".to_owned()),
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
    Ok(TargetProfile {
        name: "桌面调试目标".into(),
        tool: ToolKind::OpenOcd,
        target: ui.get_debug_target().trim().into(),
        interface: if interface.is_empty() {
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
    let weak = ui.as_weak();
    let presenter = Arc::new(Mutex::new(presenter::Presenter::default()));
    let worker_presenter = presenter.clone();
    let watch_updates: WatchUpdates = Arc::new(Mutex::new(None));
    let worker_updates = watch_updates.clone();
    let worker = std::thread::spawn(move || {
        while !worker_shutdown.load(Ordering::Relaxed) {
            let pending = worker_updates.lock().expect("watch updates lock").take();
            let event = if let Some(watches) = pending {
                Some(application.execute(Command::UpdateWatches(watches)))
            } else {
                match receiver.recv_timeout(std::time::Duration::from_millis(60)) {
                    Ok(command) => Some(application.execute(command)),
                    Err(mpsc::RecvTimeoutError::Timeout) => application.poll(),
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            };
            let Some(event) = event else {
                continue;
            };
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
                        ui.set_status(error.into());
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
    ui.on_remove_watch(move |index| {
        if let Some(ui) = weak.upgrade() {
            let mut presenter = presenter.lock().expect("presenter lock");
            presenter.remove_watch(&ui, index);
            sync_watches(&callback_updates, &presenter);
        }
    });

    ui.invoke_refresh_tools();
    let result = ui.run();
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
