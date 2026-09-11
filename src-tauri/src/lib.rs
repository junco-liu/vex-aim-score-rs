use serde::{Deserialize, Serialize};
use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Manager};

const BRIDGE_SCRIPT: &str = r#"
(() => {
  if (window.vexAim) return;

  const UUIDS = {
    AIM_CODE: '08590f7e-db05-467e-8757-72f6faeb13e5',
    AIM_RX_DATA: '08590f7e-db05-467e-8757-72f6faeb13f5',
    AIM_TX_DATA: '08590f7e-db05-467e-8757-72f6faeb1306',
    RC_AI_STATUS: '6c7851a0-adf7-4de6-881d-1ce2cd0fecc9',
    RC_STATUS: '6c7851a0-adf7-4de6-881d-1ce2cd0fecf9'
  };

  const invoke = (cmd, payload = {}) => window.__TAURI_INTERNALS__.invoke(cmd, payload);
  const dataView = (bytes) => {
    const copy = Uint8Array.from(bytes);
    return new DataView(copy.buffer);
  };

  const cdcAck = () => dataView([0xaa, 0x55, 0x00, 0x04, 0x18, 0x76, 0xd9, 0x6f]);
  const heartbeat = (running) => dataView([0x00, 0x00, running ? 0x82 : 0x00]);

  class FakeCharacteristic extends EventTarget {
    constructor(device, uuid) {
      super();
      this.device = device;
      this.uuid = uuid;
      this.notifying = false;
      this.value = dataView([]);
    }

    async startNotifications() {
      this.notifying = true;
      if (this.uuid === UUIDS.RC_AI_STATUS || this.uuid === UUIDS.RC_STATUS) {
        this.emitValue(this.uuid === UUIDS.RC_AI_STATUS ? heartbeat(false) : dataView([0]));
      }
      return this;
    }

    async stopNotifications() {
      this.notifying = false;
      return this;
    }

    async readValue() {
      return this.value;
    }

    async writeValue(data) {
      return this.writeValueWithResponse(data);
    }

    async writeValueWithResponse(data) {
      await this.handleWrite(data);
    }

    async writeValueWithoutResponse(data) {
      await this.handleWrite(data);
    }

    async handleWrite(data) {
      const bytes = new Uint8Array(data?.buffer || data);
      if (this.uuid === UUIDS.AIM_CODE && bytes.length === 4 && !bytes.every((byte) => byte === 0xff)) {
        await invoke('save_unlock_code', {
          deviceId: this.device.id,
          macAddress: null,
          name: this.device.name,
          unlockCode: [...bytes].join('')
        });
      }
      if (this.uuid === UUIDS.AIM_RX_DATA) {
        this.device.characteristic(UUIDS.AIM_TX_DATA).emitValue(cdcAck());
      }
    }

    emitValue(value) {
      this.value = value;
      this.dispatchEvent(new Event('characteristicvaluechanged'));
    }
  }

  class FakeService {
    constructor(device, uuid) {
      this.device = device;
      this.uuid = uuid;
    }

    async getCharacteristic(uuid) {
      return this.device.characteristic(String(uuid).toLowerCase());
    }
  }

  class FakeServer {
    constructor(device) {
      this.device = device;
      this.connected = false;
    }

    async connect() {
      this.connected = true;
      await invoke('default_connect');
      return this;
    }

    disconnect() {
      this.connected = false;
      this.device.dispatchEvent(new Event('gattserverdisconnected'));
    }

    async getPrimaryService(uuid) {
      return new FakeService(this.device, String(uuid).toLowerCase());
    }
  }

  class FakeDevice extends EventTarget {
    constructor(record) {
      super();
      this.id = record?.device_id || record?.mac_address || 'tauri-vex-aim-device';
      this.name = record?.name || '1234A-1';
      this.gatt = new FakeServer(this);
      this._characteristics = new Map();
    }

    characteristic(uuid) {
      if (!this._characteristics.has(uuid)) {
        this._characteristics.set(uuid, new FakeCharacteristic(this, uuid));
      }
      return this._characteristics.get(uuid);
    }
  }

  async function defaultRecord(options = {}) {
    const info = await invoke('get_bluetooth_info');
    const prefix = options.filters?.find((filter) => filter.namePrefix)?.namePrefix;
    const record = info.default_device || {
      device_id: prefix ? `tauri-${prefix}` : 'tauri-vex-aim-device',
      name: prefix || '1234A-1'
    };
    await invoke('remember_device', {
      deviceId: record.device_id,
      macAddress: record.mac_address || null,
      name: record.name || null
    });
    return record;
  }

  const toDevice = async (record) => new FakeDevice(record || await defaultRecord());

  window.vexAim = {
    getBluetoothInfo: () => invoke('get_bluetooth_info'),
    defaultConnect: async () => {
      const info = await invoke('default_connect');
      return { info, device: await toDevice(info.default_device) };
    },
    saveUnlockCode: ({ deviceId, macAddress, name, unlockCode }) =>
      invoke('save_unlock_code', { deviceId, macAddress, name, unlockCode }),
    keyboardEvent: (event) => invoke('handle_keyboard_event', { event })
  };

  if (!navigator.bluetooth) {
    Object.defineProperty(navigator, 'bluetooth', {
      value: {},
      configurable: true
    });
  }
  navigator.bluetooth.getAvailability = () => Promise.resolve(true);
  navigator.bluetooth.getDevices = async () => {
    const info = await invoke('get_bluetooth_info');
    return info.default_device ? [await toDevice(info.default_device)] : [];
  };
  navigator.bluetooth.requestDevice = async (options = {}) => toDevice(await defaultRecord(options));

  window.addEventListener('keydown', (event) => {
    window.vexAim.keyboardEvent({
      type: 'keydown',
      key: event.key,
      code: event.code,
      altKey: event.altKey,
      ctrlKey: event.ctrlKey,
      metaKey: event.metaKey,
      shiftKey: event.shiftKey,
      repeat: event.repeat
    }).catch(() => {});
  }, true);
})();
"#;

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
struct DeviceRecord {
    device_id: String,
    mac_address: Option<String>,
    name: Option<String>,
    unlock_code: Option<String>,
    last_connected_at: Option<u64>,
}

#[derive(Debug, Serialize)]
struct BluetoothInfo {
    supported: bool,
    adapter_available: bool,
    default_device: Option<DeviceRecord>,
    message: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeyboardEventPayload {
    r#type: String,
    key: String,
    code: String,
    alt_key: bool,
    ctrl_key: bool,
    meta_key: bool,
    shift_key: bool,
    repeat: bool,
}

fn store_path(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    Ok(dir.join("bluetooth-device.json"))
}

fn load_device(app: &AppHandle) -> Result<Option<DeviceRecord>, String> {
    let path = store_path(app)?;
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(path).map_err(|error| error.to_string())?;
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn save_device(app: &AppHandle, record: &DeviceRecord) -> Result<(), String> {
    let path = store_path(app)?;
    let text = serde_json::to_string_pretty(record).map_err(|error| error.to_string())?;
    fs::write(path, text).map_err(|error| error.to_string())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn normalize_url(input: &str) -> Result<tauri::Url, String> {
    let trimmed = input.trim();
    let candidate = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let url = tauri::Url::parse(&candidate).map_err(|error| error.to_string())?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        _ => Err("Only http/https URLs are supported".into()),
    }
}

fn bluetooth_info(default_device: Option<DeviceRecord>) -> BluetoothInfo {
    BluetoothInfo {
        supported: true,
        adapter_available: true,
        default_device,
        message: "Tauri Web Bluetooth adapter is active. Device cache and 4-digit unlock code persistence are supported; native BLE GATT is not wired yet.".into(),
    }
}

#[tauri::command]
fn open_url(window: tauri::WebviewWindow, url: String) -> Result<(), String> {
    let url = normalize_url(&url)?;
    window.navigate(url).map_err(|error| error.to_string())?;
    window.set_focus().map_err(|error| error.to_string())
}

#[tauri::command]
fn get_bluetooth_info(app: AppHandle) -> Result<BluetoothInfo, String> {
    load_device(&app).map(bluetooth_info)
}

#[tauri::command]
fn default_connect(app: AppHandle) -> Result<BluetoothInfo, String> {
    let mut device = load_device(&app)?;
    if let Some(record) = &mut device {
        record.last_connected_at = Some(now_secs());
        save_device(&app, record)?;
    }
    Ok(bluetooth_info(device))
}

#[tauri::command]
fn remember_device(
    app: AppHandle,
    device_id: String,
    mac_address: Option<String>,
    name: Option<String>,
) -> Result<DeviceRecord, String> {
    let mut record = load_device(&app)?.unwrap_or_default();
    record.device_id = device_id;
    record.mac_address = mac_address;
    record.name = name;
    record.last_connected_at = Some(now_secs());
    save_device(&app, &record)?;
    Ok(record)
}

#[tauri::command]
fn save_unlock_code(
    app: AppHandle,
    device_id: String,
    mac_address: Option<String>,
    name: Option<String>,
    unlock_code: String,
) -> Result<DeviceRecord, String> {
    if unlock_code.len() != 4 || !unlock_code.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("Unlock code must be four digits".into());
    }
    let record = DeviceRecord {
        device_id,
        mac_address,
        name,
        unlock_code: Some(unlock_code),
        last_connected_at: Some(now_secs()),
    };
    save_device(&app, &record)?;
    Ok(record)
}

#[tauri::command]
fn handle_keyboard_event(event: KeyboardEventPayload) -> bool {
    let _ = (
        event.r#type,
        event.key,
        event.code,
        event.alt_key,
        event.ctrl_key,
        event.meta_key,
        event.shift_key,
        event.repeat,
    );
    true
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .append_invoke_initialization_script(BRIDGE_SCRIPT)
        .invoke_handler(tauri::generate_handler![
            open_url,
            get_bluetooth_info,
            default_connect,
            remember_device,
            save_unlock_code,
            handle_keyboard_event
        ])
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
