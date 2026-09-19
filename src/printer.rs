use futures::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use std::path::Path;
use windows::Win32::Graphics::Printing::{
    AddPrinterA, ClosePrinter, PRINTER_ATTRIBUTE_NETWORK, PRINTER_CHANGE_ALL, PRINTER_HANDLE,
    PRINTER_INFO_2A,
};
use windows::{
    Win32::{Graphics::*, System::Threading::*},
    core::{PCSTR, PSTR},
};

use crate::{ManifestPort, ManifestPrinter, driver::*, port::*, pstr_to_string, string_to_pstr};

#[derive(Debug, Default, Deserialize, Clone)]
pub struct Printer {
    pub name: String,
    pub driver_name: String,
    pub host_name: Option<String>,
    pub port_config: Option<ManifestPort>,
    pub port: Option<Port>,
    // If present, then the printer was either installed, or a valid driver
    // was detected when it was loaded
    inf_path: Option<String>,
}

impl From<&ManifestPrinter> for Printer {
    fn from(input: &ManifestPrinter) -> Printer {
        let mut result = Self::default();
        result.name = input.name.clone();
        result.host_name = Some(input.port.host_name.clone());
        result.inf_path = Some(input.driver_inf_file.clone());
        result
    }
}

impl Printer {
    pub async fn ensure_port(self: &mut Self) -> Result<(), Box<dyn std::error::Error>> {
        if self.port.is_some() {
            return Ok(());
        }
        let ports = Port::get_all()?;
        match &self.host_name {
            Some(host_name) => {
                let existing_port = ports.iter().find(|p| &p.host_name == host_name);
                match existing_port {
                    Some(port) => {
                        self.port = Some(port.to_owned());
                        Ok(())
                    }
                    None => {
                        let new_port = Port::new(host_name, host_name).create().await?;
                        self.port = Some(new_port);
                        Ok(())
                    }
                }
            }
            None => Err("Expected a host name".to_string().into()),
        }
    }

    /*
     * Searches for the printer's driver on the system and installs it
     * if not found. Runs window's pnputil command to accomplish this.
     */
    pub async fn ensure_driver(self: &mut Self) -> Result<(), Box<dyn std::error::Error>> {
        match &self.inf_path {
            Some(inf_path) => {
                // Determine the printer's driver's inf file name
                let printer_inf_file_name = Path::new(&inf_path)
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_ascii_lowercase();
                // Look for an existing driver
                match Driver::get_all()?
                    .iter()
                    .find(|d| d.get_inf_name() == printer_inf_file_name)
                {
                    Some(driver) => {
                        // Found it, assign its name
                        self.driver_name = driver.name.to_owned();
                    }
                    None => {
                        // Attempt driver installation
                        install_driver_pnputil(inf_path).await?;
                        match Driver::get_all()?
                            .iter()
                            .find(|d| d.get_inf_name() == printer_inf_file_name)
                        {
                            Some(found_driver) => {
                                self.driver_name = found_driver.name.to_owned();
                            }
                            None => {
                                return Err(format!(
                                    "Could not find the newly installed driver: {}",
                                    printer_inf_file_name
                                )
                                .into());
                            }
                        }
                    }
                };
            }
            None => {
                return Err("Expected a .inf file path".to_string().into());
            }
        }

        Ok(())
    }

    pub async fn install(self: &mut Self) -> Result<(), Box<dyn std::error::Error>> {
        self.ensure_port().await?;
        self.ensure_driver().await?;

        let pi2 = PRINTER_INFO_2A {
            pPrinterName: string_to_pstr(&self.name),
            pDriverName: string_to_pstr(&self.driver_name),
            pPortName: string_to_pstr(
                &self
                    .port
                    .as_ref()
                    .ok_or("Printer must have a port".to_string())?
                    .name,
            ),
            pPrintProcessor: string_to_pstr("winprint"),
            pDatatype: string_to_pstr("RAW"),
            Attributes: PRINTER_ATTRIBUTE_NETWORK,
            Priority: 1,
            DefaultPriority: 1,
            ..Default::default()
        };

        unsafe {
            let handle = AddPrinterA(PSTR::null(), 2, &pi2 as *const _ as *const u8)?;
            ClosePrinter(PRINTER_HANDLE { Value: handle.0 })?;
        }

        Ok(())
    }

    pub fn get_all() -> Result<Vec<Self>, Box<dyn std::error::Error>> {
        let mut bytes_needed = 0;
        let mut num_returned = 0;

        let _ = unsafe {
            Printing::EnumPrintersA(
                Printing::PRINTER_ENUM_LOCAL | Printing::PRINTER_ENUM_NETWORK,
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
            Printing::EnumPrintersA(
                Printing::PRINTER_ENUM_LOCAL | Printing::PRINTER_ENUM_NETWORK,
                windows::core::PSTR::null(),
                2,
                Some(&mut buffer),
                &mut bytes_needed,
                &mut num_returned,
            )?;
        }

        let printer_infos = unsafe {
            std::slice::from_raw_parts(
                buffer.as_ptr() as *mut Printing::PRINTER_INFO_2A,
                num_returned as usize,
            )
        };

        let printers = printer_infos
            .iter()
            .map(|info| -> Result<Printer, Box<dyn std::error::Error>> {
            	let driver_name = pstr_to_string(info.pDriverName).unwrap_or_default();
             	let driver = Driver::from_name(&driver_name)?;
              	let port_name = pstr_to_string(info.pPortName);
                Ok(Printer {
                    name: pstr_to_string(info.pPrinterName).unwrap_or_default(),
                    port: port_name.as_ref().and_then(|name| Port::get(&name).ok()),
                    port_config: None,
                    driver_name: driver_name,
                    inf_path: Some(driver.inf_path.to_string()),
                    host_name: if info.pPortName.is_null() {
                    	None
                    } else {
                    	let path = format!(r"SYSTEM\CurrentControlSet\Control\Print\Monitors\Standard TCP/IP Port\Ports\{}", unsafe { info.pPortName.to_string().unwrap_or_default() });
                    	windows_registry::LOCAL_MACHINE.open(path).and_then(|key| key.get_string("HostName")).ok()
                    }
                })
            })
            .collect::<Result<Vec<Printer>, _>>()?;

        return Ok(printers);
    }
}
