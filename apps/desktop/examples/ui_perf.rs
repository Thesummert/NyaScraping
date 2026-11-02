//! Headless benchmark for HiDPI redraws and large firmware-tree selection.
//! Run with `cargo run -p nya-scraping --example ui_perf --locked`.
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PlatformError, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, Model, Rgb8Pixel};
use std::{rc::Rc, time::Instant};

#[path = "../src/firmware_browser.rs"]
mod firmware_browser;
#[path = "../src/presenter.rs"]
mod presenter;
#[path = "../src/watch_list.rs"]
mod watch_list;
slint::include_modules!();

struct BenchPlatform(Rc<MinimalSoftwareWindow>);
impl Platform for BenchPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.0.clone())
    }
}

fn redraw(window: &MinimalSoftwareWindow, pixels: &mut [Rgb8Pixel], width: usize) {
    window.request_redraw();
    assert!(window.draw_if_needed(|renderer| {
        renderer.render(pixels, width);
    }));
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(BenchPlatform(window.clone())))?;
    let ui = AppWindow::new()?;
    ui.show()?;
    window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: 2.0 });
    window.set_size(slint::PhysicalSize::new(2960, 1800));
    let mut pixels = vec![Rgb8Pixel::default(); 2960 * 1800];
    redraw(&window, &mut pixels, 2960);
    let start = Instant::now();
    for frame in 0..20 {
        window.dispatch_event(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(30., 205. + (frame % 10) as f32 * 30.),
        });
        slint::platform::update_timers_and_animations();
        redraw(&window, &mut pixels, 2960);
    }
    println!(
        "HiDPI hover: {:.2} ms/frame",
        start.elapsed().as_secs_f64() * 1000. / 20.
    );
    let start = Instant::now();
    for frame in 0..10 {
        let width = 2960 + frame * 2;
        window.set_size(slint::PhysicalSize::new(width, 1800));
        pixels.resize(width as usize * 1800, Rgb8Pixel::default());
        redraw(&window, &mut pixels, width as usize);
    }
    println!(
        "HiDPI resize: {:.2} ms/frame",
        start.elapsed().as_secs_f64() * 100.
    );
    window.set_size(slint::PhysicalSize::new(2960, 1800));
    pixels.resize(2960 * 1800, Rgb8Pixel::default());
    let source: String = (0..2000)
        .map(|line| format!("    counter += {line}; // source line\n"))
        .collect();
    let start = Instant::now();
    ui.set_source_text(source.into());
    redraw(&window, &mut pixels, 2960);
    println!(
        "Open 2000 source lines: {:.2} ms",
        start.elapsed().as_secs_f64() * 1000.
    );
    let start = Instant::now();
    for _ in 0..10 {
        window.dispatch_event(WindowEvent::PointerScrolled {
            position: slint::LogicalPosition::new(700., 400.),
            delta_x: 0.,
            delta_y: -60.,
        });
        redraw(&window, &mut pixels, 2960);
    }
    println!(
        "Scroll source: {:.2} ms/frame",
        start.elapsed().as_secs_f64() * 100.
    );
    let mut presenter = presenter::Presenter::default();
    use nya_domain::firmware::*;
    let info = FirmwareDebugInfo {
        variables: (0..20000)
            .map(|id| DebugVariable {
                name: format!("variable_{id:05}"),
                type_name: Some("int".into()),
                byte_size: Some(4),
                members: vec![],
                declaration: None,
                function: None,
                kind: VariableKind::Global,
                location: VariableLocation::Address(id * 4),
                constant: None,
            })
            .collect(),
        ..Default::default()
    };
    let image = FirmwareImage {
        path: "benchmark.elf".into(),
        file_size: 0,
        format: "ELF".into(),
        architecture: "ARM".into(),
        kind: "Executable".into(),
        address_bits: 32,
        little_endian: true,
        entry: 0,
        has_debug_info: true,
        debug: info,
        segments: vec![],
        sections: vec![],
        symbols: vec![],
    };
    presenter.apply(
        &ui,
        nya_application::Event::FirmwareOpened(Box::new(image)).into(),
    );
    presenter.select_firmware(&ui, 0);
    assert_eq!(ui.get_firmware_tree().row_count(), 20001);
    let original_tree = ui.get_firmware_tree();
    let start = Instant::now();
    for row in 1..51 {
        presenter.select_firmware(&ui, row);
    }
    assert!(std::ptr::eq(
        original_tree.as_any(),
        ui.get_firmware_tree().as_any()
    ));
    println!(
        "Select in 20000-node tree: {:.3} ms/click",
        start.elapsed().as_secs_f64() * 20.
    );
    redraw(&window, &mut pixels, 2960);
    let start = Instant::now();
    for row in 1..21 {
        presenter.select_firmware(&ui, row);
        redraw(&window, &mut pixels, 2960);
    }
    println!(
        "Select + redraw with source and tree: {:.2} ms/frame",
        start.elapsed().as_secs_f64() * 50.
    );
    // Exercise the remaining presenter paths without a GUI event loop.
    presenter.add_selected_variable(&ui);
    presenter.add_expression(&ui, "counter");
    presenter.remove_watch(&ui, 0);

    // Verify click-to-watch and aggregate expansion through the real UI models.
    let image = nya_firmware_backend::NativeFirmwareBackend.parse(
        std::path::Path::new("fixture.axf"),
        include_bytes!("../../../crates/firmware-backend/tests/fixtures/arm-dwarf4.axf"),
    )?;
    presenter.apply(
        &ui,
        nya_application::Event::FirmwareOpened(Box::new(image)).into(),
    );
    presenter.remove_watch(&ui, 0); // remove the retained manual expression
    let file = ui
        .get_firmware_tree()
        .iter()
        .position(|row| row.label == "main.c")
        .unwrap();
    presenter.select_firmware(&ui, file as i32);
    assert!(!ui
        .get_firmware_tree()
        .iter()
        .any(|row| row.label.starts_with("ƒ ")));
    let original_count = ui.get_firmware_tree().row_count();
    presenter.search_firmware(&ui, "COMPUTE");
    assert!(ui
        .get_firmware_tree()
        .iter()
        .any(|row| row.label == "ƒ compute"));
    assert!(!ui
        .get_firmware_tree()
        .iter()
        .any(|row| row.label == "ƒ main"));
    presenter.search_firmware(&ui, "missing-file-name");
    assert_eq!(ui.get_firmware_tree().row_count(), 0);
    presenter.search_firmware(&ui, "");
    assert_eq!(ui.get_firmware_tree().row_count(), original_count);
    presenter.show_functions(&ui, true);
    assert!(ui
        .get_firmware_tree()
        .iter()
        .any(|row| row.label == "ƒ compute"));
    presenter.show_functions(&ui, false);
    let sensor = ui
        .get_firmware_tree()
        .iter()
        .position(|row| row.label == "sensor")
        .unwrap();
    presenter.select_firmware(&ui, sensor as i32);
    assert_eq!(ui.get_watches().row_count(), 3);
    assert_eq!(ui.get_watches().row_data(0).unwrap().type_name, "Sensor");
    assert_eq!(ui.get_watches().row_data(1).unwrap().expression, "value");
    let member = presenter.write_request(0, 1).unwrap();
    assert!(member.expression.ends_with(".value"));
    assert!(presenter.write_request(-1, 0).is_none());
    assert!(presenter.write_request(99, 0).is_none());
    assert_eq!(ui.get_watches().row_data(2).unwrap().expression, "label");
    presenter.select_firmware(&ui, sensor as i32);
    assert_eq!(ui.get_watches().row_count(), 3);
    presenter.toggle_watch(&ui, 0, 0);
    assert_eq!(ui.get_watches().row_count(), 1);
    presenter.toggle_watch(&ui, 0, 0);
    redraw(&window, &mut pixels, 2960);
    presenter.remove_watch(&ui, 0);
    assert_eq!(ui.get_watches().row_count(), 0);
    println!("Click-to-watch, function visibility and aggregate UI checks passed");
    use nya_domain::{ToolInstallation, ToolKind, ToolState};
    let installations = vec![
        ToolInstallation {
            tool: ToolKind::PyOcd,
            version: "1.0".into(),
            executable: "/managed/pyocd/1.0/bin/pyocd".into(),
            selected: true,
        },
        ToolInstallation {
            tool: ToolKind::PyOcd,
            version: "2.0".into(),
            executable: "/managed/pyocd/2.0/bin/pyocd".into(),
            selected: false,
        },
        ToolInstallation {
            tool: ToolKind::OpenOcd,
            version: "system".into(),
            executable: "/usr/bin/openocd".into(),
            selected: true,
        },
        ToolInstallation {
            tool: ToolKind::OpenOcd,
            version: "system".into(),
            executable: "/opt/bin/openocd".into(),
            selected: false,
        },
    ];
    presenter.apply(
        &ui,
        nya_application::Event::ToolsUpdated {
            state: ToolState {
                installations,
                backend: Some(ToolKind::PyOcd),
                targets: Default::default(),
            },
            message: "后端选择已保存".into(),
        }
        .into(),
    );
    assert_eq!(ui.get_tools().row_count(), 4);
    assert!(ui.get_tools().row_data(0).unwrap().active);
    assert!(!ui.get_tools().row_data(1).unwrap().active);
    assert!(ui.get_tools().row_data(2).unwrap().selectable);
    assert!(!ui.get_tools().row_data(2).unwrap().managed);
    assert!(!ui.get_tools().row_data(3).unwrap().selectable);
    assert_eq!(ui.get_backend_summary(), "pyOCD · 1.0");
    ui.set_debug_target("stm32h743zitx".into());
    ui.set_busy(true);
    presenter.apply(
        &ui,
        nya_application::Event::TargetsUpdated(nya_domain::TargetCatalog {
            targets: vec![
                nya_domain::DebugTarget {
                    name: "stm32h743zitx".into(),
                    vendor: "STMicroelectronics".into(),
                    source: "pack".into(),
                },
                nya_domain::DebugTarget {
                    name: "cortex_m".into(),
                    vendor: "Arm".into(),
                    source: "builtin".into(),
                },
            ],
            packs: vec!["/tmp/芯片包 with spaces.pack".into()],
            message: "2 个可选目标".into(),
            ..Default::default()
        })
        .into(),
    );
    assert!(!ui.get_busy());
    assert_eq!(ui.get_targets().row_count(), 2);
    assert_eq!(
        ui.get_packs().row_data(0).unwrap().name,
        "芯片包 with spaces.pack"
    );
    presenter.search_targets(&ui, " STM32 ");
    assert_eq!(ui.get_targets().row_count(), 1);
    assert_eq!(ui.get_targets().row_data(0).unwrap().name, "stm32h743zitx");
    presenter.search_targets(&ui, "ARM");
    assert_eq!(ui.get_targets().row_count(), 1);
    presenter.search_targets(&ui, "unknown-device");
    assert_eq!(ui.get_targets().row_count(), 0);
    presenter.search_targets(&ui, "");
    assert_eq!(ui.get_targets().row_count(), 2);
    assert_eq!(ui.get_debug_target(), "stm32h743zitx");
    ui.set_current_tab(4);
    redraw(&window, &mut pixels, 2960);
    ui.set_current_tab(3);
    redraw(&window, &mut pixels, 2960);
    presenter.apply(
        &ui,
        nya_application::Event::ToolsUpdated {
            state: ToolState {
                installations: vec![],
                backend: Some(ToolKind::PyOcd),
                targets: Default::default(),
            },
            message: "已卸载 pyOCD · 1.0".into(),
        }
        .into(),
    );
    assert!(ui.get_backend_summary().contains("不可用"));
    assert_eq!(ui.get_tools().row_count(), 0);
    assert!(!ui.get_busy());
    redraw(&window, &mut pixels, 2960);
    println!(
        "Backend selection, PATH priority, missing-version and tools-page rendering checks passed"
    );
    ui.set_debug_interface("interface/old.cfg".into());
    presenter.apply(
        &ui,
        nya_application::Event::ToolsUpdated {
            state: ToolState {
                backend: Some(ToolKind::OpenOcd),
                ..Default::default()
            },
            message: "切换到 OpenOCD".into(),
        }
        .into(),
    );
    assert!(ui.get_debug_target().is_empty());
    assert!(ui.get_debug_interface().is_empty());
    assert_eq!(ui.get_targets().row_count(), 0);
    assert_eq!(ui.get_packs().row_count(), 0);
    println!("Target search, pack display and backend-switch isolation checks passed");
    presenter.apply(
        &ui,
        nya_application::Event::TargetsUpdated(nya_domain::TargetCatalog {
            targets: vec![nya_domain::DebugTarget {
                name: "/opt/openocd/target/stm32h7x.cfg".into(),
                vendor: "OpenOCD".into(),
                source: "target/stm32h7x.cfg".into(),
            }],
            probes: vec![
                nya_domain::DebugTarget {
                    name: "/opt/openocd/interface/stlink.cfg".into(),
                    vendor: "OpenOCD".into(),
                    source: "interface/stlink.cfg".into(),
                },
                nya_domain::DebugTarget {
                    name: "/opt/openocd/interface/ftdi/probe with spaces.cfg".into(),
                    vendor: "OpenOCD".into(),
                    source: "interface/ftdi/probe with spaces.cfg".into(),
                },
            ],
            probe_message: "2 个可选 Probe 配置".into(),
            message: "1 个可选目标".into(),
            ..Default::default()
        })
        .into(),
    );
    assert_eq!(ui.get_probes().row_count(), 2);
    presenter.search_probes(&ui, " STLINK ");
    assert_eq!(ui.get_probes().row_count(), 1);
    assert_eq!(
        ui.get_probes().row_data(0).unwrap().label,
        "interface/stlink.cfg"
    );
    presenter.search_probes(&ui, "ftdi");
    assert_eq!(ui.get_probes().row_count(), 1);
    assert!(ui
        .get_probes()
        .row_data(0)
        .unwrap()
        .name
        .ends_with("probe with spaces.cfg"));
    presenter.search_probes(&ui, "unknown-probe");
    assert_eq!(ui.get_probes().row_count(), 0);
    assert_eq!(ui.get_targets().row_count(), 1);
    presenter.search_probes(&ui, "");
    ui.set_debug_interface("/opt/openocd/interface/stlink.cfg".into());
    ui.set_probe_search("stlink".into());
    ui.set_current_tab(4);
    redraw(&window, &mut pixels, 2960);
    presenter.apply(
        &ui,
        nya_application::Event::ToolsUpdated {
            state: ToolState {
                backend: Some(ToolKind::PyOcd),
                ..Default::default()
            },
            message: "切换到 pyOCD".into(),
        }
        .into(),
    );
    assert!(ui.get_debug_interface().is_empty());
    assert!(ui.get_probe_search().is_empty());
    assert_eq!(ui.get_probes().row_count(), 0);
    redraw(&window, &mut pixels, 2960);
    println!("OpenOCD probe search, nested interface paths and backend-switch cleanup passed");
    use nya_domain::{
        Breakpoint, DebugSnapshot, EvaluatedWatch, SessionState, StackFrame, WatchValue,
    };
    presenter.add_expression(&ui, "counter");
    let request = presenter.watch_requests().into_iter().next().unwrap();
    ui.set_busy(true);
    presenter.apply(
        &ui,
        nya_application::Event::DebugUpdated {
            snapshot: DebugSnapshot {
                state: SessionState::Halted,
                message: "目标已暂停".into(),
                frames: vec![StackFrame {
                    level: 0,
                    function: "compute".into(),
                    file: "main.c".into(),
                    line: 8,
                    address: "0x8000010".into(),
                }],
                breakpoints: vec![Breakpoint {
                    id: "1".into(),
                    location: "compute".into(),
                    enabled: true,
                }],
                values: vec![EvaluatedWatch {
                    id: request.id.clone(),
                    value: WatchValue {
                        expression: request.expression.clone(),
                        type_name: "int".into(),
                        display_value: "42".into(),
                        error: None,
                    },
                }],
                ..Default::default()
            },
            completes_command: false,
        }
        .into(),
    );
    assert!(
        ui.get_busy(),
        "an asynchronous update must not unlock a pending command"
    );
    assert!(ui.get_debug_halted());
    assert!(ui.get_debug_connected());
    assert_eq!(ui.get_watches().row_data(0).unwrap().value, "42");
    assert_eq!(ui.get_watches().row_data(0).unwrap().type_name, "int");
    assert_eq!(ui.get_stack_frames().row_count(), 1);
    assert_eq!(ui.get_breakpoints().row_count(), 1);
    ui.set_busy(false);
    ui.set_current_tab(4);
    redraw(&window, &mut pixels, 2960);
    if let Ok(path) = std::env::var("NYA_DEBUG_SNAPSHOT") {
        use std::io::Write;
        let mut file = std::fs::File::create(path)?;
        write!(file, "P6\n2960 1800\n255\n")?;
        let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
        file.write_all(&bytes)?;
    }

    // Live samples update rows in place and survive asynchronous running/log events.
    ui.set_live_enabled(true);
    ui.set_busy(true);
    let original_watches = ui.get_watches();
    let live_snapshot = DebugSnapshot {
        state: SessionState::Running,
        watch_sample: true,
        values: vec![EvaluatedWatch {
            id: request.id.clone(),
            value: WatchValue {
                expression: "counter".into(),
                type_name: "int".into(),
                display_value: "88".into(),
                error: None,
            },
        }],
        ..Default::default()
    };
    presenter.apply(
        &ui,
        nya_application::Event::DebugUpdated {
            snapshot: live_snapshot.clone(),
            completes_command: false,
        }
        .into(),
    );
    assert!(ui.get_busy());
    assert_eq!(ui.get_watches().row_data(0).unwrap().value, "88");
    assert!(ui.get_watch_sample_status().contains("已采样"));
    assert!(std::ptr::eq(
        original_watches.as_any(),
        ui.get_watches().as_any()
    ));
    presenter.apply(
        &ui,
        nya_application::Event::DebugUpdated {
            snapshot: DebugSnapshot {
                state: SessionState::Running,
                ..Default::default()
            },
            completes_command: false,
        }
        .into(),
    );
    assert_eq!(ui.get_watches().row_data(0).unwrap().value, "88");
    ui.set_live_enabled(false);
    presenter.live_refresh_changed(&ui);
    assert_eq!(ui.get_watches().row_data(0).unwrap().value, "—");
    presenter.apply(
        &ui,
        nya_application::Event::DebugUpdated {
            snapshot: live_snapshot,
            completes_command: false,
        }
        .into(),
    );
    assert_eq!(
        ui.get_watches().row_data(0).unwrap().value,
        "—",
        "late samples after disable must be discarded"
    );
    ui.set_live_enabled(true);
    presenter.apply(
        &ui,
        nya_application::Event::DebugUpdated {
            snapshot: DebugSnapshot {
                state: SessionState::Halted,
                watch_sample: true,
                watch_sample_error: Some("断点停止".into()),
                ..Default::default()
            },
            completes_command: false,
        }
        .into(),
    );
    assert!(!ui.get_live_enabled());
    assert!(ui.get_watch_sample_status().contains("断点停止"));
    ui.set_busy(false);
    println!("Live samples, stable list models, disable and sampling-error UI checks passed");

    presenter.remove_watch(&ui, 0);
    presenter.add_expression(&ui, "other_variable");
    presenter.apply(
        &ui,
        nya_application::Event::DebugUpdated {
            snapshot: DebugSnapshot {
                state: SessionState::Halted,
                values: vec![EvaluatedWatch {
                    id: request.id,
                    value: WatchValue {
                        expression: "counter".into(),
                        type_name: "int".into(),
                        display_value: "99".into(),
                        error: None,
                    },
                }],
                ..Default::default()
            },
            completes_command: false,
        }
        .into(),
    );
    assert_eq!(
        ui.get_watches().row_data(0).unwrap().value,
        "—",
        "late results must not populate a replacement watch"
    );
    presenter.apply(
        &ui,
        nya_application::Event::DebugUpdated {
            snapshot: DebugSnapshot {
                state: SessionState::Running,
                ..Default::default()
            },
            completes_command: true,
        }
        .into(),
    );
    assert!(!ui.get_debug_halted());
    assert!(ui.get_debug_running());
    assert_eq!(ui.get_watches().row_data(0).unwrap().value, "—");
    redraw(&window, &mut pixels, 2960);
    presenter.apply(
        &ui,
        nya_application::Event::DebugUpdated {
            snapshot: DebugSnapshot::default(),
            completes_command: true,
        }
        .into(),
    );
    assert!(!ui.get_debug_connected());
    assert!(!ui.get_debug_running());
    redraw(&window, &mut pixels, 2960);
    println!(
        "Debug-page rendering, control states, runtime values and stale-result protection passed"
    );
    Ok(())
}
