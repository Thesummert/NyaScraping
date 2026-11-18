//! Offscreen wireless configuration check; optional argument is a screenshot prefix.
#![allow(dead_code)]
use nya_domain::WirelessDevice;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PlatformError, WindowAdapter};
use slint::{ComponentHandle, Rgb8Pixel};
use std::{rc::Rc, time::Duration};

#[path = "../src/acquisition.rs"]
mod acquisition;
#[path = "../src/firmware_browser.rs"]
mod firmware_browser;
#[path = "../src/presenter.rs"]
mod presenter;
#[path = "../src/watch_list.rs"]
mod watch_list;
#[path = "../src/waveform_view.rs"]
mod waveform_view;
slint::include_modules!();

struct TestPlatform(Rc<MinimalSoftwareWindow>);
impl Platform for TestPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.0.clone())
    }
}

fn render(
    window: &MinimalSoftwareWindow,
    width: usize,
    height: usize,
    screenshot: Option<String>,
) -> Vec<Rgb8Pixel> {
    slint::platform::update_timers_and_animations();
    let mut pixels = vec![Rgb8Pixel::default(); width * height];
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, width);
    });
    if let Some(path) = screenshot {
        let bytes: Vec<_> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
        image::save_buffer(
            path,
            &bytes,
            width as u32,
            height as u32,
            image::ColorType::Rgb8,
        )
        .unwrap();
    }
    pixels
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let prefix = std::env::args().nth(1);
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(window.clone())))?;
    let ui = AppWindow::new()?;
    ui.set_project_dialog_open(false);
    ui.set_wireless_enabled(true);
    ui.set_settings_tab(2);
    ui.set_settings_open(true);
    ui.set_wireless_target("STM32H723ZGTx".into());
    let mut presenter = presenter::Presenter::default();
    let devices = vec![WirelessDevice {
        id: "a".repeat(64),
        name: "EmberMobile 手机".into(),
        endpoint: "192.168.1.100:4242".into(),
    }];
    presenter.apply(
        &ui,
        presenter::PreparedEvent::Wireless(
            devices.clone(),
            Some(devices[0].id.clone()),
            "配对成功".into(),
        ),
    );
    assert_eq!(ui.get_wireless_device_index(), 0);
    assert_eq!(ui.get_wireless_endpoint(), "192.168.1.100:4242");
    ui.set_wireless_endpoint("192.168.1.101:4242".into());
    presenter.apply(
        &ui,
        presenter::PreparedEvent::Wireless(devices.clone(), None, "列表已刷新".into()),
    );
    assert_eq!(
        ui.get_wireless_endpoint(),
        "192.168.1.101:4242",
        "refresh must preserve an edited phone address"
    );
    ui.show()?;
    window.set_size(slint::PhysicalSize::new(1480, 900));
    render(&window, 1480, 900, None);
    std::thread::sleep(Duration::from_millis(280));
    let settings = render(
        &window,
        1480,
        900,
        prefix.as_ref().map(|p| format!("{p}-settings.png")),
    );
    ui.set_busy(true);
    ui.set_wireless_pairing(true);
    ui.set_wireless_comparison_code("12345678".into());
    let pairing = render(
        &window,
        1480,
        900,
        prefix.as_ref().map(|p| format!("{p}-pairing.png")),
    );
    assert_ne!(
        settings, pairing,
        "the comparison code must appear while the worker awaits phone approval"
    );
    assert!(ui.get_busy() && ui.get_wireless_pairing());
    window.set_size(slint::PhysicalSize::new(1180, 720));
    render(
        &window,
        1180,
        720,
        prefix.as_ref().map(|p| format!("{p}-small.png")),
    );
    ui.set_busy(false);
    ui.set_wireless_pairing(false);
    ui.set_wireless_device_id("unpaired-project-phone".into());
    presenter.apply(
        &ui,
        presenter::PreparedEvent::Wireless(devices, None, "需要在此电脑配对".into()),
    );
    assert_eq!(
        ui.get_wireless_device_index(),
        -1,
        "never silently select another phone for a project"
    );
    presenter.apply(
        &ui,
        presenter::PreparedEvent::Wireless(Vec::new(), Some(String::new()), "已删除配对".into()),
    );
    assert!(ui.get_wireless_device_id().is_empty());
    assert_eq!(ui.get_wireless_device_index(), -1);
    println!("Wireless settings, pairing display, address preservation and device selection passed at 1480×900 and 1180×720.");
    Ok(())
}
