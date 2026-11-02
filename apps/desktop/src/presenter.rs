use crate::AppWindow;
use nya_application::Event;
use slint::{ModelRc, SharedString, VecModel};
use std::rc::Rc;

pub fn apply(ui: &AppWindow, event: Event) {
    ui.set_busy(false);
    match event {
        Event::ToolsDiscovered(installations) => {
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
        Event::SourceOpened(document) => {
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
        Event::Failed(error) => ui.set_status(format!("操作失败：{error}").into()),
    }
}
