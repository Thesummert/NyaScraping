use crate::{
    acquisition::{
        sampling_interval_micros, Acquisition, MemorySource, CAPACITY, MAX_MEMORY_CHANNELS,
    },
    firmware_browser::FirmwareBrowser,
    watch_list::WatchList,
    waveform_view::{self, COLORS, PLOT_WIDTH},
    AppWindow, BreakpointRow, FirmwareTreeRow, FrameRow, PackRow, TargetRow, ToolRow, WatchRow,
    WaveformRow,
};
use nya_application::Event;
use nya_domain::{
    firmware::FirmwareImage, DebugSnapshot, DebugTarget, Document, EvaluatedWatch, SessionState,
    TargetCatalog, ToolKind, ToolState, WatchRequest,
};
use slint::{Model, ModelRc, VecModel};
use std::{collections::HashMap, rc::Rc, sync::Arc, time::Duration};

#[derive(Default)]
pub struct Presenter {
    browser: Option<FirmwareBrowser>,
    watches: WatchList,
    pub acquisition: Arc<Acquisition>,
    last_sequence: Option<u64>,
    last_plot_sequence: Option<u64>,
    watch_revision: u64,
    sample_count: u64,
    values: HashMap<String, EvaluatedWatch>,
    targets: Vec<DebugTarget>,
    probes: Vec<DebugTarget>,
}

/// Plain Rust data prepared on the worker, before entering the Slint event loop.
pub enum PreparedEvent {
    Internal,
    Tools(ToolState, String),
    Targets(TargetCatalog),
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
            Event::MemoryRead => Self::Internal,
            Event::ToolsUpdated { state, message } => Self::Tools(state, message),
            Event::TargetsUpdated(catalog) => Self::Targets(catalog),
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
                    "{} 个源文件，{} 个函数条目，{} 个变量/参数。\n\n点击目录或文件可展开；双击变量添加监视。函数默认隐藏，可通过开关显示。\n\n{}",
                    image.debug.files.len(), image.debug.functions.len(), image.debug.variables.len(), image.debug.warnings.join("\n"),
                ),
                status: format!(
                    "解析完成：{} 个源文件，{} 个函数，{} 个变量/参数{}",
                    image.debug.files.len(), image.debug.functions.len(), image.debug.variables.len(),
                    if image.debug.warnings.is_empty() { "" } else { "（存在调试信息提示，见详情）" },
                ),
                browser: FirmwareBrowser::new(&image.debug, image.little_endian, image.kind == "Executable"),
            })),
        }
    }
}

impl Presenter {
    pub fn clear_firmware(&mut self, ui: &AppWindow) {
        ui.set_live_enabled(false);
        self.acquisition.stop();
        self.acquisition.set_targets(Vec::new());
        self.browser = None;
        self.watches.reset_firmware();
        self.sync_waveforms(ui);
        self.invalidate_watches(ui);
        self.refresh_tree(ui);
        ui.set_firmware_selected_node(-1);
        ui.set_firmware_title("尚未打开编译产物".into());
        ui.set_firmware_meta("".into());
        ui.set_firmware_report("解析编译产物后，在这里查看段、节与符号表。".into());
        ui.set_firmware_details("选择文件树中的函数或变量，查看类型、声明位置及存储位置。".into());
    }

    pub fn saved_watches(&self) -> Vec<crate::watch_list::SavedWatch> {
        self.watches.saved()
    }

    pub fn restore_watches(
        &mut self,
        ui: &AppWindow,
        saved: &[crate::watch_list::SavedWatch],
    ) -> usize {
        let missing = self.watches.restore(saved, self.browser.as_ref());
        self.sync_waveforms(ui);
        self.invalidate_watches(ui);
        missing
    }
    pub fn apply(&mut self, ui: &AppWindow, event: PreparedEvent) {
        if matches!(&event, PreparedEvent::Internal) {
            return;
        }
        if !matches!(&event, PreparedEvent::Debug(_, false)) {
            ui.set_busy(false);
        }
        match event {
            PreparedEvent::Internal => {}
            PreparedEvent::Tools(state, message) => {
                let kind = match state.backend {
                    Some(ToolKind::OpenOcd) => 0,
                    Some(ToolKind::PyOcd) => 1,
                    Some(ToolKind::ProbeRs) => 2,
                    None => -1,
                };
                if ui.get_backend_kind() != kind {
                    ui.set_live_enabled(false);
                    self.acquisition.stop();
                    self.refresh_acquisition(ui);
                    ui.set_debug_target("".into());
                    ui.set_debug_interface("".into());
                    ui.set_target_search("".into());
                    ui.set_probe_search("".into());
                }
                ui.set_backend_kind(kind);
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
                            managed: tool.version != "system"
                                && !tool.version.starts_with("local:"),
                            local: tool.version.starts_with("local:"),
                            selectable: tool.version != "system" || first_system,
                        }
                    })
                    .collect();
                ui.set_tools(ModelRc::from(Rc::new(VecModel::from(rows))));
                ui.set_backend_summary(summary.into());
                ui.set_tool_message(message.as_str().into());
                ui.set_status(message.into());
                self.apply_targets(ui, state.targets);
            }
            PreparedEvent::Targets(catalog) => {
                ui.set_status(catalog.message.as_str().into());
                self.apply_targets(ui, catalog);
            }
            PreparedEvent::Debug(snapshot, _) => {
                let keep_sample = ui.get_live_enabled()
                    && ui.get_debug_running()
                    && snapshot.state == SessionState::Running
                    && !snapshot.watch_sample
                    && snapshot.values.is_empty();
                if let Some(error) = &snapshot.watch_sample_error {
                    ui.set_live_enabled(false);
                    ui.set_watch_sample_status(format!("自动刷新已停止：{error}").into());
                } else if snapshot.watch_sample && ui.get_live_enabled() {
                    self.sample_count += 1;
                    ui.set_watch_sample_status(
                        format!(
                            "已采样 #{} · {} 项{}",
                            self.sample_count,
                            snapshot.values.len(),
                            if snapshot.state == SessionState::Running {
                                " · 运行中（最近采样值）"
                            } else {
                                " · 已暂停"
                            }
                        )
                        .into(),
                    );
                } else if ui.get_live_enabled()
                    && snapshot.state == SessionState::Running
                    && !ui.get_debug_running()
                {
                    ui.set_watch_sample_status("等待下一轮采样".into());
                } else if !matches!(snapshot.state, SessionState::Running | SessionState::Halted) {
                    ui.set_watch_sample_status(
                        if ui.get_live_enabled() {
                            "等待连接目标"
                        } else {
                            "实时刷新已关闭"
                        }
                        .into(),
                    );
                }
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
                if !keep_sample {
                    self.values = if snapshot.watch_sample && !ui.get_live_enabled() {
                        HashMap::new()
                    } else {
                        snapshot
                            .values
                            .into_iter()
                            .map(|v| (v.id.clone(), v))
                            .collect()
                    };
                }
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
                if matches!(
                    snapshot.state,
                    SessionState::Disconnected | SessionState::Faulted
                ) && self
                    .acquisition
                    .state
                    .lock()
                    .expect("acquisition lock")
                    .endpoint
                    == Some(MemorySource::DebugSession)
                {
                    self.acquisition.stop();
                    ui.set_live_enabled(false);
                    self.refresh_acquisition(ui);
                    self.refresh_watches(ui);
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
                ui.set_live_enabled(false);
                self.acquisition.stop();
                self.acquisition.set_targets(Vec::new());
                ui.set_firmware_title(view.title.into());
                ui.set_firmware_meta(view.metadata.into());
                ui.set_firmware_report(view.report.into());
                ui.set_firmware_details(view.details.into());
                let mut browser = view.browser;
                browser.set_show_functions(ui.get_functions_visible());
                browser.set_query(&ui.get_firmware_search());
                self.browser = Some(browser);
                self.watches.reset_firmware();
                self.sync_waveforms(ui);
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

    fn apply_targets(&mut self, ui: &AppWindow, catalog: TargetCatalog) {
        self.targets = catalog.targets;
        self.probes = catalog.probes;
        ui.set_probe_message(catalog.probe_message.into());
        ui.set_target_message(catalog.message.into());
        let packs: Vec<_> = catalog
            .packs
            .into_iter()
            .map(|path| PackRow {
                name: path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .as_ref()
                    .into(),
                path: path.to_string_lossy().as_ref().into(),
            })
            .collect();
        ui.set_packs(ModelRc::from(Rc::new(VecModel::from(packs))));
        self.search_targets(ui, &ui.get_target_search());
        self.search_probes(ui, &ui.get_probe_search());
    }

    pub fn search_targets(&self, ui: &AppWindow, query: &str) {
        ui.set_targets(Self::config_rows(&self.targets, query));
    }

    pub fn search_probes(&self, ui: &AppWindow, query: &str) {
        ui.set_probes(Self::config_rows(&self.probes, query));
    }

    fn config_rows(configs: &[DebugTarget], query: &str) -> ModelRc<TargetRow> {
        let query = query.trim().to_lowercase();
        let rows: Vec<_> = configs
            .iter()
            .filter(|target| {
                [&target.name, &target.vendor, &target.source]
                    .iter()
                    .any(|text| text.to_lowercase().contains(&query))
            })
            .map(|target| TargetRow {
                name: target.name.as_str().into(),
                label: if target.vendor == "OpenOCD" {
                    target.source.as_str()
                } else {
                    target.name.as_str()
                }
                .into(),
                detail: format!(
                    "{} · {}",
                    target.vendor,
                    if target.vendor == "OpenOCD" {
                        &target.name
                    } else {
                        &target.source
                    }
                )
                .into(),
            })
            .collect();
        ModelRc::from(Rc::new(VecModel::from(rows)))
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
        }
    }

    pub fn search_firmware(&mut self, ui: &AppWindow, query: &str) {
        if let Some(browser) = &mut self.browser {
            browser.set_query(query);
            self.refresh_tree(ui);
            ui.set_firmware_selected_node(-1);
        }
    }

    pub fn live_refresh_changed(&mut self, ui: &AppWindow) {
        self.sync_waveforms(ui);
        self.refresh_watches(ui);
        if !ui.get_live_enabled() {
            ui.set_watch_sample_status("实时刷新已关闭".into());
        }
        if !ui.get_live_enabled() && ui.get_debug_running() {
            self.values.clear();
            self.refresh_watches(ui);
        }
    }

    pub fn write_request(&self, entry: i32, node: i32) -> Option<WatchRequest> {
        self.watches
            .rows()
            .into_iter()
            .find(|row| row.entry as i32 == entry && row.node as i32 == node)
            .map(|row| WatchRequest {
                id: format!("{}:{}:{}", self.watch_revision, row.entry, row.node),
                expression: row.expression,
                function: row.function,
            })
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

    pub fn add_firmware_variable(&mut self, ui: &AppWindow, row: i32) {
        let variable = usize::try_from(row)
            .ok()
            .and_then(|row| self.browser.as_ref()?.variable_at(row));
        if let Some(variable) = variable {
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
        let variable = self
            .browser
            .as_ref()
            .and_then(|b| b.resolve_variable(expression.trim()));
        let added = match variable {
            Some(variable) => self.watches.add_variable(variable),
            None => self.watches.add_expression(expression),
        };
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
            self.sync_waveforms(ui);
            ui.set_status("已移除监视变量".into());
        }
    }

    pub fn plot_variable(&mut self, ui: &AppWindow, row: i32) {
        let variable = usize::try_from(row)
            .ok()
            .and_then(|row| self.browser.as_ref()?.variable_at(row));
        if let Some(variable) = variable {
            match self.watches.add_waveform(variable) {
                Ok(()) => {
                    self.invalidate_watches(ui);
                    self.sync_waveforms(ui);
                    ui.set_current_tab(1);
                    ui.set_status("已加入实时波形；在波形页开始采集".into());
                }
                Err(error) => ui.set_status(error.into()),
            }
        }
    }

    pub fn toggle_waveform(&mut self, ui: &AppWindow, index: i32) {
        let result = usize::try_from(index)
            .map_err(|_| "监视变量不存在")
            .and_then(|index| self.watches.toggle_waveform(index));
        match result {
            Ok(()) => {
                let enabled = self.watches.entries[index as usize].waveform;
                self.invalidate_watches(ui);
                self.sync_waveforms(ui);
                if enabled {
                    ui.set_current_tab(1);
                }
                ui.set_status(
                    if enabled {
                        "已加入实时波形"
                    } else {
                        "已从实时波形移除"
                    }
                    .into(),
                );
            }
            Err(error) => {
                self.invalidate_watches(ui);
                ui.set_status(error.into());
            }
        }
    }

    fn memory_source(ui: &AppWindow) -> Result<MemorySource, String> {
        if matches!(ui.get_backend_kind(), 1 | 2) {
            if !ui.get_debug_connected() {
                return Err("请先在调试页连接所选的 pyOCD / probe-rs 后端".into());
            }
            Ok(MemorySource::DebugSession)
        } else {
            ui.get_acquisition_endpoint()
                .trim()
                .parse()
                .map(MemorySource::OpenOcd)
                .map_err(|_| "请输入 OpenOCD Tcl 的 IP:端口，例如 127.0.0.1:6666".into())
        }
    }

    pub fn start_acquisition(&mut self, ui: &AppWindow) {
        if self
            .acquisition
            .state
            .lock()
            .expect("acquisition lock")
            .channels
            .is_empty()
        {
            ui.set_acquisition_status("请先添加波形通道".into());
            return;
        }
        match Self::memory_source(ui) {
            Ok(source) => self.acquisition.start_source(source),
            Err(error) => {
                ui.set_acquisition_status(error.into());
                return;
            }
        }
        self.refresh_acquisition(ui);
    }

    pub fn stop_acquisition(&mut self, ui: &AppWindow) {
        ui.set_live_enabled(false);
        ui.set_watch_sample_status("实时刷新已关闭".into());
        self.acquisition.stop();
        self.refresh_acquisition(ui);
        self.refresh_watches(ui);
    }

    fn sync_waveforms(&mut self, ui: &AppWindow) {
        let live = ui.get_live_enabled();
        let interval =
            sampling_interval_micros(ui.get_live_interval_index(), &ui.get_live_custom_hz())
                .unwrap_or(500_000);
        // Waveforms take priority; monitoring fills the remaining bounded read slots.
        let mut targets = self
            .watches
            .entries
            .iter()
            .filter(|e| e.waveform)
            .filter_map(|e| Some((e.channel_id()?, e.memory.clone()?, true)))
            .collect::<Vec<_>>();
        if live {
            targets.extend(
                self.watches
                    .entries
                    .iter()
                    .filter(|e| !e.waveform)
                    .filter_map(|e| Some((e.channel_id()?, e.memory.clone()?, false)))
                    .take(MAX_MEMORY_CHANNELS - targets.len()),
            );
        }
        let empty = targets.is_empty();
        if empty {
            self.acquisition.stop();
        }
        self.acquisition
            .configure(targets, Duration::from_micros(interval));
        if live
            && !empty
            && self
                .acquisition
                .state
                .lock()
                .expect("acquisition lock")
                .endpoint
                .is_none()
        {
            match Self::memory_source(ui) {
                Ok(source) => self.acquisition.start_source(source),
                Err(error) => {
                    ui.set_watch_sample_status(error.as_str().into());
                    ui.set_acquisition_status(error.into());
                }
            }
        }
        self.last_sequence = None;
        self.last_plot_sequence = None;
        self.refresh_acquisition(ui);
    }

    pub fn refresh_acquisition(&mut self, ui: &AppWindow) {
        let state = self.acquisition.state.lock().expect("acquisition lock");
        let sequence = state.sequence;
        let update_values = self.last_sequence != Some(sequence);
        let update_plot = self.last_plot_sequence != Some(sequence)
            && (ui.get_current_tab() == 1 || self.last_plot_sequence.is_none());
        if !update_values && !update_plot {
            return;
        }
        let active = state.endpoint.is_some();
        let status = state.status.clone();
        let rate = if state.elapsed.is_zero() {
            0.0
        } else {
            state.sweeps.saturating_sub(1) as f64 / state.elapsed.as_secs_f64()
        };
        let statistics = format!("实测平均 {rate:.0} 次/秒/通道 · 累计 {} 轮 · 每通道最多 {CAPACITY} 点 · 时间戳为主机读取时刻", state.sweeps);
        let mut values = Vec::new();
        let mut plots = Vec::new();
        for (watch_index, entry) in self.watches.entries.iter().enumerate() {
            let data = entry
                .channel_id()
                .and_then(|id| state.channels.iter().find(|c| c.id == id));
            let latest = data
                .and_then(|c| c.samples.latest())
                .map(|sample| waveform_view::format_value(sample.value));
            if update_values
                && (entry.waveform || (ui.get_live_enabled() && entry.memory.is_some()))
            {
                let value = match latest.as_ref() {
                    Some(value) if !active => format!("{value}（已停止）"),
                    Some(value) => value.clone(),
                    None => if entry.waveform {
                        "等待采集"
                    } else {
                        "未采集"
                    }
                    .into(),
                };
                values.push((watch_index, value));
            }
            if update_plot && entry.waveform {
                let bins = data
                    .map(|c| {
                        c.samples
                            .envelope(state.elapsed, Duration::from_secs(5), PLOT_WIDTH)
                    })
                    .unwrap_or_else(|| vec![None; PLOT_WIDTH]);
                let count = data.map(|c| c.samples.len()).unwrap_or(0);
                let evicted = data.map(|c| c.samples.evicted()).unwrap_or(0);
                let invalid = data.map(|c| c.invalid).unwrap_or(0);
                plots.push((
                    watch_index,
                    entry.expression.clone(),
                    entry.scope.clone(),
                    latest.unwrap_or_else(|| "—".into()),
                    bins,
                    count,
                    invalid,
                    evicted,
                ));
            }
        }
        // Release the sampler before rasterization or Slint model updates.
        drop(state);
        ui.set_acquisition_active(active);
        ui.set_acquisition_status(status.into());
        ui.set_acquisition_statistics(statistics.into());
        if ui.get_live_enabled() {
            ui.set_watch_sample_status(
                format!(
                    "无暂停刷新 · {} 项内存变量 · {}",
                    self.acquisition
                        .state
                        .lock()
                        .expect("acquisition lock")
                        .channels
                        .len(),
                    ui.get_acquisition_status()
                )
                .into(),
            );
        }
        if update_values {
            let model = ui.get_watches();
            if let Some(model) = model.as_any().downcast_ref::<VecModel<WatchRow>>() {
                for index in 0..model.row_count() {
                    if let Some(mut row) = model.row_data(index) {
                        if row.node_index != 0 {
                            continue;
                        }
                        if let Some((_, value)) = values
                            .iter()
                            .find(|(entry, _)| *entry as i32 == row.entry_index)
                        {
                            if row.value.as_str() != value {
                                row.value = value.as_str().into();
                                model.set_row_data(index, row);
                            }
                        }
                    }
                }
            }
            self.last_sequence = Some(sequence);
        }
        if update_plot {
            let rows: Vec<_> = plots
                .into_iter()
                .enumerate()
                .map(
                    |(index, (watch_index, name, scope, value, bins, count, invalid, evicted))| {
                        let color = COLORS[index % COLORS.len()];
                        let (plot, minimum, maximum) = waveform_view::render(&bins, color);
                        WaveformRow {
                            watch_index: watch_index as i32,
                            name: name.into(),
                            scope: scope.into(),
                            value: value.into(),
                            minimum: minimum.into(),
                            maximum: maximum.into(),
                            statistics: format!("{count} 点 · 淘汰 {evicted} · 无效 {invalid}")
                                .into(),
                            tint: slint::Color::from_rgb_u8(color[0], color[1], color[2]),
                            plot,
                        }
                    },
                )
                .collect();
            let existing = ui.get_waveform_channels();
            if let Some(model) = existing
                .as_any()
                .downcast_ref::<VecModel<WaveformRow>>()
                .filter(|m| m.row_count() == rows.len())
            {
                for (index, row) in rows.into_iter().enumerate() {
                    model.set_row_data(index, row);
                }
            } else {
                ui.set_waveform_channels(ModelRc::from(Rc::new(VecModel::from(rows))));
            }
            self.last_plot_sequence = Some(sequence);
        }
    }

    fn invalidate_watches(&mut self, ui: &AppWindow) {
        if ui.get_live_enabled() {
            ui.set_watch_sample_status("监视项已变化，等待下一轮采样".into());
        }
        self.watch_revision += 1;
        ui.set_watch_revision(self.watch_revision as i32);
        self.values.clear();
        self.sync_waveforms(ui);
        self.refresh_watches(ui);
    }

    pub fn watch_requests(&self) -> Vec<WatchRequest> {
        self.watches
            .rows()
            .into_iter()
            .filter(|row| !self.watches.entries[row.entry].waveform)
            .map(|row| WatchRequest {
                id: format!("{}:{}:{}", self.watch_revision, row.entry, row.node),
                expression: row.expression,
                function: row.function,
            })
            .collect()
    }

    fn refresh_watches(&self, ui: &AppWindow) {
        let acquisition = self.acquisition.state.lock().expect("acquisition lock");
        let rows: Vec<_> = self
            .watches
            .rows()
            .into_iter()
            .map(|entry| {
                let id = format!("{}:{}:{}", self.watch_revision, entry.entry, entry.node);
                let runtime = self.values.get(&id).map(|v| &v.value);
                let watch = &self.watches.entries[entry.entry];
                let waveform = watch.waveform && entry.node == 0;
                let waveform_value = if waveform
                    || (ui.get_live_enabled() && watch.memory.is_some() && entry.node == 0)
                {
                    watch
                        .channel_id()
                        .and_then(|id| acquisition.channels.iter().find(|c| c.id == id))
                        .and_then(|c| c.samples.latest())
                        .map(|sample| {
                            let value = waveform_view::format_value(sample.value);
                            if acquisition.endpoint.is_none() {
                                format!("{value}（已停止）")
                            } else {
                                value
                            }
                        })
                        .or_else(|| Some("等待采集".into()))
                } else {
                    None
                };
                WatchRow {
                    waveform,
                    can_plot: watch.memory.is_some() && entry.node == 0,
                    plot_hint: if watch.memory.is_some() {
                        "运行中读取"
                    } else {
                        "需固定地址的数值变量"
                    }
                    .into(),
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
                    value: waveform_value
                        .as_deref()
                        .or_else(|| runtime.map(|v| v.display_value.as_str()))
                        .unwrap_or("—")
                        .into(),
                    error: if ui.get_live_enabled()
                        && !ui.get_debug_halted()
                        && (watch.memory.is_none() || entry.node != 0)
                    {
                        "运行中无固定数值地址；暂停后求值".into()
                    } else if ui.get_live_enabled()
                        && watch.memory.is_some()
                        && entry.node == 0
                        && !acquisition
                            .channels
                            .iter()
                            .any(|channel| Some(channel.id) == watch.channel_id())
                    {
                        "无暂停采集最多 64 项，请减少监视变量".into()
                    } else {
                        runtime
                            .and_then(|v| v.error.as_deref())
                            .unwrap_or_default()
                            .into()
                    },
                }
            })
            .collect();
        drop(acquisition);
        let current = ui.get_watches();
        if let Some(model) = current.as_any().downcast_ref::<VecModel<WatchRow>>() {
            // Preserve the list's scroll position and row components during value refreshes.
            if model.row_count() == rows.len()
                && rows.iter().enumerate().all(|(i, row)| {
                    model.row_data(i).is_some_and(|old| {
                        old.entry_index == row.entry_index
                            && old.node_index == row.node_index
                            && old.expression == row.expression
                            && old.depth == row.depth
                            && old.expanded == row.expanded
                    })
                })
            {
                for (index, row) in rows.into_iter().enumerate() {
                    if model.row_data(index).as_ref() != Some(&row) {
                        model.set_row_data(index, row);
                    }
                }
                return;
            }
        }
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
                variable: row.variable,
                can_plot: row.can_plot,
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
