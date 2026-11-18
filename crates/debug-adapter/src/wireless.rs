//! Wireless debugging through EmberMobile. The TCP bootstrap never receives secrets.
mod store;
mod transport;

use crate::{error, DebugAdapter, Error, InstallProgress, Result, SharedInstallProgress};
use nya_domain::{
    DebugAction, DebugSession, TargetProfile, WirelessConfig, WirelessDevice, WirelessRegistry,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    path::PathBuf,
    time::{Duration, Instant},
};
use store::{SavedDevice, Store};
use transport::Client;

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .map_err(error)
}

fn bootstrap(endpoint: &str) -> Result<(SocketAddr, Value)> {
    let mut socket = None;
    for address in endpoint.to_socket_addrs().map_err(error)? {
        if let Ok(stream) = TcpStream::connect_timeout(&address, Duration::from_secs(3)) {
            socket = Some(stream);
            break;
        }
    }
    let mut stream =
        socket.ok_or_else(|| Error("无法连接手机 Ember 服务，请检查地址和局域网".into()))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(error)?;
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .map_err(error)?;
    let address = stream.peer_addr().map_err(error)?;
    stream
        .write_all(
            b"{\"id\":1,\"version\":1,\"token\":\"\",\"method\":\"v2_info\",\"params\":{}}\n",
        )
        .map_err(error)?;
    let mut bytes = Vec::new();
    BufReader::new(stream)
        .take(65536)
        .read_until(b'\n', &mut bytes)
        .map_err(error)?;
    if bytes.last() != Some(&b'\n') {
        return Err(Error("手机发现响应过长或不完整".into()));
    }
    let response: Value = serde_json::from_slice(&bytes).map_err(error)?;
    if response["id"] != 1 || response["ok"] != true {
        return Err(Error("手机不支持 Ember v2 发现，请升级 App".into()));
    }
    let advertisement = response["result"]["v2"].clone();
    if !advertisement["device"]["id"]
        .as_str()
        .is_some_and(|id| id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(Error("手机未提供有效的稳定设备 ID".into()));
    }
    Ok((address, advertisement))
}

fn authenticated(
    device: &SavedDevice,
    endpoint: &str,
    runtime: &tokio::runtime::Runtime,
) -> Result<(SocketAddr, Client)> {
    let (address, advertisement) = bootstrap(endpoint)?;
    if advertisement["device"]["id"] != device.id {
        return Err(Error("该地址的手机与已配对设备不一致".into()));
    }
    let client = runtime.block_on(Client::connect(address, &advertisement, &device.token))?;
    Ok((address, client))
}

impl DebugAdapter {
    fn wireless_directory(&self) -> PathBuf {
        self.manager.config_path.with_file_name("wireless")
    }

    fn wireless_idle(&self) -> Result<()> {
        if self.server.is_some() || self.session.client.is_some() || self.wireless.is_some() {
            return Err(Error("请先断开当前调试会话".into()));
        }
        Ok(())
    }

    fn open_wireless(
        &self,
        config: &WirelessConfig,
        profile: &TargetProfile,
    ) -> Result<RemoteSession> {
        self.wireless_idle()?;
        if profile.target.trim().is_empty()
            || !(1..=50000).contains(&config.speed_khz)
            || !matches!(config.protocol.as_str(), "swd" | "jtag")
        {
            return Err(Error(
                "请填写芯片型号、1–50000 kHz 调试速率和 swd/jtag 协议".into(),
            ));
        }
        let device = Store::open(&self.wireless_directory())?
            .devices
            .into_iter()
            .find(|p| p.id == config.device_id)
            .ok_or_else(|| Error("手机尚未配对，请先配对".into()))?;
        let endpoint = config.endpoint.as_deref().unwrap_or(&device.endpoint);
        let runtime = runtime()?;
        let (address, client) = authenticated(&device, endpoint, &runtime)?;
        let state = runtime.block_on(client.call("backend_state", json!({})))?;
        let connection = &state["connection"];
        let session_id = connection["session_id"]
            .as_i64()
            .ok_or_else(|| Error("请先在手机上授权并连接 USB 调试探针".into()))?;
        if state["gdb"]["running"] == true || state["flash"]["state"] == "running" {
            return Err(Error("手机正在被 GDB 或烧录任务占用".into()));
        }
        let mut remote = RemoteSession {
            client,
            runtime,
            address,
            session_id,
            owns_target: false,
            owns_gdb: false,
        };
        if connection["attached"] == true {
            if !connection["chip"]
                .as_str()
                .is_some_and(|chip| chip.eq_ignore_ascii_case(&profile.target))
            {
                return Err(Error(
                    "手机当前目标芯片与所选型号不一致，请先在手机断开目标".into(),
                ));
            }
        } else {
            remote.call(
                "attach",
                json!({"chip":profile.target,"speed_khz":config.speed_khz,
                "protocol":config.protocol,"under_reset":config.under_reset}),
            )?;
            remote.owns_target = true;
        }
        Ok(remote)
    }

    pub(super) fn start_wireless_server(
        &mut self,
        profile: &TargetProfile,
        config: &WirelessConfig,
    ) -> Result<()> {
        self.wireless_idle()?;
        if profile.gdb_port < 1024 || profile.gdb_port == 4242 {
            return Err(Error("GDB 端口必须为 1024–65535，且不能为 4242".into()));
        }
        let mut remote = self.open_wireless(config, profile)?;
        remote.call(
            "gdb_start",
            json!({"core":config.core,"port":profile.gdb_port}),
        )?;
        remote.owns_gdb = true;
        remote.address.set_port(profile.gdb_port);
        self.wireless = Some(remote);
        Ok(())
    }

    pub(super) fn flash_wireless(
        &mut self,
        profile: TargetProfile,
        config: WirelessConfig,
    ) -> Result<()> {
        self.wireless_idle()?;
        if config.core != 0 {
            return Err(Error(
                "手机独立烧录接口仅支持复位核心 0；其他核心请连接 GDB 后下载".into(),
            ));
        }
        let format = match profile
            .firmware
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "elf" | "axf" => "elf",
            "hex" => "hex",
            _ => return Err(Error("无线烧录请选择 ELF、AXF 或 HEX 固件".into())),
        };
        let file = File::open(&profile.firmware).map_err(error)?;
        let size = file.metadata().map_err(error)?.len();
        if size == 0 || size > 64 * 1024 * 1024 {
            return Err(Error("固件大小必须为 1–64 MiB".into()));
        }
        // Hash and upload the same bounded snapshot, even if a build replaces the file.
        let mut bytes = Vec::new();
        file.take(64 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(error)?;
        if bytes.len() as u64 != size {
            return Err(Error("读取期间固件大小发生变化，请重试".into()));
        }
        progress(&self.install_progress, "正在连接无线调试器…", None);
        let mut remote = self.open_wireless(&config, &profile)?;
        let upload = remote.call(
            "firmware_begin",
            json!({"name":profile.firmware.file_name().unwrap().to_string_lossy(),
            "size":bytes.len(),"sha256":format!("{:x}", Sha256::digest(&bytes))}),
        )?;
        let id = upload["id"]
            .as_u64()
            .ok_or_else(|| Error("手机未返回固件上传 ID".into()))?;
        let transfer = (|| {
            remote
                .runtime
                .block_on(remote.client.upload(id, &bytes, |sent| {
                    progress(
                        &self.install_progress,
                        "正在上传固件…",
                        Some(20.0 * sent as f32 / bytes.len() as f32),
                    );
                }))?;
            let finished = remote.call("firmware_finish", json!({"upload_id":id}))?;
            if finished["ready"] != true {
                return Err(Error("手机未确认固件完整性".into()));
            }
            Ok(())
        })();
        if let Err(error) = transfer {
            let _ = remote.call("firmware_abort", json!({"upload_id":id}));
            return Err(error);
        }
        // Once flash is submitted, a lost reply is ambiguous. Never detach a
        // potentially running flash task, including when its job ID was lost.
        let owned_target = remote.owns_target;
        remote.owns_target = false;
        let job = remote.call(
            "flash",
            json!({"firmware_id":id,"format":format,"run_after":true}),
        )?["job_id"]
            .as_u64()
            .ok_or_else(|| Error("手机未返回烧录任务 ID，目标状态请在手机确认".into()))?;
        remote.wait_flash(job, &self.install_progress)?;
        remote.owns_target = owned_target;
        remote
            .stop()
            .map_err(|e| Error(format!("固件已烧录并复位，但清理手机会话失败：{e}")))?;
        progress(
            &self.install_progress,
            "固件已无线烧录并自动复位",
            Some(100.0),
        );
        Ok(())
    }
}

fn progress(shared: &SharedInstallProgress, stage: &str, percent: Option<f32>) {
    if let Ok(mut value) = shared.lock() {
        *value = InstallProgress {
            stage: stage.into(),
            percent,
            ..Default::default()
        };
    }
}

impl WirelessRegistry for DebugAdapter {
    fn paired_wireless_devices(&self) -> Result<Vec<WirelessDevice>> {
        Ok(Store::open(&self.wireless_directory())?
            .devices
            .iter()
            .map(SavedDevice::public)
            .collect())
    }

    fn pair_wireless(
        &mut self,
        endpoint: &str,
        name: &str,
        on_code: &mut dyn FnMut(&str),
    ) -> Result<WirelessDevice> {
        self.wireless_idle()?;
        if name.is_empty() || name.len() > 96 || name.chars().any(char::is_control) {
            return Err(Error("电脑名称必须为 1–96 字节，且不能含控制字符".into()));
        }
        let (address, advertisement) = bootstrap(endpoint)?;
        let runtime = runtime()?;
        let existing = Store::open(&self.wireless_directory())?
            .devices
            .into_iter()
            .find(|p| advertisement["device"]["id"] == p.id);
        if let Some(mut saved) = existing {
            if let Ok(client) =
                runtime.block_on(Client::connect(address, &advertisement, &saved.token))
            {
                runtime.block_on(client.close());
                saved.endpoint = endpoint.into();
                let mut store = Store::open(&self.wireless_directory())?;
                if let Some(device) = store
                    .devices
                    .iter_mut()
                    .find(|p| p.id == saved.id && p.token == saved.token)
                {
                    *device = saved.clone();
                    store.save()?;
                    return Ok(saved.public());
                }
            }
        }
        let (identity, token) =
            runtime.block_on(Client::pair(address, &advertisement, name, on_code))?;
        let saved = SavedDevice {
            id: identity["server_id"]
                .as_str()
                .ok_or_else(|| Error("缺少手机 ID".into()))?
                .into(),
            name: identity["name"]
                .as_str()
                .ok_or_else(|| Error("缺少手机名称".into()))?
                .into(),
            endpoint: endpoint.into(),
            token,
        };
        let mut store = Store::open(&self.wireless_directory())?;
        store.devices.retain(|p| p.id != saved.id);
        store.devices.push(saved.clone());
        store.save()?;
        Ok(saved.public())
    }

    fn forget_wireless(&mut self, id: &str) -> Result<bool> {
        self.wireless_idle()?;
        let saved = Store::open(&self.wireless_directory())?
            .devices
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| Error("未找到已配对设备".into()))?;
        let revoke = (|| -> Result<()> {
            let runtime = runtime()?;
            let (_, client) = authenticated(&saved, &saved.endpoint, &runtime)?;
            runtime.block_on(client.exchange(transport::FORGET, &[]))?;
            runtime.block_on(client.close());
            Ok(())
        })();
        let mut store = Store::open(&self.wireless_directory())?;
        store
            .devices
            .retain(|p| p.id != id || p.token != saved.token);
        store.save()?;
        Ok(revoke.is_ok())
    }
}

pub(super) struct RemoteSession {
    client: Client,
    runtime: tokio::runtime::Runtime,
    pub address: SocketAddr,
    session_id: i64,
    owns_target: bool,
    owns_gdb: bool,
}

impl RemoteSession {
    fn call(&self, method: &str, mut params: Value) -> Result<Value> {
        if matches!(
            method,
            "attach" | "detach" | "gdb_start" | "gdb_stop" | "flash"
        ) {
            params["session_id"] = json!(self.session_id);
        }
        self.runtime.block_on(self.client.call(method, params))
    }
    pub fn check(&self) -> Result<()> {
        if let Some(reason) = self.client.connection.close_reason() {
            return Err(error(reason));
        }
        Ok(())
    }
    fn wait_flash(&self, job: u64, shared: &SharedInstallProgress) -> Result<()> {
        let start = Instant::now();
        loop {
            let state = self.call("backend_state", json!({})).map_err(|e| {
                Error(format!(
                    "烧录状态查询失败，手机可能仍在烧录，请检查后重连：{e}"
                ))
            })?;
            if state["connection"]["session_id"] != self.session_id || state["flash"]["id"] != job {
                return Err(Error("手机烧录任务或探针会话已变化，请检查目标状态".into()));
            }
            let flash = &state["flash"];
            match flash["state"].as_str() {
                Some("succeeded") => return Ok(()),
                Some("failed") => return Err(Error(format!("无线烧录失败：{}", flash["error"]))),
                Some("running") => {}
                _ => return Err(Error("手机返回无效的烧录任务状态".into())),
            }
            let percent = flash["done"]
                .as_f64()
                .zip(flash["total"].as_f64())
                .filter(|(_, total)| *total > 0.0)
                .map(|(done, total)| (20.0 + 79.0 * (done / total).clamp(0.0, 1.0)) as f32);
            progress(
                shared,
                &format!("无线烧录：{}", flash["stage"].as_str().unwrap_or("处理中")),
                percent,
            );
            if start.elapsed() >= Duration::from_secs(900) {
                return Err(Error(
                    "等待烧录超时，手机任务可能仍在运行，请在手机检查状态".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    pub fn stop(&mut self) -> Result<()> {
        let mut failure = None;
        if self.owns_gdb {
            match self.call("gdb_stop", json!({})) {
                Ok(_) => self.owns_gdb = false,
                Err(e) => failure = Some(e),
            }
        }
        if self.owns_target && !self.owns_gdb {
            match self.call("detach", json!({})) {
                Ok(_) => self.owns_target = false,
                Err(e) => {
                    failure.get_or_insert(e);
                }
            }
        }
        self.runtime.block_on(self.client.close());
        // A failed cleanup needs explicit user recovery on the phone. Avoid
        // retrying mutations implicitly from Drop after an uncertain response.
        self.owns_gdb = false;
        self.owns_target = false;
        match failure {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}
impl Drop for RemoteSession {
    fn drop(&mut self) {
        if self.owns_target || self.owns_gdb {
            let _ = self.stop();
        }
    }
}

impl Drop for DebugAdapter {
    fn drop(&mut self) {
        if self.wireless.is_some() {
            let _ = self.execute_debug(DebugAction::Disconnect);
        }
    }
}

#[cfg(test)]
mod tests;
