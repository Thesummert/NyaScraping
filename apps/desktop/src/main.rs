mod firmware_browser;
mod presenter;
mod watch_list;

use nya_application::{Application, Command};
use nya_debug_adapter::DebugAdapter;
use nya_firmware_backend::NativeFirmwareBackend;
use nya_text_backend::NativeTextBackend;
use slint::ComponentHandle;
use std::sync::{
    mpsc::{self, SyncSender},
    Arc, Mutex,
};

slint::include_modules!();

fn submit(ui: &AppWindow, sender: &SyncSender<Command>, command: Command) {
    match sender.try_send(command) {
        Ok(()) => {
            ui.set_busy(true);
            ui.set_status("正在处理…".into());
        }
        Err(error) => ui.set_status(format!("无法提交任务：{error}").into()),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ui = AppWindow::new()?;
    let mut application = Application::new(
        DebugAdapter::new()?,
        NativeTextBackend,
        NativeFirmwareBackend,
    );
    // A bounded queue prevents repeated UI actions from accumulating indefinitely.
    let (sender, receiver) = mpsc::sync_channel(8);
    let weak = ui.as_weak();
    let presenter = Arc::new(Mutex::new(presenter::Presenter::default()));
    let worker_presenter = presenter.clone();
    let worker = std::thread::spawn(move || {
        while let Ok(command) = receiver.recv() {
            let event = presenter::PreparedEvent::from(application.execute(command));
            if weak
                .upgrade_in_event_loop({
                    let presenter = worker_presenter.clone();
                    move |ui| presenter.lock().expect("presenter lock").apply(&ui, event)
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
    ui.on_select_firmware_node(move |index| {
        if let Some(ui) = weak.upgrade() {
            tree_presenter
                .lock()
                .expect("presenter lock")
                .select_firmware(&ui, index);
        }
    });

    let weak = ui.as_weak();
    let watch_presenter = presenter.clone();
    ui.on_add_selected_variable(move || {
        if let Some(ui) = weak.upgrade() {
            watch_presenter
                .lock()
                .expect("presenter lock")
                .add_selected_variable(&ui);
        }
    });

    let weak = ui.as_weak();
    let watch_presenter = presenter.clone();
    ui.on_add_expression(move |expression| {
        if let Some(ui) = weak.upgrade() {
            watch_presenter
                .lock()
                .expect("presenter lock")
                .add_expression(&ui, &expression);
        }
    });

    let weak = ui.as_weak();
    ui.on_remove_watch(move |index| {
        if let Some(ui) = weak.upgrade() {
            presenter
                .lock()
                .expect("presenter lock")
                .remove_watch(&ui, index);
        }
    });

    ui.invoke_refresh_tools();
    let result = ui.run();
    // Dropping the UI releases callback senders; closing the channel stops the worker.
    drop(ui);
    drop(sender);
    worker
        .join()
        .map_err(|_| std::io::Error::other("Application worker panicked"))?;
    result?;
    Ok(())
}
