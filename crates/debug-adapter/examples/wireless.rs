//! A hardware-facing backend example; pairing approval happens on the phone.
use nya_debug_adapter::DebugAdapter;
use nya_domain::{
    DebugAction, DebugSession, TargetProfile, ToolKind, WatchRequest, WirelessConfig,
    WirelessRegistry,
};
use std::{
    env,
    io::{self, Write},
    path::PathBuf,
    time::{Duration, Instant},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args().skip(1).collect();
    let mut adapter = DebugAdapter::new()?;
    match args.first().map(String::as_str) {
        Some("paired") if args.len() == 1 => {
            for device in adapter.paired_wireless_devices()? {
                println!("{}  {}  {}", device.id, device.name, device.endpoint);
            }
        }
        Some("pair") if (2..=3).contains(&args.len()) => {
            let device = adapter.pair_wireless(&args[1], args.get(2).map(String::as_str).unwrap_or("NyaScraping"), &mut |code| {
                eprintln!("配对校验码：{code}；请在手机核对并同意配对（120 秒内）。");
            })?;
            println!("已配对：{}  {}", device.id, device.name);
        }
        Some("forget") if args.len() == 2 => {
            println!("本机配对已删除；手机端撤销：{}", adapter.forget_wireless(&args[1])?);
        }
        Some(command @ ("flash" | "debug")) if (4..=6).contains(&args.len()) => {
            let mut config = WirelessConfig::new(&args[1]);
            config.endpoint = args.get(5).cloned();
            let profile = TargetProfile {
                name: "EmberMobile".into(), tool: ToolKind::ProbeRs,
                target: args[2].clone(), interface: None, transport: None, gdb_port: 3333,
                firmware: PathBuf::from(&args[3]),
                gdb_executable: args.get(4).map(PathBuf::from).unwrap_or_else(|| "arm-none-eabi-gdb".into()),
            };
            if command == "flash" {
                adapter.execute_debug(DebugAction::FlashWireless { profile, connection: config })?;
                println!("固件已烧录并复位运行。");
            } else {
                adapter.execute_debug(DebugAction::ConnectWireless { profile, connection: config, reset: false })?;
                interactive(&mut adapter)?;
            }
        }
        _ => return Err("Usage: wireless pair <ip:4242> [computer-name] | paired | forget <id> | flash/debug <id> <chip> <firmware> [gdb] [ip:4242]".into()),
    }
    Ok(())
}

fn interactive(adapter: &mut DebugAdapter) -> Result<(), Box<dyn std::error::Error>> {
    println!("命令：continue, pause, step, next, out, reset, download, break <位置>, watch <表达式>, status, quit");
    loop {
        print!("wireless> ");
        io::stdout().flush()?;
        let mut line = String::new();
        if io::stdin().read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim();
        if line == "quit" {
            break;
        }
        let action = match line {
            "continue" => Some(DebugAction::Continue),
            "pause" => Some(DebugAction::Pause),
            "step" => Some(DebugAction::StepInto),
            "next" => Some(DebugAction::StepOver),
            "out" => Some(DebugAction::StepOut),
            "reset" => Some(DebugAction::Reset),
            "download" => Some(DebugAction::Download),
            "status" | "" => None,
            text if text.starts_with("break ") => {
                Some(DebugAction::AddBreakpoint(text[6..].into()))
            }
            text if text.starts_with("watch ") => {
                let watch = WatchRequest {
                    id: "example".into(),
                    expression: text[6..].into(),
                    function: None,
                    address: None,
                };
                println!("{:?}", adapter.debug_snapshot(&[watch]).values);
                None
            }
            _ => {
                eprintln!("未知命令");
                continue;
            }
        };
        if let Some(action) = action {
            if let Err(error) = adapter.execute_debug(action) {
                eprintln!("{error}");
            }
        }
        // Let asynchronous MI stop notifications and stack updates arrive.
        let deadline = Instant::now() + Duration::from_millis(150);
        while Instant::now() < deadline {
            adapter.poll_debug()?;
            std::thread::sleep(Duration::from_millis(10));
        }
        let snapshot = adapter.debug_snapshot(&[]);
        println!("{:?} {}", snapshot.state, snapshot.message);
        for frame in snapshot.frames {
            println!(
                "  #{} {} {}:{}",
                frame.level, frame.function, frame.file, frame.line
            );
        }
    }
    adapter.execute_debug(DebugAction::Disconnect)?;
    Ok(())
}
