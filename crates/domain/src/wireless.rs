use crate::Result;

/// Public pairing metadata. Long-lived credentials stay inside the backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WirelessDevice {
    pub id: String,
    pub name: String,
    pub endpoint: String,
}

#[derive(Clone, Debug)]
pub struct WirelessConfig {
    pub device_id: String,
    /// TCP discovery endpoint (normally phone-ip:4242); None uses the saved address.
    pub endpoint: Option<String>,
    pub core: u32,
    pub speed_khz: u32,
    pub protocol: String,
    pub under_reset: bool,
}

impl WirelessConfig {
    pub fn new(device_id: impl Into<String>) -> Self {
        Self {
            device_id: device_id.into(),
            endpoint: None,
            core: 0,
            speed_khz: 1000,
            protocol: "swd".into(),
            under_reset: false,
        }
    }
}

/// Blocking methods: run on the backend worker. Forward the comparison code to
/// the user immediately; approval belongs to the phone and expires in 120 seconds.
pub trait WirelessRegistry: Send {
    fn paired_wireless_devices(&self) -> Result<Vec<WirelessDevice>>;
    fn pair_wireless(
        &mut self,
        endpoint: &str,
        computer_name: &str,
        on_code: &mut dyn FnMut(&str),
    ) -> Result<WirelessDevice>;
    /// Forget locally even when offline. Returns whether remote revocation succeeded.
    fn forget_wireless(&mut self, device_id: &str) -> Result<bool>;
}
