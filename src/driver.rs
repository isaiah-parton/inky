use futures::StreamExt;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::ffi::CString;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Graphics::Printing::{DRIVER_INFO_3A, EnumPrinterDriversA};
use windows::{
    Win32::{Graphics::*, System::Threading::*},
    core::{PCSTR, PSTR},
};
use windows_registry::LOCAL_MACHINE;

use crate::{pstr_to_string, string_to_pstr};

#[derive(Debug, Default)]
pub struct Driver {
    pub name: String,
    pub inf_path: String,
    data_file: Option<String>,
    config_file: Option<String>,
    driver_version: String,
    date: chrono::NaiveDate,
    version: u32,
}

fn is_driver_installed(driver_name: &str) -> Result<bool, windows::core::Error> {
    let mut needed = 0;
    let mut returned = 0;

    unsafe {
        EnumPrinterDriversA(
            PCSTR::null(),
            windows::core::s!("Windows x64"),
            3,
            None,
            &mut needed,
            &mut returned,
        )?;
    }

    if needed == 0 {
        return Ok(false);
    }

    let mut buffer = vec![0_u8; needed as usize];
    unsafe {
        EnumPrinterDriversA(
            PCSTR::null(),
            windows::core::s!("Windows x64"),
            3,
            Some(buffer.as_mut_slice()),
            &mut needed,
            &mut returned,
        )?;
    }

    let driver_infos = unsafe {
        Vec::from_raw_parts(
            buffer.as_ptr() as *mut DRIVER_INFO_3A,
            returned as usize,
            returned as usize,
        )
    };

    let exists = driver_infos
        .iter()
        .find(|d| {
            if d.pName.is_null() {
                false
            } else {
                unsafe { d.pName.to_string().unwrap() == driver_name }
            }
        })
        .is_some();

    Ok(exists)
}

impl Driver {
    pub fn from_name(name: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let base_path = r"SYSTEM\CurrentControlSet\Control\Print\Environments\Windows x64\Drivers";

        let base_key = LOCAL_MACHINE.open(base_path)?;
        let mut versions = base_key.keys()?;

        while let Some(version) = versions.next() {
            match base_key.open(&format!(r"{}\{}", version, name)) {
                Ok(driver_key) => {
                    let mut driver = Driver::default();
                    driver.name = name.to_string();
                    driver.inf_path = driver_key.get_string("InfPath")?;
                    driver.date = chrono::NaiveDate::parse_from_str(
                        &driver_key.get_string("DriverDate")?,
                        "%m/%d/%Y",
                    )?;
                    driver.driver_version = driver_key.get_string("DriverVersion")?;
                    driver.version = driver_key.get_u32("Version")?;
                    return Ok(driver);
                }
                Err(e) => {
                    continue;
                }
            }
        }

        Err(format!("Driver not found: {}", name).into())
    }

    pub fn get_inf_name(self: &Self) -> &str {
        Path::new(&self.inf_path)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
    }

    pub fn from_inf_file(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let output = Command::new("dism")
            .args(["/Online", "/Get-DriverInfo", &format!("/Driver:{path}")])
            .output()?;
        let output_str = String::from_utf8(output.stdout).unwrap();

        let re = Regex::new(r"Description : (.+)")?;

        let caps = re
            .captures(&output_str)
            .ok_or("Could not parse output from DISM")?;

        let mut driver = Self::default();

        driver.name = caps
            .get(0)
            .ok_or("Expected at least one description entry in driver info")?
            .as_str()
            .to_string();

        Ok(driver)
    }

    pub fn get_all() -> Result<Vec<Self>, Box<dyn std::error::Error>> {
        let base_key = r"SYSTEM\CurrentControlSet\Control\Print\Environments\Windows x64\Drivers";

        let versions = LOCAL_MACHINE
            .open(base_key)?
            .keys()?
            .collect::<Vec<String>>();

        let mut drivers = Vec::new();

        for version in &versions {
            let names = LOCAL_MACHINE
                .open(format!(r"{}\{}", base_key, version))?
                .keys()?
                .collect::<Vec<String>>();
            for name in names {
                let driver_key =
                    LOCAL_MACHINE.open(format!(r"{}\{}\{}", base_key, version, name))?;
                let mut driver = Driver::default();
                driver.name = name.clone();
                driver.inf_path = driver_key.get_string("InfPath")?;
                driver.date = chrono::NaiveDate::parse_from_str(
                    &driver_key.get_string("DriverDate")?,
                    "%m/%d/%Y",
                )?;
                driver.driver_version = driver_key.get_string("DriverVersion")?;
                driver.version = driver_key.get_u32("Version")?;
                drivers.push(driver);
            }
        }

        Ok(drivers)
    }

    /*
     * Aternative method to fetch from Win32 API

    pub fn get_all() -> Result<Vec<Self>, windows::core::Error> {
        let mut bytes_needed = 0;
        let mut num_returned = 0;

        let _ = unsafe {
            Printing::EnumPrinterDriversA(
                windows::core::PSTR::null(),
                windows::core::PSTR::null(),
                2,
                None,
                &mut bytes_needed,
                &mut num_returned,
            )
        };

        if bytes_needed == 0 {
            return Ok(Vec::new());
        }

        let mut buffer = vec![0_u8; bytes_needed as usize];
        num_returned = 0;

        unsafe {
            Printing::EnumPrinterDriversA(
                windows::core::PSTR::null(),
                windows::core::PSTR::null(),
                2,
                Some(&mut buffer),
                &mut bytes_needed,
                &mut num_returned,
            )?
        }

        let driver_infos = unsafe {
            Vec::from_raw_parts(
                buffer.as_ptr() as *mut Printing::DRIVER_INFO_2A,
                num_returned as usize,
                num_returned as usize,
            )
        };

        let drivers = driver_infos
            .iter()
            .map(|info| Self {
                name: pstr_to_string(info.pName).unwrap_or_default(),
                path: pstr_to_string(info.pDriverPath).unwrap_or_default(),
                data_file: pstr_to_string(info.pDataFile),
                config_file: pstr_to_string(info.pConfigFile),
                version: info.cVersion,
                date: chrono::NaiveDate::default(),
            })
            .collect::<Vec<Self>>();

        std::mem::forget(driver_infos);

        Ok(drivers)
    }
    */
}
