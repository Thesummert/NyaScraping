use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

#[derive(Default)]
struct PhoneState {
    token: Option<String>,
    comparison: String,
    calls: Vec<(String, Value)>,
    attached: bool,
    gdb: bool,
    upload: Vec<u8>,
    upload_size: u64,
    sha256: String,
    job: bool,
    reject_pairing: bool,
    fail_flash: bool,
    replace_job: bool,
    reject_upload: bool,
    fail_gdb: bool,
    short_memory: bool,
    wrong_memory_kind: bool,
    memory_delay_ms: u64,
    current_session: Option<i64>,
}

/// A real TLS/QUIC peer and a strict TCP discovery endpoint. No USB or MCU.
struct Phone {
    endpoint: String,
    state: Arc<Mutex<PhoneState>>,
    quic: quinn::Endpoint,
    _runtime: tokio::runtime::Runtime,
}
impl Phone {
    fn start() -> Self {
        let runtime = runtime().unwrap();
        let guard = runtime.enter();
        let cert = rcgen::generate_simple_self_signed(vec!["ember.local".into()]).unwrap();
        let der = cert.cert.der().clone();
        let pin = format!("{:x}", Sha256::digest(der.as_ref()));
        let mut tls = quinn::rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![der.clone()],
                quinn::rustls::pki_types::PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der())
                    .into(),
            )
            .unwrap();
        tls.alpn_protocols = vec![b"ember/2".to_vec()];
        let config = quinn::ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(tls).unwrap(),
        ));
        let quic = quinn::Endpoint::server(config, "127.0.0.1:0".parse().unwrap()).unwrap();
        let tcp = tokio::net::TcpListener::from_std({
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            listener
        })
        .unwrap();
        let endpoint = tcp.local_addr().unwrap().to_string();
        let advertisement = json!({"port":quic.local_addr().unwrap().port(),"certificate_der":STANDARD.encode(&der),
            "fingerprint":pin,"device":{"id":"a".repeat(64),"name":"test phone","pairing":1},"traffic_control":2});
        runtime.spawn(async move {
            loop {
                let Ok((socket, _)) = tcp.accept().await else {
                    break;
                };
                let advertisement = advertisement.clone();
                tokio::spawn(async move {
                    let mut reader = tokio::io::BufReader::new(socket);
                    let mut line = String::new();
                    reader.read_line(&mut line).await.unwrap();
                    let request: Value = serde_json::from_str(&line).unwrap();
                    assert_eq!(request["method"], "v2_info");
                    assert_eq!(request["token"], "", "Never leak secrets to TCP discovery");
                    let reply = json!({"id":1,"ok":true,"result":{"v2":advertisement}});
                    reader
                        .get_mut()
                        .write_all(format!("{reply}\n").as_bytes())
                        .await
                        .unwrap();
                });
            }
        });
        let state = Arc::new(Mutex::new(PhoneState::default()));
        let server = quic.clone();
        let shared = state.clone();
        runtime.spawn(async move {
            while let Some(incoming) = server.accept().await {
                let state = shared.clone();
                let pin = pin.clone();
                tokio::spawn(async move {
                    let connection = incoming.await.unwrap();
                    let mut authenticated = false;
                    while let Ok((mut send, mut recv)) = connection.accept_bi().await {
                        let (kind, bytes) = transport::read(&mut recv).await.unwrap();
                        if kind == transport::MEMORY_READ && authenticated {
                            let request: Value = serde_json::from_slice(&bytes).unwrap();
                            let (delay, wrong_kind) = {
                                let state = state.lock().unwrap();
                                (state.memory_delay_ms, state.wrong_memory_kind)
                            };
                            if delay > 0 { tokio::time::sleep(Duration::from_millis(delay)).await; }
                            let (kind, bytes) = match memory_read(&state, request) {
                                Ok(bytes) => (if wrong_kind { transport::OK } else { transport::DATA }, bytes),
                                Err(e) => (255, serde_json::to_vec(&json!({"code":e.0,"message":e.0})).unwrap()),
                            };
                            if transport::write(&mut send, kind, &bytes).await.is_ok() {
                                let _ = send.finish();
                                let _ = send.stopped().await;
                            }
                            continue;
                        }
                        let result: Result<Value> = match kind {
                            transport::PAIR => {
                                let request: Value = serde_json::from_slice(&bytes).unwrap();
                                assert_eq!(request.as_object().unwrap().len(), 2);
                                let mut code = [0; 4];
                                connection.export_keying_material(&mut code, b"ember-pairing-v1", b"").unwrap();
                                let mut state = state.lock().unwrap();
                                state.comparison = format!("{:08}", u32::from_be_bytes(code) % 100_000_000);
                                if state.reject_pairing {
                                    Err(Error("pairing_rejected".into()))
                                } else {
                                    state.token = Some(format!("pair:{}:{}.{}", "b".repeat(64), request["secret"].as_str().unwrap(), pin));
                                    Ok(json!({"pair_id":"b".repeat(64),"server_id":"a".repeat(64),"name":"test phone"}))
                                }
                            }
                            transport::AUTH => {
                                let request: Value = serde_json::from_slice(&bytes).unwrap();
                                assert_eq!(request["control"], true);
                                authenticated = state.lock().unwrap().token.as_deref() == request["token"].as_str();
                                if authenticated { Ok(json!({})) } else { Err(Error("unauthorized".into())) }
                            }
                            transport::RPC if authenticated => {
                                let request: Value = serde_json::from_slice(&bytes).unwrap();
                                rpc(&state, request["method"].as_str().unwrap(), request["params"].clone())
                            }
                            transport::UPLOAD if authenticated => {
                                let request: Value = serde_json::from_slice(&bytes).unwrap();
                                assert_eq!(request, json!({"upload_id":5,"offset":0}));
                                let mut uploaded = Vec::new();
                                loop {
                                    let (kind, bytes) = transport::read(&mut recv).await.unwrap();
                                    if kind == transport::END { assert!(bytes.is_empty()); break; }
                                    assert_eq!(kind, transport::DATA);
                                    assert!(!bytes.is_empty() && bytes.len() <= 65536);
                                    uploaded.extend_from_slice(&bytes);
                                }
                                let mut state = state.lock().unwrap();
                                state.upload = uploaded;
                                if state.reject_upload { Err(Error("upload_failed".into())) }
                                else { Ok(json!({"received":state.upload.len()})) }
                            }
                            transport::FORGET if authenticated => {
                                state.lock().unwrap().token = None;
                                Ok(json!({}))
                            }
                            _ => Err(Error("unauthorized".into())),
                        };
                        let (kind, value) = match result {
                            Ok(value) => (transport::OK, value),
                            Err(e) => (255, json!({"code":e.0,"message":e.0})),
                        };
                        transport::write(&mut send, kind, &serde_json::to_vec(&value).unwrap()).await.unwrap();
                        send.finish().unwrap();
                        let _ = send.stopped().await;
                    }
                });
            }
        });
        drop(guard);
        Self {
            endpoint,
            state,
            quic,
            _runtime: runtime,
        }
    }
}
impl Drop for Phone {
    fn drop(&mut self) {
        self.quic.close(0u32.into(), b"test finished");
    }
}

fn rpc(shared: &Mutex<PhoneState>, method: &str, params: Value) -> Result<Value> {
    let mut state = shared.lock().unwrap();
    // Mirror ember-core's deny_unknown_fields contracts.
    let keys: &[&str] = match method {
        "backend_state" => &[],
        "attach" => &["session_id", "chip", "speed_khz", "protocol", "under_reset"],
        "detach" | "gdb_stop" => &["session_id"],
        "gdb_start" => &["session_id", "core", "port"],
        "firmware_begin" => &["name", "size", "sha256"],
        "firmware_finish" | "firmware_abort" => &["upload_id"],
        "flash" => &["session_id", "firmware_id", "format", "run_after"],
        _ => return Err(Error("unknown_method".into())),
    };
    if params
        .as_object()
        .unwrap()
        .keys()
        .any(|key| !keys.contains(&key.as_str()))
    {
        return Err(Error("invalid_params".into()));
    }
    if keys.contains(&"session_id") && params["session_id"] != 7 {
        return Err(Error("stale_session".into()));
    }
    state.calls.push((method.into(), params.clone()));
    match method {
        "backend_state" => Ok(
            json!({"connection":{"session_id":7,"attached":state.attached,"chip":if state.attached {json!("test-chip")} else {Value::Null}},
            "gdb":{"running":state.gdb},"flash":if state.job {json!({"id":if state.replace_job {10} else {9},
                "state":if state.fail_flash {"failed"} else {"succeeded"},"error":"probe disconnected"})} else {Value::Null}}),
        ),
        "attach" => {
            assert_eq!(params["chip"], "test-chip");
            state.attached = true;
            Ok(json!({}))
        }
        "detach" => {
            assert!(!state.gdb);
            state.attached = false;
            Ok(json!({}))
        }
        "gdb_start" if state.fail_gdb => Err(Error("gdb_error".into())),
        "gdb_start" => {
            state.gdb = true;
            Ok(json!({}))
        }
        "gdb_stop" => {
            state.gdb = false;
            Ok(json!({}))
        }
        "firmware_begin" => {
            state.upload_size = params["size"].as_u64().unwrap();
            state.sha256 = params["sha256"].as_str().unwrap().into();
            Ok(json!({"id":5}))
        }
        "firmware_finish" => {
            assert_eq!(state.upload_size, state.upload.len() as u64);
            assert_eq!(state.sha256, format!("{:x}", Sha256::digest(&state.upload)));
            Ok(json!({"ready":true}))
        }
        "firmware_abort" => Ok(json!({})),
        "flash" => {
            assert_eq!(params["firmware_id"], 5);
            assert_eq!(params["run_after"], true);
            state.job = true;
            Ok(json!({"job_id":9}))
        }
        _ => unreachable!(),
    }
}

fn memory_read(shared: &Mutex<PhoneState>, params: Value) -> Result<Vec<u8>> {
    let mut state = shared.lock().unwrap();
    let mut keys: Vec<_> = params
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["address", "core", "length", "session_id"]);
    state.calls.push(("memory_read".into(), params.clone()));
    if params["session_id"].as_i64() != Some(state.current_session.unwrap_or(7)) {
        return Err(Error("stale_session".into()));
    }
    if state.gdb {
        return Err(Error("gdb_busy".into()));
    }
    if !state.attached {
        return Err(Error("not_attached".into()));
    }
    let address = params["address"].as_u64().unwrap();
    let length = params["length"].as_u64().unwrap();
    let length = if state.short_memory {
        length - 1
    } else {
        length
    };
    Ok((0..length).map(|i| address.wrapping_add(i) as u8).collect())
}

fn memory_blocks() -> Vec<nya_domain::waveform::ReadBlock> {
    vec![
        nya_domain::waveform::ReadBlock {
            address: 0x20000004,
            byte_size: 4,
            little_endian: true,
        },
        nya_domain::waveform::ReadBlock {
            address: 0x20000030,
            byte_size: 2,
            little_endian: false,
        },
    ]
}

#[test]
fn native_acquisition_reads_binary_memory_without_gdb_or_execution_commands() {
    let phone = Phone::start();
    let (_root, mut adapter, profile) = fixture();
    let mut config = pair(&mut adapter, &phone);
    config.core = 2;
    let generation = adapter
        .start_wireless_acquisition(&profile, &config)
        .unwrap();
    let values = adapter
        .read_wireless_memory(generation, &memory_blocks())
        .unwrap();
    assert_eq!(values, [vec![4, 5, 6, 7], vec![0x30, 0x31]]);
    assert_eq!(
        phone
            .state
            .lock()
            .unwrap()
            .calls
            .iter()
            .find(|(method, _)| method == "memory_read")
            .unwrap()
            .1["core"],
        2
    );
    adapter.stop_wireless_acquisition(generation).unwrap();
    let state = phone.state.lock().unwrap();
    let calls: Vec<_> = state.calls.iter().map(|(m, _)| m.as_str()).collect();
    assert_eq!(
        calls,
        [
            "backend_state",
            "attach",
            "memory_read",
            "memory_read",
            "detach"
        ]
    );
    assert!(!state.attached && !state.gdb);
    assert!(adapter.wireless.is_none());
}

#[test]
fn native_acquisition_rejects_stale_reads_stops_and_invalid_ranges() {
    let phone = Phone::start();
    let (_root, mut adapter, profile) = fixture();
    let config = pair(&mut adapter, &phone);
    let old = adapter
        .start_wireless_acquisition(&profile, &config)
        .unwrap();
    adapter.stop_wireless_acquisition(old).unwrap();
    let new = adapter
        .start_wireless_acquisition(&profile, &config)
        .unwrap();
    let before = phone.state.lock().unwrap().calls.len();
    assert_ne!(old, new);
    assert!(adapter.read_wireless_memory(old, &memory_blocks()).is_err());
    adapter.stop_wireless_acquisition(old).unwrap();
    for (address, length, count) in [(0, 0, 1), (0, 1025, 1), (u64::MAX, 4, 1), (0, 4, 9)] {
        let blocks = vec![
            nya_domain::waveform::ReadBlock {
                address,
                byte_size: length,
                little_endian: true
            };
            count
        ];
        assert!(adapter.read_wireless_memory(new, &blocks).is_err());
    }
    assert_eq!(phone.state.lock().unwrap().calls.len(), before);
    assert!(adapter.read_wireless_memory(new, &memory_blocks()).is_ok());
    assert!(adapter
        .execute_debug(DebugAction::FlashWireless {
            profile,
            connection: config
        })
        .is_err());
    adapter.stop_wireless_acquisition(new).unwrap();
}

#[test]
fn native_acquisition_rejects_short_wrong_kind_stale_session_and_gdb_ownership() {
    for failure in ["short", "kind", "session", "gdb"] {
        let phone = Phone::start();
        let (_root, mut adapter, profile) = fixture();
        let config = pair(&mut adapter, &phone);
        phone.state.lock().unwrap().attached = true;
        let generation = adapter
            .start_wireless_acquisition(&profile, &config)
            .unwrap();
        {
            let mut state = phone.state.lock().unwrap();
            match failure {
                "short" => state.short_memory = true,
                "kind" => state.wrong_memory_kind = true,
                "session" => state.current_session = Some(8),
                "gdb" => state.gdb = true,
                _ => unreachable!(),
            }
        }
        assert!(adapter
            .read_wireless_memory(generation, &memory_blocks())
            .is_err());
        adapter.stop_wireless_acquisition(generation).unwrap();
        assert!(phone.state.lock().unwrap().attached);
        assert!(!phone
            .state
            .lock()
            .unwrap()
            .calls
            .iter()
            .any(|(m, _)| m == "detach" || m == "gdb_stop"));
    }
}

#[test]
fn native_memory_timeouts_are_bounded_and_do_not_report_partial_batches() {
    let phone = Phone::start();
    let (_root, mut adapter, profile) = fixture();
    let config = pair(&mut adapter, &phone);
    let generation = adapter
        .start_wireless_acquisition(&profile, &config)
        .unwrap();
    phone.state.lock().unwrap().memory_delay_ms = 1500;
    let started = Instant::now();
    let error = adapter
        .read_wireless_memory(generation, &memory_blocks())
        .unwrap_err();
    assert!(error.0.contains("超时"));
    assert!(started.elapsed() < Duration::from_millis(1300));
    adapter.stop_wireless_acquisition(generation).unwrap();
}

fn fixture() -> (tempfile::TempDir, DebugAdapter, TargetProfile) {
    let root = tempfile::tempdir().unwrap();
    let firmware = root.path().join("firmware with spaces.axf");
    std::fs::write(&firmware, b"fixture").unwrap();
    let adapter = DebugAdapter {
        manager: nya_scraping_core::manager::Manager {
            config_path: root.path().join("config/config"),
            data_dir: root.path().join("data"),
        },
        install_cancel: Default::default(),
        install_progress: Default::default(),
        server: None,
        session: Default::default(),
        wireless: None,
    };
    let profile = TargetProfile {
        name: "wireless".into(),
        tool: nya_domain::ToolKind::ProbeRs,
        target: "test-chip".into(),
        interface: None,
        transport: None,
        gdb_port: 3333,
        gdb_executable: "gdb".into(),
        firmware,
    };
    (root, adapter, profile)
}

fn pair(adapter: &mut DebugAdapter, phone: &Phone) -> WirelessConfig {
    let mut comparison = String::new();
    let device = adapter
        .pair_wireless(&phone.endpoint, "test computer", &mut |code| {
            comparison = code.into()
        })
        .unwrap();
    assert_eq!(comparison.len(), 8);
    assert_eq!(comparison, phone.state.lock().unwrap().comparison);
    assert_eq!(
        adapter.paired_wireless_devices().unwrap(),
        vec![device.clone()]
    );
    WirelessConfig::new(device.id)
}

#[test]
fn pairing_persists_and_revokes_without_exposing_credentials() {
    let phone = Phone::start();
    let (root, mut adapter, _) = fixture();
    let config = pair(&mut adapter, &phone);
    let public = format!("{:?}", adapter.paired_wireless_devices().unwrap());
    let token = phone.state.lock().unwrap().token.clone().unwrap();
    assert!(!public.contains(&token));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = adapter.wireless_directory();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(path.join("paired-devices.dat"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    drop(adapter);
    let mut reopened = fixture().1;
    reopened.manager.config_path = root.path().join("config/config");
    assert_eq!(reopened.paired_wireless_devices().unwrap().len(), 1);
    assert!(reopened.forget_wireless(&config.device_id).unwrap());
    assert!(reopened.paired_wireless_devices().unwrap().is_empty());
    assert!(phone.state.lock().unwrap().token.is_none());
}

#[test]
fn pairing_rejection_saves_nothing_and_never_accesses_hardware() {
    let phone = Phone::start();
    phone.state.lock().unwrap().reject_pairing = true;
    let (_root, mut adapter, _) = fixture();
    let error = adapter
        .pair_wireless(&phone.endpoint, "computer", &mut |_| {})
        .unwrap_err();
    assert!(error.0.contains("pairing_rejected"));
    assert!(adapter.paired_wireless_devices().unwrap().is_empty());
    assert!(phone.state.lock().unwrap().calls.is_empty());
}

#[test]
fn certificate_change_is_rejected_before_auth_or_hardware_access() {
    let phone = Phone::start();
    let replacement = Phone::start();
    let (_root, mut adapter, profile) = fixture();
    let mut config = pair(&mut adapter, &phone);
    config.endpoint = Some(replacement.endpoint.clone());
    let error = adapter
        .execute_debug(DebugAction::FlashWireless {
            profile,
            connection: config,
        })
        .unwrap_err();
    assert!(error.0.contains("证书指纹"));
    assert!(replacement.state.lock().unwrap().calls.is_empty());
}

#[test]
fn upload_checks_sha_and_flash_job_before_reporting_success() {
    let phone = Phone::start();
    let (_root, mut adapter, profile) = fixture();
    let config = pair(&mut adapter, &phone);
    let bytes: Vec<_> = (0..180000).map(|i| (i % 251) as u8).collect();
    std::fs::write(&profile.firmware, &bytes).unwrap();
    adapter
        .execute_debug(DebugAction::FlashWireless {
            profile,
            connection: config,
        })
        .unwrap();
    let state = phone.state.lock().unwrap();
    assert_eq!(state.upload, bytes);
    let calls: Vec<_> = state.calls.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        calls,
        [
            "backend_state",
            "attach",
            "firmware_begin",
            "firmware_finish",
            "flash",
            "backend_state",
            "detach"
        ]
    );
    assert!(!state.attached);
    assert_eq!(
        adapter.install_progress.lock().unwrap().percent,
        Some(100.0)
    );
}

#[test]
fn failed_or_replaced_flash_does_not_report_success_or_detach() {
    for replaced in [false, true] {
        let phone = Phone::start();
        let (_root, mut adapter, profile) = fixture();
        let config = pair(&mut adapter, &phone);
        {
            let mut state = phone.state.lock().unwrap();
            state.fail_flash = !replaced;
            state.replace_job = replaced;
        }
        assert!(adapter
            .execute_debug(DebugAction::FlashWireless {
                profile,
                connection: config
            })
            .is_err());
        assert_ne!(
            adapter.install_progress.lock().unwrap().percent,
            Some(100.0)
        );
        assert!(!phone
            .state
            .lock()
            .unwrap()
            .calls
            .iter()
            .any(|(name, _)| name == "detach"));
    }
}

#[test]
fn upload_failure_aborts_without_flashing() {
    let phone = Phone::start();
    let (_root, mut adapter, profile) = fixture();
    let config = pair(&mut adapter, &phone);
    phone.state.lock().unwrap().reject_upload = true;
    assert!(adapter
        .execute_debug(DebugAction::FlashWireless {
            profile,
            connection: config
        })
        .unwrap_err()
        .0
        .contains("upload_failed"));
    let state = phone.state.lock().unwrap();
    assert!(state.calls.iter().any(|(name, _)| name == "firmware_abort"));
    assert!(!state.calls.iter().any(|(name, _)| name == "flash"));
    assert!(!state.attached);
}

#[test]
fn existing_pairing_is_reused_and_can_be_forgotten_offline() {
    let phone = Phone::start();
    let (_root, mut adapter, _) = fixture();
    let config = pair(&mut adapter, &phone);
    let original = phone.state.lock().unwrap().token.clone();
    let saved = adapter
        .pair_wireless(&phone.endpoint, "computer", &mut |_| {
            panic!("Already paired")
        })
        .unwrap();
    assert_eq!(saved.id, config.device_id);
    assert_eq!(phone.state.lock().unwrap().token, original);
    drop(phone);
    assert!(!adapter.forget_wireless(&config.device_id).unwrap());
    assert!(adapter.paired_wireless_devices().unwrap().is_empty());
}

#[test]
fn preexisting_target_is_borrowed_and_busy_gdb_is_left_alone() {
    let phone = Phone::start();
    let (_root, mut adapter, profile) = fixture();
    let config = pair(&mut adapter, &phone);
    phone.state.lock().unwrap().attached = true;
    adapter
        .execute_debug(DebugAction::FlashWireless {
            profile: profile.clone(),
            connection: config.clone(),
        })
        .unwrap();
    assert!(phone.state.lock().unwrap().attached);
    assert!(!phone
        .state
        .lock()
        .unwrap()
        .calls
        .iter()
        .any(|(m, _)| m == "attach" || m == "detach"));
    phone.state.lock().unwrap().gdb = true;
    let error = adapter
        .execute_debug(DebugAction::FlashWireless {
            profile,
            connection: config,
        })
        .unwrap_err();
    assert!(error.0.contains("占用"));
    assert!(phone.state.lock().unwrap().gdb);
}

#[cfg(unix)]
fn fake_gdb(root: &std::path::Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let gdb = root.join("fake-gdb");
    std::fs::write(&gdb, include_bytes!("../../tests/fixtures/fake_gdb.py")).unwrap();
    std::fs::set_permissions(&gdb, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(gdb.with_extension("options"), r#"{"wireless":true}"#).unwrap();
    gdb
}

#[cfg(unix)]
#[test]
fn gdb_failure_and_adapter_drop_release_only_owned_resources() {
    for fail_start in [true, false] {
        let phone = Phone::start();
        let (root, mut adapter, mut profile) = fixture();
        let config = pair(&mut adapter, &phone);
        profile.gdb_executable = fake_gdb(root.path());
        phone.state.lock().unwrap().fail_gdb = fail_start;
        let result = adapter.execute_debug(DebugAction::ConnectWireless {
            profile,
            connection: config,
            reset: false,
        });
        assert_eq!(result.is_err(), fail_start);
        drop(adapter);
        let state = phone.state.lock().unwrap();
        assert!(!state.attached && !state.gdb);
        assert!(state.calls.iter().any(|(m, _)| m == "detach"));
        assert_eq!(
            state.calls.iter().any(|(m, _)| m == "gdb_stop"),
            !fail_start
        );
    }
}

#[cfg(unix)]
#[test]
fn wireless_gdb_reuses_debug_actions_and_cleans_owned_services() {
    let phone = Phone::start();
    let (root, mut adapter, mut profile) = fixture();
    let config = pair(&mut adapter, &phone);
    let gdb = fake_gdb(root.path());
    profile.gdb_executable = gdb.clone();
    adapter
        .execute_debug(DebugAction::ConnectWireless {
            profile,
            connection: config,
            reset: true,
        })
        .unwrap();
    adapter
        .execute_debug(DebugAction::AddBreakpoint("compute".into()))
        .unwrap();
    adapter.execute_debug(DebugAction::Continue).unwrap();
    let bytes = adapter.read_live_memory(&memory_blocks()).unwrap();
    assert_eq!(bytes.len(), 2);
    assert_eq!(
        adapter.debug_snapshot(&[]).state,
        nya_domain::SessionState::Running
    );
    adapter.execute_debug(DebugAction::Pause).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while adapter.debug_snapshot(&[]).state != nya_domain::SessionState::Halted {
        assert!(Instant::now() < deadline);
        adapter.poll_debug().unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    adapter.execute_debug(DebugAction::StepOver).unwrap();
    adapter.execute_debug(DebugAction::Download).unwrap();
    adapter.execute_debug(DebugAction::Disconnect).unwrap();
    let commands = std::fs::read_to_string(gdb.with_extension("commands")).unwrap();
    assert!(commands.contains("-target-select remote 127.0.0.1:"));
    assert!(commands.contains("-data-read-memory-bytes"));
    assert_eq!(commands.matches("-exec-interrupt --all").count(), 1);
    assert!(commands.contains("-target-download"));
    assert!(commands.contains("target-non-stop"));
    let state = phone.state.lock().unwrap();
    assert!(!state.gdb && !state.attached);
    let calls: Vec<_> = state.calls.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        calls,
        ["backend_state", "attach", "gdb_start", "gdb_stop", "detach"]
    );
    assert!(adapter.wireless.is_none());
}

#[tokio::test]
async fn malformed_and_oversized_frames_are_rejected() {
    let mut oversized = b"EMB2\x02\0\0\0".to_vec();
    oversized.extend_from_slice(&70000u32.to_le_bytes());
    assert!(transport::read(&mut oversized.as_slice()).await.is_err());
    assert!(transport::read(&mut b"BAD!\x02\0\0\0\0\0\0\0".as_slice())
        .await
        .is_err());
}
