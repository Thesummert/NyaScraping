use crate::{submit, AppWindow};
use nya_application::Command;
use rfd::AsyncFileDialog;
use slint::ComponentHandle;
use std::{path::Path, sync::mpsc::SyncSender};

#[derive(Clone, Copy)]
pub enum FileKind {
    Firmware,
    Source,
    Pack,
    ProbeConfig,
}

pub fn browse_local_tool(ui: &AppWindow) {
    if ui.get_busy() || ui.get_file_dialog_open() || ui.get_debug_connected() {
        return;
    }
    let mut dialog = AsyncFileDialog::new()
        .set_title("选择本地调试器目录")
        .set_parent(&ui.window().window_handle());
    let current = ui.get_local_tool_directory();
    if Path::new(current.as_str()).is_dir() {
        dialog = dialog.set_directory(current.as_str());
    }
    ui.set_file_dialog_open(true);
    let weak = ui.as_weak();
    if let Err(error) = slint::spawn_local(async move {
        let selected = dialog.pick_folder().await;
        let Some(ui) = weak.upgrade() else {
            return;
        };
        ui.set_file_dialog_open(false);
        if let Some(folder) = selected {
            if let Some(path) = folder.path().to_str() {
                if !ui.get_debug_connected() {
                    ui.set_local_tool_directory(path.into());
                }
            } else {
                ui.set_status("所选目录路径包含无法显示的字符".into());
            }
        }
    }) {
        ui.set_file_dialog_open(false);
        ui.set_status(format!("无法打开目录选择窗口：{error}").into());
    }
}

pub fn browse(ui: &AppWindow, sender: SyncSender<Command>, kind: FileKind) {
    if ui.get_busy()
        || ui.get_file_dialog_open()
        || (matches!(
            kind,
            FileKind::Firmware | FileKind::Pack | FileKind::ProbeConfig
        ) && ui.get_debug_connected())
        || (matches!(kind, FileKind::Pack)
            && (ui.get_backend_kind() != 1 || ui.get_external_server()))
        || (matches!(kind, FileKind::ProbeConfig)
            && (ui.get_backend_kind() != 0 || ui.get_external_server()))
    {
        return;
    }

    let (dialog, current_path) = match kind {
        FileKind::Firmware => (
            AsyncFileDialog::new().set_title("打开编译产物").add_filter(
                "编译产物 (AXF / ELF / OUT / O / SO / EXE / DLL / DYLIB)",
                &["axf", "elf", "out", "o", "so", "exe", "dll", "dylib"],
            ),
            ui.get_firmware_path(),
        ),
        FileKind::Source => (
            AsyncFileDialog::new().set_title("打开源码文件").add_filter(
                "源码与文本文件",
                &["c", "h", "cpp", "hpp", "cc", "cxx", "s", "asm", "rs", "txt"],
            ),
            ui.get_source_path(),
        ),
        FileKind::Pack => (
            AsyncFileDialog::new()
                .set_title("添加 CMSIS 芯片包")
                .add_filter("CMSIS 芯片包", &["pack"]),
            "".into(),
        ),
        FileKind::ProbeConfig => (
            AsyncFileDialog::new()
                .set_title("选择 OpenOCD Probe 配置")
                .add_filter("OpenOCD 配置", &["cfg"]),
            ui.get_debug_interface(),
        ),
    };
    // Content detection also accepts extensionless files and unusual extensions.
    let mut dialog = dialog
        .add_filter("所有文件", &["*"])
        .set_parent(&ui.window().window_handle());
    let current_path = Path::new(current_path.as_str());
    if current_path.is_dir() {
        dialog = dialog.set_directory(current_path);
    } else if let Some(parent) = current_path.parent().filter(|path| path.is_dir()) {
        dialog = dialog.set_directory(parent);
    }

    ui.set_file_dialog_open(true);
    let weak = ui.as_weak();
    if let Err(error) = slint::spawn_local(async move {
        let selected = dialog.pick_file().await;
        let Some(ui) = weak.upgrade() else {
            return;
        };
        ui.set_file_dialog_open(false);
        let Some(file) = selected else {
            return;
        };
        let path = file.path();
        let Some(display_path) = path.to_str() else {
            ui.set_status("所选文件路径包含无法显示的字符，请重命名后重试".into());
            return;
        };
        let command = match kind {
            FileKind::ProbeConfig => {
                if !ui.get_debug_connected()
                    && ui.get_backend_kind() == 0
                    && !ui.get_external_server()
                {
                    ui.set_debug_interface(display_path.into());
                    ui.set_status("已选择 OpenOCD Probe 配置".into());
                }
                return;
            }
            FileKind::Pack => {
                if ui.get_debug_connected() || ui.get_backend_kind() != 1 {
                    return;
                }
                Command::AddPack(path.into())
            }
            FileKind::Firmware => {
                if ui.get_debug_connected() {
                    return;
                }
                ui.set_firmware_path(display_path.into());
                Command::OpenFirmware(path.into())
            }
            FileKind::Source => {
                ui.set_source_path(display_path.into());
                Command::OpenSource(path.into())
            }
        };
        submit(&ui, &sender, command);
    }) {
        ui.set_file_dialog_open(false);
        ui.set_status(format!("无法打开文件选择窗口：{error}").into());
    }
}
