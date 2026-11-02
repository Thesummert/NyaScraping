use crate::{
    firmware_browser::FirmwareBrowser, watch_list::WatchList, AppWindow, FirmwareTreeRow, WatchRow,
};
use nya_application::Event;
use nya_domain::{firmware::FirmwareImage, Document, ToolInstallation};
use slint::{ModelRc, SharedString, VecModel};
use std::rc::Rc;

#[derive(Default)]
pub struct Presenter {
    browser: Option<FirmwareBrowser>,
    watches: WatchList,
}

/// Plain Rust data prepared on the worker, before entering the Slint event loop.
pub enum PreparedEvent {
    Tools(Vec<ToolInstallation>),
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
            Event::ToolsDiscovered(tools) => Self::Tools(tools),
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
                    "{} 个源文件，{} 个函数条目，{} 个变量/参数。\n\n点击目录、文件或函数可展开；点击变量查看类型、源码位置及存储位置。\n\n{}",
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
        ui.set_busy(false);
        match event {
            PreparedEvent::Tools(installations) => {
                let count = installations.len();
                let rows: Vec<SharedString> = installations
                    .into_iter()
                    .map(|tool| {
                        format!(
                            "{}  ·  {}{}\n{}",
                            tool.tool.name(),
                            tool.version,
                            if tool.selected {
                                "  ·  当前选择"
                            } else {
                                ""
                            },
                            tool.executable.display()
                        )
                        .into()
                    })
                    .collect();
                ui.set_tools(ModelRc::from(Rc::new(VecModel::from(rows))));
                ui.set_status(format!("扫描完成，找到 {count} 个工具安装位置").into());
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
                self.browser = Some(view.browser);
                self.watches.reset_firmware();
                self.refresh_watches(ui);
                ui.set_can_add_variable(false);
                self.refresh_tree(ui);
                ui.set_firmware_selected_node(-1);
                ui.set_status(view.status.into());
            }
            PreparedEvent::Failed(error) => ui.set_status(format!("操作失败：{error}").into()),
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
            ui.set_can_add_variable(
                self.browser
                    .as_ref()
                    .and_then(|b| b.selected_variable())
                    .is_some(),
            );
            if structure_changed {
                self.refresh_tree(ui);
            }
        }
    }

    pub fn add_selected_variable(&mut self, ui: &AppWindow) {
        if let Some(variable) = self.browser.as_ref().and_then(|b| b.selected_variable()) {
            let added = self.watches.add_variable(variable);
            self.refresh_watches(ui);
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
        self.refresh_watches(ui);
        ui.set_status(
            if added {
                "表达式已添加，等待调试会话求值"
            } else {
                "表达式为空或已在监视列表中"
            }
            .into(),
        );
    }

    pub fn remove_watch(&mut self, ui: &AppWindow, index: i32) {
        if usize::try_from(index).is_ok_and(|index| self.watches.remove(index)) {
            self.refresh_watches(ui);
            ui.set_status("已移除监视变量".into());
        }
    }

    fn refresh_watches(&self, ui: &AppWindow) {
        let rows: Vec<_> = self
            .watches
            .entries
            .iter()
            .map(|entry| WatchRow {
                expression: entry.expression.as_str().into(),
                type_name: entry.type_name.as_str().into(),
                scope: entry.scope.as_str().into(),
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
