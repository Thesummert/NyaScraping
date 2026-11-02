use crate::{
    firmware_browser::FirmwareBrowser, watch_list::WatchList, AppWindow, BreakpointRow,
    FirmwareTreeRow, FrameRow, ToolRow, WatchRow,
};
use nya_application::Event;
use nya_domain::{
    firmware::FirmwareImage, DebugSnapshot, Document, EvaluatedWatch, SessionState, ToolKind,
    ToolState, WatchRequest,
};
use slint::{ModelRc, VecModel};
use std::{collections::HashMap, rc::Rc};

#[derive(Default)]
pub struct Presenter {
    browser: Option<FirmwareBrowser>,
    watches: WatchList,
    watch_revision: u64,
    values: HashMap<String, EvaluatedWatch>,
}

/// Plain Rust data prepared on the worker, before entering the Slint event loop.
pub enum PreparedEvent {
    Tools(ToolState, String),
    Debug(DebugSnapshot, bool),
    Source(Document),
    Firmware(Box<FirmwareView>),
    Failed(String),
}

pub struct FirmwareView {
    title: String,
    metadata: String,
    report: String,
    details: String,
    status: String,
    browser: FirmwareBrowser,
}

impl From<Event> for PreparedEvent {
    fn from(event: Event) -> Self {
        match event {
            Event::ToolsUpdated { state, message } => Self::Tools(state, message),
            Event::DebugUpdated { snapshot, completes_command } => Self::Debug(snapshot, completes_command),
            Event::SourceOpened(document) => Self::Source(document),
            Event::Failed(error) => Self::Failed(error),
            Event::FirmwareOpened(image) => Self::Firmware(Box::new(FirmwareView {
                title: image.path.display().to_string(),
                metadata: format!(
                    "{} · {} · {} 位 · {} · {} · 入口 0x{:X} · {} bytes",
                    image.format, image.architecture, image.address_bits,
                    if image.little_endian { "小端" } else { "大端" },
                    image.kind, image.entry, image.file_size,
                ),
                report: firmware_report(&image),
                details: format!(
                    "{} 个源文件，{} 个函数条目，{} 个变量/参数。\n\n点击目录或文件可展开；点击变量直接添加监视。函数默认隐藏，可通过开关显示。\n\n{}",
                    image.debug.files.len(), image.debug.functions.len(), image.debug.variables.len(), image.debug.warnings.join("\n"),
                ),
                status: format!(
                    "解析完成：{} 个源文件，{} 个函数，{} 个变量/参数{}",
                    image.debug.files.len(), image.debug.functions.len(), image.debug.variables.len(),
                    if image.debug.warnings.is_empty() { "" } else { "（存在调试信息提示，见详情）" },
                ),
                browser: FirmwareBrowser::new(&image.debug),
            })),
        }
    }
}

impl Presenter {
    pub fn apply(&mut self, ui: &AppWindow, event: PreparedEvent) {
        if !matches!(&event, PreparedEvent::Debug(_, false)) {
            ui.set_busy(false);
        }
        match event {
            PreparedEvent::Tools(state, message) => {
                let active = state.installations.iter().find(|installation| {
                    Some(installation.tool) == state.backend && installation.selected
                });
                let summary = match (state.backend, active) {
                    (_, Some(tool)) => format!("{} · {}", tool.tool.name(), tool.version),
                    (Some(tool), None) => format!("{}（不可用，请选择已安装版本）", tool.name()),
                    (None, _) => "尚未选择调试后端".into(),
                };
                let rows: Vec<_> = state
                    .installations
                    .iter()
                    .enumerate()
                    .map(|(index, tool)| {
                        let first_system = !state.installations[..index].iter().any(|previous| {
                            previous.tool == tool.tool && previous.version == "system"
                        });
                        ToolRow {
                            kind: match tool.tool {
                                ToolKind::OpenOcd => 0,
                                ToolKind::PyOcd => 1,
                                ToolKind::ProbeRs => 2,
                            },
                            name: tool.tool.name().into(),
                            version: tool.version.as_str().into(),
                            path: tool.executable.to_string_lossy().as_ref().into(),
                            selected: tool.selected,
                            active: Some(tool.tool) == state.backend && tool.selected,
                            managed: tool.version != "system",
                            selectable: tool.version != "system" || first_system,
                        }
                    })
                    .collect();
                ui.set_tools(ModelRc::from(Rc::new(VecModel::from(rows))));
                ui.set_backend_summary(summary.into());
                ui.set_tool_message(message.as_str().into());
                ui.set_status(message.into());
            }
            PreparedEvent::Debug(snapshot, _) => {
                ui.set_debug_halted(snapshot.state == SessionState::Halted);
                ui.set_debug_running(snapshot.state == SessionState::Running);
                ui.set_debug_connected(matches!(
                    snapshot.state,
                    SessionState::Halted
                        | SessionState::Running
                        | SessionState::Attaching
                        | SessionState::ServerReady
                ));
                ui.set_debug_state(
                    match snapshot.state {
                        SessionState::Disconnected => "未连接",
                        SessionState::Starting => "服务启动中",
                        SessionState::ServerReady => "服务就绪",
                        SessionState::Attaching => "连接中",
                        SessionState::Halted => "已暂停",
                        SessionState::Running => "运行中",
                        SessionState::Faulted => "连接异常",
                    }
                    .into(),
                );
                self.values = snapshot
                    .values
                    .into_iter()
                    .map(|v| (v.id.clone(), v))
                    .collect();
                self.refresh_watches(ui);
                let frames: Vec<_> = snapshot
                    .frames
                    .iter()
                    .map(|frame| FrameRow {
                        level: frame.level as i32,
                        selected: frame.level == snapshot.selected_frame,
                        label: format!(
                            "#{} {} · {}:{} {}",
                            frame.level, frame.function, frame.file, frame.line, frame.address
                        )
                        .into(),
                    })
                    .collect();
                ui.set_stack_frames(ModelRc::from(Rc::new(VecModel::from(frames))));
                let breakpoints: Vec<_> = snapshot
                    .breakpoints
                    .into_iter()
                    .map(|bp| BreakpointRow {
                        id: bp.id.as_str().into(),
                        label: format!(
                            "#{} {}{}",
                            bp.id,
                            bp.location,
                            if bp.enabled { "" } else { "（禁用）" }
                        )
                        .into(),
                    })
                    .collect();
                ui.set_breakpoints(ModelRc::from(Rc::new(VecModel::from(breakpoints))));
                ui.set_execution_location(
                    snapshot
                        .frames
                        .iter()
                        .find(|f| f.level == snapshot.selected_frame)
                        .map(|f| format!("当前帧 #{} {}:{}", f.level, f.file, f.line))
                        .unwrap_or_default()
                        .into(),
                );
                if let Some(document) = snapshot.source {
                    ui.set_source_title(document.path.display().to_string().into());
                    ui.set_source_path(document.path.display().to_string().into());
                    ui.set_source_text(document.text.into());
                    ui.set_source_meta(document.encoding.into());
                }
                ui.set_debug_log(snapshot.log.into());
                if !snapshot.message.is_empty() {
                    ui.set_status(snapshot.message.into());
                }
            }
            PreparedEvent::Source(document) => {
                ui.set_source_title(document.path.display().to_string().into());
                ui.set_source_meta(
                    format!(
                        "{}  ·  {:?}{}",
                        document.encoding,
                        document.line_ending,
                        if document.mixed_endings {
                            "  ·  混合换行"
                        } else {
                            ""
                        }
                    )
                    .into(),
                );
                ui.set_source_text(document.text.into());
                ui.set_status("源码已通过 Rust 文本后端读取".into());
            }
            PreparedEvent::Firmware(view) => {
                ui.set_firmware_title(view.title.into());
                ui.set_firmware_meta(view.metadata.into());
                ui.set_firmware_report(view.report.into());
                ui.set_firmware_details(view.details.into());
                let mut browser = view.browser;
                browser.set_show_functions(ui.get_functions_visible());
                self.browser = Some(browser);
                self.watches.reset_firmware();
                self.invalidate_watches(ui);
                self.refresh_tree(ui);
                ui.set_firmware_selected_node(-1);
                ui.set_status(view.status.into());
            }
            PreparedEvent::Failed(error) => {
                let message = format!("操作失败：{error}");
                ui.set_tool_message(message.as_str().into());
                ui.set_status(message.into());
            }
        }
    }

    pub fn select_firmware(&mut self, ui: &AppWindow, index: i32) {
        let structure_changed = usize::try_from(index)
            .ok()
            .is_some_and(|index| self.browser.as_ref().is_some_and(|b| b.is_branch(index)));
        if let Some(details) = usize::try_from(index)
            .ok()
            .and_then(|index| self.browser.as_mut()?.select(index))
        {
            ui.set_firmware_details(details.into());
            ui.set_firmware_selected_node(index);
            if structure_changed {
                self.refresh_tree(ui);
            }
            self.add_selected_variable(ui);
        }
    }

    pub fn show_functions(&mut self, ui: &AppWindow, show: bool) {
        ui.set_functions_visible(show);
        if let Some(browser) = &mut self.browser {
            browser.set_show_functions(show);
            self.refresh_tree(ui);
            ui.set_firmware_selected_node(-1);
        }
    }

    pub fn toggle_watch(&mut self, ui: &AppWindow, entry: i32, node: i32) {
        if let (Ok(entry), Ok(node)) = (usize::try_from(entry), usize::try_from(node)) {
            self.watches.toggle(entry, node);
            self.invalidate_watches(ui);
        }
    }

    pub fn add_selected_variable(&mut self, ui: &AppWindow) {
        if let Some(variable) = self.browser.as_ref().and_then(|b| b.selected_variable()) {
            let added = self.watches.add_variable(variable);
            if added {
                self.invalidate_watches(ui);
            }
            ui.set_status(
                if added {
                    "变量已添加到右侧监视列表"
                } else {
                    "该变量已在监视列表中"
                }
                .into(),
            );
        }
    }

    pub fn add_expression(&mut self, ui: &AppWindow, expression: &str) {
        let added = self.watches.add_expression(expression);
        if added {
            self.invalidate_watches(ui);
        }
        ui.set_status(
            if added {
                "表达式已添加，目标暂停后求值"
            } else {
                "表达式为空或已在监视列表中"
            }
            .into(),
        );
    }

    pub fn remove_watch(&mut self, ui: &AppWindow, index: i32) {
        if usize::try_from(index).is_ok_and(|index| self.watches.remove(index)) {
            self.invalidate_watches(ui);
            ui.set_status("已移除监视变量".into());
        }
    }

    fn invalidate_watches(&mut self, ui: &AppWindow) {
        self.watch_revision += 1;
        self.values.clear();
        self.refresh_watches(ui);
    }

    pub fn watch_requests(&self) -> Vec<WatchRequest> {
        self.watches
            .rows()
            .into_iter()
            .map(|row| WatchRequest {
                id: format!("{}:{}:{}", self.watch_revision, row.entry, row.node),
                expression: row.expression,
                function: row.function,
            })
            .collect()
    }

    fn refresh_watches(&self, ui: &AppWindow) {
        let rows: Vec<_> = self
            .watches
            .rows()
            .into_iter()
            .map(|entry| {
                let id = format!("{}:{}:{}", self.watch_revision, entry.entry, entry.node);
                let runtime = self.values.get(&id).map(|v| &v.value);
                WatchRow {
                    expression: entry.name.into(),
                    entry_index: entry.entry as i32,
                    node_index: entry.node as i32,
                    depth: entry.depth as i32,
                    branch: entry.branch,
                    expanded: entry.expanded,
                    type_name: runtime
                        .filter(|v| !v.type_name.is_empty())
                        .map(|v| v.type_name.as_str())
                        .unwrap_or(&entry.type_name)
                        .into(),
                    scope: entry.scope.into(),
                    value: runtime
                        .map(|v| v.display_value.as_str())
                        .unwrap_or("—")
                        .into(),
                    error: runtime
                        .and_then(|v| v.error.as_deref())
                        .unwrap_or_default()
                        .into(),
                }
            })
            .collect();
        ui.set_watches(ModelRc::from(Rc::new(VecModel::from(rows))));
    }

    fn refresh_tree(&mut self, ui: &AppWindow) {
        let rows: Vec<_> = self
            .browser
            .as_mut()
            .map(|b| b.rows())
            .unwrap_or_default()
            .into_iter()
            .map(|row| FirmwareTreeRow {
                label: row.label.into(),
                type_name: row.type_name.into(),
                depth: row.depth as i32,
                branch: row.branch,
                expanded: row.expanded,
            })
            .collect();
        ui.set_firmware_tree(ModelRc::from(Rc::new(VecModel::from(rows))));
    }
}

fn firmware_report(image: &FirmwareImage) -> String {
    use std::fmt::Write;
    // Bound the desktop text model; the backend retains the complete metadata.
    const PREVIEW_LIMIT: usize = 1000;
    let mut report = format!(
        "DWARF 调试信息节：{}（函数、变量与声明位置见文件树）\n符号地址为静态地址；可重定位文件中的地址通常为节内偏移。\n每类最多预览 {PREVIEW_LIMIT} 条。符号表可能为空，或包含同名的普通/动态符号。\n",
        if image.has_debug_info { "存在" } else { "未检测到" },
    );
    report.push_str("\n段 / Segments（虚拟地址、内存大小、文件偏移、文件大小、对齐、标志）\n");
    for segment in image.segments.iter().take(PREVIEW_LIMIT) {
        let _ = writeln!(
            report,
            "0x{:016X}  0x{:X}  0x{:X}  0x{:X}  0x{:X}  {}",
            segment.address,
            segment.memory_size,
            segment.file_offset,
            segment.file_size,
            segment.alignment,
            segment.flags
        );
    }
    report.push_str("\n节 / Sections（索引、名称、地址、大小、文件范围、类型、标志）\n");
    for section in image.sections.iter().take(PREVIEW_LIMIT) {
        let range = section.file_range.map_or_else(
            || "无文件数据".into(),
            |(offset, size)| format!("0x{offset:X}+0x{size:X}"),
        );
        let _ = writeln!(
            report,
            "[{}] {:?}  0x{:016X}  0x{:X}  {}  {}  {}",
            section.index,
            section.name,
            section.address,
            section.size,
            range,
            section.kind,
            section.flags
        );
    }
    report.push_str("\n符号 / Symbols（地址、大小、类型、作用域、节、标志、名称）\n");
    for symbol in image.symbols.iter().take(PREVIEW_LIMIT) {
        let _ = writeln!(
            report,
            "0x{:016X}  0x{:X}  {}  {}  {:?}  {}{}{}  {:?}",
            symbol.address,
            symbol.size,
            symbol.kind,
            symbol.scope,
            symbol.section_index,
            if symbol.undefined { "undefined " } else { "" },
            if symbol.weak { "weak " } else { "" },
            if symbol.dynamic { "dynamic" } else { "" },
            symbol.name
        );
    }
    report
}
