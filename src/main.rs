mod driver;

use futures::{StreamExt, stream};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::ffi::{CString, c_void};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};
use tide::Request;
use tide::prelude::*;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Graphics::Printing::{
    AddPrinterA, DRIVER_INFO_3A, EnumPortsA, EnumPrinterDriversA, PORT_INFO_2A,
    PRINTER_ATTRIBUTE_LOCAL, PRINTER_ATTRIBUTE_NETWORK, PRINTER_INFO_2A,
};
use windows::{
    Win32::{Graphics::*, System::Threading::*},
    core::{PCSTR, PSTR},
};
use windows_registry::LOCAL_MACHINE;

use driver::*;

#[derive(Deserialize, Serialize, Default)]
struct ClientConfig {
    dry_run: bool,
    server_address: String,
    manifest_path: Option<String>,
    sync_interval: Option<Duration>,
}

#[derive(Deserialize, Serialize, Default)]
struct ServerConfig {
    manifest_path: String,
}

#[derive(Deserialize, Serialize, Debug)]
struct Manifest {
    printers: Vec<ManifestPrinter>,
}

#[derive(Deserialize, Serialize, Debug)]
struct ManifestPrinter {
    name: String,
    host_name: String,
    driver_inf_file: String,
}

fn pstr_to_string(pstr: windows::core::PSTR) -> Option<String> {
    if pstr.is_null() {
        None
    } else {
        unsafe { pstr.to_string().ok() }
    }
}

fn string_to_pstr(s: &str) -> PSTR {
    PSTR::from_raw(CString::new(s).unwrap().into_bytes().as_mut_ptr())
}

#[derive(Debug, Default, Clone)]
struct Port {
    name: String,
    host_name: String,
}

impl Port {
    fn new(name: impl Into<String>, host_name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            host_name: host_name.into(),
        }
    }

    fn create(self: Self) -> Result<Self, Box<dyn std::error::Error>> {
        let status = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "Add-PrinterPort -Name '{}' -PrinterHostAddress '{}'",
                    &self.name, &self.host_name
                ),
            ])
            .status()?;
        if status.success() {
            Ok(self)
        } else {
            Err(format!("Add-PrinterPort failed: {status}").into())
        }
    }

    fn update(self: &Self) -> Result<(), Box<dyn std::error::Error>> {
        let status = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "Set-PrinterPort -Name '{}' -PrinterHostAddress '{}'",
                    &self.name, &self.host_name
                ),
            ])
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("Set-PrinterPort failed: {status}").into())
        }
    }

    fn get_all() -> Result<Vec<Self>, Box<dyn std::error::Error>> {
        let base_key =
            r"SYSTEM\CurrentControlSet\Control\Print\Monitors\Standard TCP/IP Port\Ports";
        let ports_key = LOCAL_MACHINE.open(base_key)?;
        let ports: Vec<Self> = ports_key
            .keys()?
            .map(|name| -> windows::core::Result<Self> {
                let host_name = LOCAL_MACHINE
                    .open(format!(r"{}\{}", base_key, name))?
                    .get_string("HostName")?;
                Ok(Port {
                    name: name,
                    host_name: host_name,
                })
            })
            .collect::<Result<Vec<Self>, _>>()?;
        Ok(ports)
    }
}

#[derive(Debug, Default)]
struct Printer {
    name: String,
    driver_name: String,
    port_name: Option<String>,
    host_name: Option<String>,
    // If present, then the printer was either installed, or a valid driver
    // was detected when it was loaded
    inf_path: Option<String>,
}

impl From<&ManifestPrinter> for Printer {
    fn from(input: &ManifestPrinter) -> Printer {
        let mut result = Self::default();
        result.name = input.name.clone();
        result.host_name = Some(input.host_name.clone());
        result.inf_path = Some(input.driver_inf_file.clone());
        result
    }
}

impl Printer {
    fn ensure_port(self: &mut Self) -> Result<(), Box<dyn std::error::Error>> {
        if self.port_name.is_some() {
            return Ok(());
        }
        let ports = Port::get_all()?;
        match &self.host_name {
            Some(host_name) => {
                let existing_port = ports.iter().find(|p| &p.host_name == host_name);
                match existing_port {
                    Some(port) => {
                        self.port_name = Some(port.name.to_owned());
                        Ok(())
                    }
                    None => {
                        let new_port = Port::new(host_name, host_name).create()?;
                        self.port_name = Some(new_port.name);
                        Ok(())
                    }
                }
            }
            None => Err("Expected a host name".to_string().into()),
        }
    }

    fn ensure_driver(self: &mut Self) -> Result<(), Box<dyn std::error::Error>> {
        let drivers = Driver::get_all()?;

        match &self.inf_path {
            Some(inf_path) => {
                match drivers.iter().find(|d| {
                    d.get_inf_name()
                        == Path::new(&inf_path)
                            .file_name()
                            .unwrap()
                            .to_ascii_lowercase()
                }) {
                    Some(driver) => {
                        self.driver_name = driver.name.to_owned();
                    }
                    None => {
                        self.driver_name = String::new();
                    }
                };
            }
            None => {
                return Err("Expected a .inf file path".to_string().into());
            }
        }

        Ok(())
    }

    fn install(self: &mut Self) -> Result<(), Box<dyn std::error::Error>> {
        self.ensure_port()?;
        self.ensure_driver()?;

        println!("Installing printer");
        println!("\tName: {}", self.name);
        println!("\tDriver: {}", self.driver_name);
        self.port_name
            .as_ref()
            .inspect(|port_name| println!("\tPort: {}", port_name));

        let pi2 = PRINTER_INFO_2A {
            pPrinterName: string_to_pstr(&self.name),
            pDriverName: string_to_pstr(&self.driver_name),
            pPortName: string_to_pstr(
                &self
                    .port_name
                    .as_ref()
                    .ok_or("Expected a port name".to_string())?,
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
            CloseHandle(handle)?;
        }

        Ok(())
    }

    fn get_all() -> Result<Vec<Self>, Box<dyn std::error::Error>> {
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
                Ok(Printer {
                    name: pstr_to_string(info.pPrinterName).unwrap_or_default(),
                    port_name: pstr_to_string(info.pPortName),
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

async fn sync_printers(config: &ClientConfig) -> Result<(), Box<dyn std::error::Error>> {
    println!("Syncing with main server");

    let manifest = match &config.manifest_path {
        Some(path) => {
            std::fs::read_to_string(path).and_then(|s| Ok(serde_json::from_str::<Manifest>(&s)?))?
        }
        None => {
            reqwest::get(format!("{}/manifest", config.server_address))
                .await?
                .json::<Manifest>()
                .await?
        }
    };

    let printers = Printer::get_all()?;
    let drivers = Driver::get_all()?;

    let mut drivers_to_add = Vec::new();

    for printer in &printers {
        if drivers
            .iter()
            .find(|d| d.name == printer.driver_name)
            .is_none()
        {
            drivers_to_add.push(printer.driver_name.clone());
        }
    }

    let mut printers_to_add: Vec<Printer> = manifest
        .printers
        .iter()
        .filter(|p| printers.iter().find(|o| &o.name == &p.name).is_none())
        .map(Printer::from)
        .collect();

    if config.dry_run {
        println!("Printing results for dry-run:");
        if printers_to_add.is_empty() {
            println!("\tNo printers would be installed");
        } else {
            println!("\tPrinters to be installed:");
            for printer in printers_to_add {
                println!("\t\tName: {}", printer.name);
                printer
                    .host_name
                    .inspect(|host_name| println!("\t\tHost: {}", host_name));
            }
        }
    } else {
        for printer in &mut printers_to_add {
            printer.install()?;
            println!("Installed printer: {}", printer.name);
        }
    }

    Ok(())
}

async fn serve(config: &ServerConfig) -> Result<(), Box<dyn std::error::Error>> {
    let mut app = tide::new();
    app.at("/manifest").serve_file(&config.manifest_path)?;
    Ok(())
}

async fn install_driver_pnputil(inf_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let si = STARTUPINFOA::default();
    let mut pi = PROCESS_INFORMATION::default();

    let _ = unsafe {
        CreateProcessA(
            PCSTR::null(),
            Some(PSTR::from_raw(
                CString::new(format!("pnputil /add-driver \"{}\" /install", inf_path))
                    .unwrap()
                    .into_bytes()
                    .as_mut_ptr(),
            )),
            None,
            None,
            false,
            CREATE_NO_WINDOW,
            None,
            None,
            &si,
            &mut pi,
        )?
    };

    unsafe {
        WaitForSingleObject(pi.hProcess, INFINITE);
    }
    let mut exit_code = 0;
    unsafe {
        GetExitCodeProcess(pi.hProcess, &mut exit_code)?;
        CloseHandle(pi.hProcess)?;
        CloseHandle(pi.hThread)?;
    }

    if exit_code != 0 {
        return Err(format!("Sub-process exited with code: {}", exit_code).into());
    }

    Ok(())
}

#[tokio::main]
async fn main() {
    println!("Launching Printer Sync Tool");

    // Stuff configurable by args
    let mut is_server = false;
    let mut dry_run = false;
    let mut config_path = String::from("config.json");
    let mut manifest_path = String::from("manifest.json");

    // Parse args
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        if arg == "--serve" {
            is_server = true;
        } else if arg == "--config" {
            config_path = args
                .next()
                .expect("Expected an argument to follow the --config flag")
                .clone();
        } else if arg == "--manifest" {
            manifest_path = args
                .next()
                .expect("Expected an argument to follow the --manifest flag")
                .clone();
        } else if arg == "--dry" {
            dry_run = true;
        } else {
            panic!("Unrecognized argument {}", arg);
        }
    }

    let mut config =
        if std::fs::exists(&config_path).expect("Couldn't check existence of config file") {
            std::fs::read_to_string(&config_path)
                .and_then(|s| Ok(serde_json::from_str::<ClientConfig>(&s).unwrap_or_default()))
                .unwrap_or_default()
        } else {
            ClientConfig::default()
        };

    // Overwrite manifest path from args
    config.manifest_path = Some(manifest_path);
    if dry_run {
        config.dry_run = true;
    }

    if is_server {
        match serve(&ServerConfig::default()).await {
            Ok(()) => {}
            Err(e) => eprintln!("Error starting server: {}", e),
        };
    } else {
        match sync_printers(&config).await {
            Ok(()) => {}
            Err(e) => eprintln!("Error syncing printers: {}", e),
        };
    }

    // let interval = tokio::time::interval(Duration::from_secs(3));

    // let forever = futures::stream::unfold(interval, |mut interval| async {
    //     interval.tick().await;
    //     match sync_printers(&config).await {
    //         Ok(()) => {}
    //         Err(e) => eprintln!("Error syncing printers: {}", e),
    //     };
    //     Some(((), interval))
    // });

    // // let now = Instant::now();
    // forever.for_each(|_| async {}).await;
}
