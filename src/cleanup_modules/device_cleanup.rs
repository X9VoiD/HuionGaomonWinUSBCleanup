use async_trait::async_trait;
use error_stack::{IntoReport, Result, ResultExt};
use regex::Regex;
use serde::Deserialize;
use uuid::Uuid;
use windows::core::HSTRING;
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Foundation::BOOL;

use super::*;

use crate::cleanup_modules::create_dump_file;
use crate::services;
use crate::services::identifiers;
use crate::services::regex_cache;
use crate::services::windows::{enumerate_devices, Device};
use crate::State;

const DEVICE_MODULE_NAME: &str = "Device Cleanup";
const DEVICE_MODULE_CLI: &str = "device-cleanup";
const DEVICE_IDENTIFIER: &str = "device_identifiers.json";

#[derive(Default)]
pub struct DeviceCleanupModule {
    objects_to_uninstall: Vec<DeviceToUninstall>,
    device_dumper: DeviceDumper,
}

impl DeviceCleanupModule {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ModuleMetadata for DeviceCleanupModule {
    fn name(&self) -> &str {
        DEVICE_MODULE_NAME
    }

    fn cli_name(&self) -> &str {
        DEVICE_MODULE_CLI
    }

    fn help(&self) -> &str {
        "remove devices from the system"
    }

    fn noun(&self) -> &str {
        "devices"
    }
}

#[async_trait]
impl ModuleStrategy for DeviceCleanupModule {
    type Object = Device;
    type ToUninstall = DeviceToUninstall;

    async fn initialize(&mut self, state: &State) -> Result<(), ModuleError> {
        let resource = identifiers::get_resource(DEVICE_IDENTIFIER, state)
            .await
            .into_module_report(DEVICE_MODULE_NAME)?;
        let devices_raw = resource.get_content();
        let devices: Vec<DeviceToUninstall> = serde_json::from_slice(devices_raw)
            .into_report()
            .into_module_report(DEVICE_MODULE_NAME)?;
        self.objects_to_uninstall = devices;
        Ok(())
    }

    fn get_objects(&self) -> Result<Vec<Self::Object>, ModuleError> {
        services::windows::enumerate_devices().into_module_report(DEVICE_MODULE_NAME)
    }

    fn get_objects_to_uninstall(&self) -> &[Self::ToUninstall] {
        self.objects_to_uninstall.as_slice()
    }

    async fn uninstall_object(
        &self,
        object: Self::Object,
        to_uninstall: &Self::ToUninstall,
        _state: &State,
        run_info: &mut ModuleRunInfo,
    ) -> Result<(), UninstallError> {
        unsafe {
            let device_info_set = SetupDiCreateDeviceInfoList(None, None)
                .into_report()
                .attach_printable_lazy(|| "failed to create a device list")
                .into_uninstall_report(to_uninstall)?;
            let mut device_info_data = SP_DEVINFO_DATA {
                cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
                ..SP_DEVINFO_DATA::default()
            };

            if !SetupDiOpenDeviceInfoW(
                device_info_set,
                &HSTRING::from(object.instance_id()),
                None,
                0,
                Some(&mut device_info_data),
            )
            .as_bool()
            {
                let error = windows::core::Error::from_win32();
                return Err(error)
                    .into_report()
                    .attach_printable_lazy(|| {
                        format!("failed to open device info of {}", object.instance_id())
                    })
                    .into_uninstall_report(to_uninstall);
            }

            let mut reboot: BOOL = false.into();
            if !DiUninstallDevice(
                None,
                device_info_set,
                &device_info_data,
                0,
                Some(&mut reboot),
            )
            .as_bool()
            {
                let error = windows::core::Error::from_win32();
                return Err(error)
                    .into_report()
                    .attach_printable_lazy(|| {
                        format!("failed to uninstall device {}", object.instance_id())
                    })
                    .into_uninstall_report(to_uninstall);
            }

            if reboot.as_bool() {
                run_info.reboot_required = true;
            }

            if to_uninstall.uninstall_inf.unwrap_or(false) {
                let inf_path = Path::new(object.driver_store_location().unwrap())
                    .join(object.inf_original_name().unwrap());
                let driver_uninstall = super::driver_cleanup::DriverToUninstall {
                    friendly_name: object.friendly_name().unwrap().to_string(),
                    original_name: None,
                    provider: None,
                    class: None
                };

                super::driver_cleanup::uninstall_driver(&inf_path, &driver_uninstall, run_info)
                    .attach_printable_lazy(|| {
                        format!("failed to uninstall driver for device {}", object.instance_id())
                    })?
            }

            Ok(())
        }
    }

    fn get_dumper(&self) -> Option<&dyn Dumper> {
        Some(&self.device_dumper)
    }
}

#[derive(Default)]
struct DeviceDumper {}

#[async_trait]
impl Dumper for DeviceDumper {
    async fn dump(&self, state: &State) -> Result<(), ModuleError> {
        let inf_regex = Regex::new(r"^oem[0-9]+\.inf$").unwrap();
        let devices: Vec<Device> = enumerate_devices()
            .into_module_report(DEVICE_MODULE_NAME)?
            .into_iter()
            .filter(|d| inf_regex.is_match(d.inf_name().unwrap_or("")))
            .filter(is_of_interest)
            .collect();

        let file_path =
            get_path_to_dump(state, "devices.json").into_module_report(DEVICE_MODULE_NAME)?;
        let dump_file = create_dump_file(&file_path).into_module_report(DEVICE_MODULE_NAME)?;
        let file_name = file_path.as_path().to_str().unwrap();

        if devices.is_empty() {
            println!("No devices to dump");
            return Ok(());
        }

        serde_json::to_writer_pretty(dump_file, &devices)
            .into_report()
            .attach_printable_lazy(|| format!("failed to dump devices into '{}'", file_name))
            .into_module_report(DEVICE_MODULE_NAME)?;

        match devices.len() {
            1 => println!("Dumped 1 device to {}", file_name),
            n => println!("Dumped {} devices to {}", n, file_name),
        }

        Ok(())
    }
}

#[derive(Deserialize, Debug)]
pub struct DeviceToUninstall {
    friendly_name: String,
    device_desc: Option<String>,
    manufacturer: Option<String>,
    hardware_id: Option<String>,
    class_uuid: Option<Uuid>,
    uninstall_inf: Option<bool>
}

impl ToUninstall<Device> for DeviceToUninstall {
    fn matches(&self, other: &Device) -> bool {
        regex_cache::cached_match(other.description(), self.device_desc.as_deref())
            && regex_cache::cached_match(other.manufacturer(), self.manufacturer.as_deref())
            && match self.class_uuid {
                Some(uuid) => *other.class_guid() == uuid,
                None => true,
            }
            && other
                .hardware_ids()
                .iter()
                .any(|hwid| regex_cache::cached_match(Some(hwid), self.hardware_id.as_deref()))
    }
}

impl std::fmt::Display for DeviceToUninstall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.friendly_name)
    }
}

fn is_of_interest(device: &Device) -> bool {
    use crate::services::interest::is_of_interest_iter as candidate_iter;
    let strings = [
        device.description(),
        device.manufacturer(),
        device.inf_original_name(),
    ]
    .into_iter()
    .flatten()
    .chain(device.hardware_ids().iter().map(|s| s.as_str()));

    candidate_iter(strings)
}
