//! Time static firmware parsing: cargo run -p nya-firmware-backend --example parse_firmware -- FILE
use nya_domain::firmware::DebugMember;
use nya_firmware_backend::NativeFirmwareBackend;
use std::{path::Path, time::Instant};

fn member_count(members: &[DebugMember]) -> usize {
    members.iter().map(|m| 1 + member_count(&m.members)).sum()
}

fn main() {
    let path = std::env::args_os().nth(1).expect("provide a firmware path");
    let start = Instant::now();
    let mut stage = "";
    let image = NativeFirmwareBackend
        .load_with_progress(Path::new(&path), &mut |progress| {
            if progress.stage != stage {
                println!("{:.3}s: {}", start.elapsed().as_secs_f64(), progress.stage);
                stage = progress.stage;
            }
        })
        .expect("parse firmware");
    println!(
        "Parsed in {:.3}s: {} files, {} functions, {} variables, {} members, {} executable lines",
        start.elapsed().as_secs_f64(),
        image.debug.files.len(),
        image.debug.functions.len(),
        image.debug.variables.len(),
        image
            .debug
            .variables
            .iter()
            .map(|v| member_count(&v.members))
            .sum::<usize>(),
        image.debug.executable_lines.len(),
    );
    for warning in &image.debug.warnings {
        println!("Warning: {warning}");
    }
}
