use crate::{core_tool, error, DebugAdapter};
use nya_domain::{DebugTarget, Error, Result, TargetCatalog, ToolKind, ToolRegistry};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

const QUERY_TIMEOUT: Duration = Duration::from_secs(30);
const OUTPUT_LIMIT: u64 = 16 * 1024 * 1024;

// File-backed output avoids pipe deadlocks, including on verbose pack downloads.
fn run(mut command: Command, timeout: Duration) -> Result<String> {
    let mut stdout = tempfile::tempfile().map_err(error)?;
    let mut stderr = tempfile::tempfile().map_err(error)?;
    command.stdout(stdout.try_clone().map_err(error)?);
    command.stderr(stderr.try_clone().map_err(error)?);
    command.env("NO_COLOR", "1").env("CLICOLOR", "0");
    let mut child = command.spawn().map_err(error)?;
    let started = Instant::now();
    let status = loop {
        let result = (|| {
            if stdout.metadata().map_err(error)?.len() > OUTPUT_LIMIT
                || stderr.metadata().map_err(error)?.len() > OUTPUT_LIMIT
            {
                return Err(Error("工具输出超过 16 MiB 限制".into()));
            }
            if started.elapsed() >= timeout {
                return Err(Error(format!("工具执行超时（{} 秒）", timeout.as_secs())));
            }
            child.try_wait().map_err(error)
        })();
        match result {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        }
    };
    stdout.seek(SeekFrom::Start(0)).map_err(error)?;
    stderr.seek(SeekFrom::Start(0)).map_err(error)?;
    let mut output = String::new();
    stdout
        .take(OUTPUT_LIMIT + 1)
        .read_to_string(&mut output)
        .map_err(error)?;
    let mut diagnostic = String::new();
    stderr
        .take(OUTPUT_LIMIT + 1)
        .read_to_string(&mut diagnostic)
        .map_err(error)?;
    if output.len() as u64 > OUTPUT_LIMIT || diagnostic.len() as u64 > OUTPUT_LIMIT {
        return Err(Error("工具输出超过 16 MiB 限制".into()));
    }
    if !status.success() {
        let detail: String = format!("{}\n{}", diagnostic.trim(), output.trim())
            .chars()
            .take(2000)
            .collect();
        return Err(Error(format!("工具退出：{status} · {detail}")));
    }
    Ok(output)
}

fn pyocd_targets(executable: &Path, packs: &[PathBuf]) -> Result<Vec<DebugTarget>> {
    let mut command = Command::new(executable);
    command.args(["json", "--targets"]);
    append_packs(&mut command, packs);
    parse_pyocd(&run(command, QUERY_TIMEOUT)?)
}

fn append_packs(command: &mut Command, packs: &[PathBuf]) {
    for pack in packs {
        command.arg("--pack").arg(pack);
    }
}

fn parse_pyocd(output: &str) -> Result<Vec<DebugTarget>> {
    let value: serde_json::Value = serde_json::from_str(output).map_err(error)?;
    if value["status"].as_i64() != Some(0) {
        return Err(Error(format!("pyOCD 目标加载失败：{}", value["error"])));
    }
    let rows = value["targets"]
        .as_array()
        .ok_or_else(|| Error("pyOCD 未返回目标列表".into()))?;
    rows.iter()
        .map(|row| {
            let name = row["name"]
                .as_str()
                .filter(|name| !name.is_empty())
                .ok_or_else(|| Error("pyOCD 返回的目标缺少名称".into()))?;
            Ok(DebugTarget {
                name: name.into(),
                vendor: row["vendor"].as_str().unwrap_or_default().into(),
                source: row["source"].as_str().unwrap_or("pyOCD").into(),
            })
        })
        .collect()
}

fn parse_probe_rs(output: &str) -> Result<Vec<DebugTarget>> {
    let targets: Vec<_> = output
        .lines()
        .filter(|line| line.starts_with("        ") || line.starts_with("\t\t"))
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.ends_with(':'))
        .map(|name| DebugTarget {
            name: name.into(),
            source: "probe-rs".into(),
            ..Default::default()
        })
        .collect();
    if targets.is_empty() {
        return Err(Error(
            "probe-rs 未返回可识别的芯片列表，可手动填写目标".into(),
        ));
    }
    Ok(targets)
}

fn openocd_configs(executable: &Path, category: &str) -> Result<Vec<DebugTarget>> {
    let mut roots = Vec::new();
    if let Some(paths) = std::env::var_os("OPENOCD_SCRIPTS") {
        roots.extend(std::env::split_paths(&paths));
    }
    for executable in [
        executable.to_path_buf(),
        fs::canonicalize(executable).map_err(error)?,
    ] {
        for ancestor in executable.ancestors().skip(1).take(4) {
            for suffix in ["share/openocd/scripts", "openocd/scripts", "scripts"] {
                roots.push(ancestor.join(suffix));
            }
        }
    }
    let mut targets = Vec::new();
    let mut scanned = std::collections::HashSet::new();
    for root in roots {
        let directory = root.join(category);
        if directory.is_dir() && scanned.insert(fs::canonicalize(&directory).map_err(error)?) {
            scan_configs(&directory, &root, 0, &mut targets)?;
        }
    }
    if targets.is_empty() {
        return Err(Error(format!(
            "未找到 OpenOCD {category}/*.cfg；可手动填写，或设置 OPENOCD_SCRIPTS 后刷新"
        )));
    }
    Ok(targets)
}

fn scan_configs(
    directory: &Path,
    root: &Path,
    depth: usize,
    targets: &mut Vec<DebugTarget>,
) -> Result<()> {
    if depth > 8 || targets.len() > 50_000 {
        return Err(Error("OpenOCD 配置数量或目录深度超限".into()));
    }
    for entry in fs::read_dir(directory).map_err(error)? {
        let entry = entry.map_err(error)?;
        let kind = entry.file_type().map_err(error)?;
        let path = entry.path();
        if kind.is_dir() {
            scan_configs(&path, root, depth + 1, targets)?;
        } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "cfg") {
            targets.push(DebugTarget {
                name: fs::canonicalize(&path)
                    .map_err(error)?
                    .to_string_lossy()
                    .into(),
                vendor: "OpenOCD".into(),
                source: path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/"),
            });
        }
    }
    Ok(())
}

impl DebugAdapter {
    fn packs_path(&self) -> PathBuf {
        self.manager
            .config_path
            .with_file_name("desktop-packs.json")
    }

    fn packs(&self) -> Result<Vec<PathBuf>> {
        let path = self.packs_path();
        let file = match fs::File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(error(e)),
        };
        if file.metadata().map_err(error)?.len() > 1024 * 1024 {
            return Err(Error("芯片包配置文件过大".into()));
        }
        serde_json::from_reader(file).map_err(error)
    }

    fn save_packs(&self, packs: &[PathBuf]) -> Result<()> {
        let path = self.packs_path();
        let parent = path
            .parent()
            .ok_or_else(|| Error("无效的配置目录".into()))?;
        fs::create_dir_all(parent).map_err(error)?;
        let mut file = tempfile::NamedTempFile::new_in(parent).map_err(error)?;
        serde_json::to_writer_pretty(&mut file, packs).map_err(error)?;
        file.flush().map_err(error)?;
        file.persist(path).map_err(error)?;
        Ok(())
    }

    fn require_pyocd(&self) -> Result<PathBuf> {
        if self.server.is_some() || self.session.client.is_some() {
            return Err(Error("请断开调试会话后管理芯片包".into()));
        }
        if self.active_backend()? != Some(ToolKind::PyOcd) {
            return Err(Error("请先选择 pyOCD 后端".into()));
        }
        self.manager
            .resolve(core_tool(ToolKind::PyOcd))
            .map_err(error)
    }

    pub(super) fn load_targets(&self) -> Result<TargetCatalog> {
        let Some(tool) = self.active_backend()? else {
            return Ok(TargetCatalog {
                message: "请先选择调试后端".into(),
                ..Default::default()
            });
        };
        let mut catalog = TargetCatalog::default();
        if tool == ToolKind::PyOcd {
            catalog.packs = self.packs()?;
        }
        let result = self
            .manager
            .resolve(core_tool(tool))
            .map_err(error)
            .and_then(|executable| match tool {
                ToolKind::PyOcd => pyocd_targets(&executable, &catalog.packs),
                ToolKind::OpenOcd => {
                    match openocd_configs(&executable, "interface") {
                        Ok(mut probes) => {
                            probes.sort_by_cached_key(|probe| probe.source.to_lowercase());
                            probes.dedup_by(|a, b| a.name == b.name);
                            catalog.probe_message = format!("{} 个可选 Probe 配置", probes.len());
                            catalog.probes = probes;
                        }
                        Err(error) => {
                            catalog.probe_message = format!("Probe 配置加载失败：{error}")
                        }
                    }
                    openocd_configs(&executable, "target")
                }
                ToolKind::ProbeRs => {
                    let mut command = Command::new(executable);
                    command.args(["chip", "list"]);
                    parse_probe_rs(&run(command, QUERY_TIMEOUT)?)
                }
            });
        match result {
            Ok(mut targets) => {
                targets.sort_by_cached_key(|target| target.name.to_lowercase());
                targets.dedup_by(|a, b| a.name.eq_ignore_ascii_case(&b.name));
                catalog.message = format!("{} 个可选目标", targets.len());
                catalog.targets = targets;
            }
            Err(error) => {
                catalog.message = format!("目标加载失败：{error}");
                if tool == ToolKind::OpenOcd && catalog.probe_message.is_empty() {
                    catalog.probe_message = format!("Probe 配置加载失败：{error}");
                }
            }
        }
        Ok(catalog)
    }

    pub(super) fn add_local_pack(&self, path: &Path) -> Result<()> {
        let executable = self.require_pyocd()?;
        let path = fs::canonicalize(path).map_err(error)?;
        if !path.is_file()
            || !path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("pack"))
        {
            return Err(Error("请选择 CMSIS .pack 芯片包文件".into()));
        }
        let mut packs = self.packs()?;
        if packs.contains(&path) {
            return Ok(());
        }
        if packs.len() >= 128 {
            return Err(Error("最多添加 128 个本地芯片包".into()));
        }
        packs.push(path);
        // Only persist after the selected pyOCD successfully reads this pack.
        pyocd_targets(&executable, &packs)?;
        self.save_packs(&packs)
    }

    pub(super) fn remove_local_pack(&self, path: &Path) -> Result<()> {
        self.require_pyocd()?;
        let mut packs = self.packs()?;
        packs.retain(|pack| pack != path);
        self.save_packs(&packs)
    }

    pub(super) fn install_target_pack(&self, part: &str) -> Result<()> {
        let executable = self.require_pyocd()?;
        if part.is_empty()
            || part.len() > 128
            || part.starts_with('-')
            || !part
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-+./".contains(&c))
        {
            return Err(Error("请输入具体芯片型号，如 STM32H743ZITx".into()));
        }
        let mut command = Command::new(executable);
        command.args(["pack", "install", part]);
        run(command, Duration::from_secs(300))?;
        Ok(())
    }

    pub(super) fn configure_packs(&self, tool: ToolKind, command: &mut Command) -> Result<()> {
        if tool == ToolKind::PyOcd {
            append_packs(command, &self.packs()?);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nya_scraping_core::manager::Manager;

    #[test]
    fn target_formats_keep_real_names_and_reject_errors() {
        let targets = parse_pyocd(r#"{"status":0,"targets":[{"name":"stm32h743zitx","vendor":"STMicroelectronics","source":"pack"}]}"#).unwrap();
        assert_eq!(targets[0].name, "stm32h743zitx");
        assert_eq!(targets[0].source, "pack");
        for output in [
            r#"{"status":1,"error":"bad pack"}"#,
            r#"{"status":0}"#,
            r#"{"status":0,"targets":[{}]}"#,
            "not json",
        ] {
            assert!(parse_pyocd(output).is_err());
        }
        let targets = parse_probe_rs("Available chips:\nSTM32H7\n    Variants:\n        STM32H743ZITx\n        STM32H750VBTx\n").unwrap();
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].name, "STM32H743ZITx");
        assert!(parse_probe_rs("error or incompatible output").is_err());
    }

    #[test]
    fn openocd_configs_are_selected_by_absolute_path() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("bin/openocd");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(&executable, "fixture").unwrap();
        let target = root
            .path()
            .join("share/openocd/scripts/target/stm32h7x.cfg");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, "# fixture").unwrap();
        let targets = openocd_configs(&executable, "target").unwrap();
        let found = targets
            .iter()
            .find(|row| row.source == "target/stm32h7x.cfg")
            .unwrap();
        assert_eq!(Path::new(&found.name), fs::canonicalize(target).unwrap());
    }

    #[test]
    fn openocd_probe_configs_are_separate_and_reach_server_commands() {
        use nya_scraping_core::backend::{
            gdbserver_command, operation_command, GdbServerOptions, Operation,
        };
        let root = tempfile::tempdir().unwrap();
        let manager = Manager {
            config_path: root.path().join("config/config"),
            data_dir: root.path().join("tools"),
        };
        let installation = manager.data_dir.join("openocd/1.0/xpack-openocd-1.0");
        let executable = installation.join(if cfg!(windows) {
            "bin/openocd.exe"
        } else {
            "bin/openocd"
        });
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(&executable, "fixture").unwrap();
        let scripts = installation.join("share/openocd/scripts");
        for name in [
            "interface/nya-stlink.cfg",
            "interface/ftdi/nya probe.cfg",
            "target/nya-target.cfg",
        ] {
            let path = scripts.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "# fixture").unwrap();
        }
        let mut adapter = DebugAdapter {
            manager,
            server: None,
            session: Default::default(),
        };
        adapter.select(ToolKind::OpenOcd, "1.0").unwrap();
        let catalog = adapter.target_catalog().unwrap();
        assert!(catalog
            .probes
            .iter()
            .all(|probe| probe.source.starts_with("interface/")));
        assert!(catalog
            .targets
            .iter()
            .all(|target| target.source.starts_with("target/")));
        let probe = catalog
            .probes
            .iter()
            .find(|probe| probe.source == "interface/ftdi/nya probe.cfg")
            .unwrap();
        assert_eq!(
            catalog
                .probes
                .iter()
                .filter(|probe| probe.source == "interface/ftdi/nya probe.cfg")
                .count(),
            1
        );
        let target = catalog
            .targets
            .iter()
            .find(|target| target.source == "target/nya-target.cfg")
            .unwrap();
        let options = GdbServerOptions {
            target: target.name.clone(),
            interface: Some(probe.name.clone()),
            port: 3333,
        };
        for command in [
            gdbserver_command(core_tool(ToolKind::OpenOcd), &executable, &options).unwrap(),
            operation_command(
                core_tool(ToolKind::OpenOcd),
                &executable,
                &options,
                &Operation::Reset,
            )
            .unwrap(),
        ] {
            let args: Vec<_> = command.get_args().take(4).collect();
            assert_eq!(
                args,
                [
                    std::ffi::OsStr::new("-f"),
                    std::ffi::OsStr::new(&probe.name),
                    std::ffi::OsStr::new("-f"),
                    std::ffi::OsStr::new(&target.name)
                ]
            );
        }
    }

    // Real process boundaries verify argument quoting, persistence, and failed imports.
    #[cfg(unix)]
    #[test]
    fn local_packs_validate_persist_reload_and_reach_pyocd_commands() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let manager = Manager {
            config_path: root.path().join("config/config"),
            data_dir: root.path().join("tools"),
        };
        let executable = manager.data_dir.join("pyocd/1.0/bin/pyocd");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(
            &executable,
            r#"#!/usr/bin/env python3
import json, pathlib, sys
args = sys.argv[1:]
pathlib.Path(__file__).with_suffix('.args.json').write_text(json.dumps(args))
if args[:2] == ['pack', 'install']:
    print('installed')
else:
    packs = [args[i + 1] for i, v in enumerate(args) if v == '--pack']
    if any(not pathlib.Path(p).is_file() or pathlib.Path(p).name == 'invalid.pack' for p in packs):
        print(json.dumps({'status': 1, 'error': 'invalid pack'}))
        sys.exit(1)
    targets = [{'name': 'cortex_m', 'vendor': 'Arm', 'source': 'builtin'}]
    if packs:
        targets.extend([{'name': 'stm32h743zitx', 'vendor': 'ST', 'source': 'pack'}] * 2)
    print(json.dumps({'status': 0, 'targets': targets}))
"#,
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let mut adapter = DebugAdapter {
            manager,
            server: None,
            session: Default::default(),
        };
        adapter.select(ToolKind::PyOcd, "1.0").unwrap();
        let pack = root.path().join("芯片包 with spaces.pack");
        fs::write(&pack, "valid fixture").unwrap();
        adapter.add_pack(&pack).unwrap();
        adapter.add_pack(&pack).unwrap();
        let catalog = adapter.target_catalog().unwrap();
        assert_eq!(catalog.packs, [fs::canonicalize(&pack).unwrap()]);
        assert_eq!(catalog.targets.len(), 2); // Duplicate target names are removed.
        let invalid = root.path().join("invalid.pack");
        fs::write(&invalid, "invalid").unwrap();
        assert!(adapter.add_pack(&invalid).is_err());
        assert_eq!(adapter.packs().unwrap().len(), 1);
        let reopened = DebugAdapter {
            manager: Manager {
                config_path: adapter.manager.config_path.clone(),
                data_dir: adapter.manager.data_dir.clone(),
            },
            server: None,
            session: Default::default(),
        };
        assert_eq!(reopened.target_catalog().unwrap().packs, catalog.packs);
        let mut command = Command::new("pyocd");
        command.arg("gdbserver");
        reopened
            .configure_packs(ToolKind::PyOcd, &mut command)
            .unwrap();
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(
            args,
            [
                std::ffi::OsStr::new("gdbserver"),
                std::ffi::OsStr::new("--pack"),
                catalog.packs[0].as_os_str()
            ]
        );
        let mut other = Command::new("openocd");
        reopened
            .configure_packs(ToolKind::OpenOcd, &mut other)
            .unwrap();
        assert_eq!(other.get_args().count(), 0);
        assert!(adapter.install_pack("--help").is_err());
        assert!(adapter.install_pack("*").is_err());
        adapter.install_pack("STM32H743ZITx").unwrap();
        let args: Vec<String> =
            serde_json::from_slice(&fs::read(executable.with_extension("args.json")).unwrap())
                .unwrap();
        assert_eq!(args, ["pack", "install", "STM32H743ZITx"]);
        fs::rename(&pack, root.path().join("moved.pack")).unwrap();
        let catalog = adapter.target_catalog().unwrap();
        assert!(catalog.targets.is_empty());
        assert_eq!(catalog.packs.len(), 1); // Missing files remain removable.
        adapter.remove_pack(&catalog.packs[0]).unwrap();
        assert!(adapter.packs().unwrap().is_empty());
        assert!(root.path().join("moved.pack").exists());
    }

    #[test]
    fn query_timeout_terminates_the_child() {
        if Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let mut command = Command::new("python3");
        command.args(["-c", "import time; time.sleep(10)"]);
        let started = Instant::now();
        assert!(run(command, Duration::from_millis(50))
            .unwrap_err()
            .0
            .contains("超时"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn installed_pyocd_can_enumerate_targets_without_a_probe() {
        if Command::new("pyocd").arg("--version").output().is_err() {
            return;
        }
        let targets = pyocd_targets(Path::new("pyocd"), &[]).unwrap();
        assert!(targets.iter().any(|target| target.name == "cortex_m"));
    }
}
