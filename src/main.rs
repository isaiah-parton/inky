mod driver;
pub mod port;
pub mod printer;
mod server;

use serde::{Deserialize, Serialize};
use std::ffi::{CString, c_void};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Graphics::Printing::{
    AddPrinterA, ClosePrinter, DRIVER_INFO_3A, EnumPortsA, EnumPrinterDriversA,
    FindFirstPrinterChangeNotification, PORT_INFO_2A, PRINTER_ATTRIBUTE_LOCAL,
    PRINTER_ATTRIBUTE_NETWORK, PRINTER_CHANGE_ALL, PRINTER_HANDLE, PRINTER_INFO_2A,
};
use windows::{
    Win32::{Graphics::*, System::Threading::*},
    core::{PCSTR, PSTR},
};

use driver::*;
use port::*;
use printer::*;
use server::*;

#[derive(Deserialize, Serialize, Default)]
pub struct ClientConfig {
    dry_run: Option<bool>,
    server_address: Option<String>,
    manifest_path: Option<String>,
    sync_interval: Option<Duration>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Manifest {
    printers: Vec<ManifestPrinter>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ManifestPort {
    pub name: Option<String>,
    pub host_name: String,
    pub port_number: Option<u16>,
    pub lpr_queue_name: Option<String>,
    pub snmp_enabled: Option<bool>,
    pub snmp_community: Option<String>,
    pub snmp_index: Option<u32>,
    pub port_type: Option<PortType>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct ManifestPrinter {
    name: String,
    driver_inf_file: String,
    port: ManifestPort,
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

async fn sync_printers(config: &ClientConfig) -> Result<(), Box<dyn std::error::Error>> {
    let manifest = match &config.server_address {
        Some(server_address) => {
            reqwest::get(format!("{}/manifest", server_address))
                .await?
                .json::<Manifest>()
                .await?
        }
        None => match &config.manifest_path {
            Some(path) => std::fs::read_to_string(path)
                .and_then(|s| Ok(serde_json::from_str::<Manifest>(&s)?))?,
            None => Manifest {
                printers: Vec::new(),
            },
        },
    };

    let printers = Printer::get_all()?;

    let mut printers_to_add: Vec<Printer> = manifest
        .printers
        .iter()
        .filter(|p| printers.iter().find(|o| &o.name == &p.name).is_none())
        .map(Printer::from)
        .collect();

    if config.dry_run.is_some_and(|b| b) {
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
            match printer.install().await {
                Ok(()) => {
                    println!("Installed printer:");
                }
                Err(e) => {
                    println!("Failed to install printer: {}", e);
                }
            }
            println!("\tName: {}", printer.name);
            printer
                .host_name
                .as_ref()
                .inspect(|host_name| println!("\tAddress: {}", host_name));
            println!("\tDriver: {}", printer.driver_name);
            printer
                .port
                .as_ref()
                .inspect(|port| println!("\tPort: {}", port.name));
        }
        if printers_to_add.is_empty() {
            println!("All printers up to date!");
        }
    }

    Ok(())
}

async fn listen_to_printer_changes() -> Result<(), Box<dyn std::error::Error>> {
    let mut server_handle = PRINTER_HANDLE::default();
    unsafe {
        Printing::OpenPrinterA(None, &mut server_handle, None)?;
    }

    let change_handle =
        unsafe { FindFirstPrinterChangeNotification(server_handle, PRINTER_CHANGE_ALL, 0, None) };

    if change_handle.is_invalid() {
        return Err("Listener handle is invalid".to_string().into());
    }

    loop {
        let wait_result = unsafe { WaitForSingleObject(change_handle, u32::MAX) };
        if wait_result == WAIT_OBJECT_0 {}
    }
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
        config.dry_run = Some(true);
    }

    if is_server {
        let server_result = Server::new(ServerConfig::default()).start("0.0.0.0").await;
        match server_result {
            Ok(()) => {}
            Err(e) => eprintln!("Failed to start server: {}", e),
        };
    } else {
        match sync_printers(&config).await {
            Ok(()) => {}
            Err(e) => eprintln!("Failed to sync printers: {}", e),
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
