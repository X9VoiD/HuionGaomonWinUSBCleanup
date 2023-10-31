use std::path::Path;

use async_trait::async_trait;
use error_stack::{IntoReport, Result, ResultExt};
use serde::Deserialize;
use uuid::Uuid;
use windows::core::HSTRING;
use windows::Win32::Devices::DeviceAndDriverInstallation::DiUninstallDriverW;
use windows::Win32::Foundation::BOOL;

use super::*;

use crate::cleanup_modules::{create_dump_file, get_path_to_dump};
use crate::services;
use crate::services::identifiers;
use crate::services::regex_cache;
use crate::services::windows::{enumerate_drivers, Driver};
use crate::State;

const DRIVER_MODULE_NAME: &str = "Driver Cleanup";
const DRIVER_MODULE_CLI: &str = "driver-cleanup";
const DRIVER_IDENTIFIER: &str = "driver_identifiers.json";

#[derive(Default)]
pub struct DriverCleanupModule {
    objects_to_uninstall: Vec<DriverToUninstall>,
    driver_dumper: DriverDumper,
}

impl DriverCleanupModule {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ModuleMetadata for DriverCleanupModule {
    fn name(&self) -> &str {
        DRIVER_MODULE_NAME
    }

    fn cli_name(&self) -> &str {
        DRIVER_MODULE_CLI
    }

    fn help(&self) -> &str {
        "uninstall device drivers from the system"
    }

    fn noun(&self) -> &str {
        "drivers"
    }
}

#[async_trait]
impl ModuleStrategy for DriverCleanupModule {
    type Object = Driver;
    type ToUninstall = DriverToUninstall;

    async fn initialize(&mut self, state: &State) -> Result<(), ModuleError> {
        let resource = identifiers::get_resource(DRIVER_IDENTIFIER, state)
            .await
            .into_module_report(DRIVER_MODULE_NAME)?;
        let drivers_raw = resource.get_content();
        let drivers: Vec<DriverToUninstall> = serde_json::from_slice(drivers_raw)
            .into_report()
            .into_module_report(DRIVER_MODULE_NAME)?;
        self.objects_to_uninstall = drivers;
        Ok(())
    }

    fn get_objects(&self) -> Result<Vec<Self::Object>, ModuleError> {
        services::windows::enumerate_drivers().into_module_report(DRIVER_MODULE_NAME)
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
        let inf_path = Path::new(object.driver_store_location().unwrap())
            .join(object.inf_original_name().unwrap());
        uninstall_driver(&inf_path, to_uninstall, run_info)
    }

    fn get_dumper(&self) -> Option<&dyn Dumper> {
        Some(&self.driver_dumper)
    }
}

pub(crate) fn uninstall_driver(
    inf_path: &Path,
    to_uninstall: &DriverToUninstall,
    run_info: &mut ModuleRunInfo,
) -> Result<(), UninstallError> {
    unsafe {
        let mut reboot: BOOL = false.into();
        if !DiUninstallDriverW(
            None,
            &HSTRING::from(inf_path),
            0,
            Some(&mut reboot),
        )
        .as_bool()
        {
            let err = windows::core::Error::from_win32();
            return Err(err)
                .into_report()
                .attach_printable_lazy(|| {
                    format!("failed to uninstall inf: {}", inf_path.display())
                })
                .into_uninstall_report(to_uninstall);
        }

        if reboot.as_bool() {
            run_info.reboot_required = true;
        }

        Ok(())
    }
}

#[derive(Default)]
struct DriverDumper {}

#[async_trait]
impl Dumper for DriverDumper {
    async fn dump(&self, state: &State) -> Result<(), ModuleError> {
        let drivers: Vec<Driver> = enumerate_drivers()
            .into_module_report(DRIVER_MODULE_NAME)?
            .into_iter()
            .filter(is_of_interest)
            .collect();

        let file_path =
            get_path_to_dump(state, "drivers.json").into_module_report(DRIVER_MODULE_NAME)?;
        let dump_file = create_dump_file(&file_path).into_module_report(DRIVER_MODULE_NAME)?;
        let file_name = file_path.as_path().to_str().unwrap();

        if drivers.is_empty() {
            println!("No drivers to dump");
            return Ok(());
        }

        serde_json::to_writer_pretty(dump_file, &drivers)
            .into_report()
            .attach_printable_lazy(|| format!("failed to dump drivers into '{}'", file_name))
            .into_module_report(DRIVER_MODULE_NAME)?;

        match drivers.len() {
            1 => println!("Dumped 1 driver into '{}'", file_name),
            n => println!("Dumped {} drivers into '{}'", n, file_name),
        }

        Ok(())
    }
}

#[derive(Deserialize, Debug)]
pub struct DriverToUninstall {
    pub friendly_name: String,
    pub original_name: Option<String>,
    pub provider: Option<String>,
    pub class: Option<Uuid>,
}

impl ToUninstall<Driver> for DriverToUninstall {
    fn matches(&self, other: &Driver) -> bool {
        regex_cache::cached_match(other.inf_original_name(), self.original_name.as_deref())
            && regex_cache::cached_match(other.provider(), self.provider.as_deref())
            && match self.class {
                Some(class) => *other.class_guid() == class,
                None => true,
            }
    }
}

impl std::fmt::Display for DriverToUninstall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.friendly_name)
    }
}

fn is_of_interest(driver: &Driver) -> bool {
    use crate::services::interest::is_of_interest_iter as candidate_iter;

    let strings = [driver.inf_original_name(), driver.provider()];
    candidate_iter(strings.into_iter().flatten())
}
