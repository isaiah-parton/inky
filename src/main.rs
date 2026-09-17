mod driver;
pub mod port;
pub mod printer;
mod server;

use futures::{StreamExt, stream};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::{CString, c_void};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tide::listener::ToListener;
use tide::prelude::*;
use tide::{Request, Response};
use tokio::sync::Mutex;
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
use windows_registry::LOCAL_MACHINE;

use driver::*;
use port::*;
use printer::*;
use server::*;

#[derive(Deserialize, Serialize, Default)]
pub struct ClientConfig {
    dry_run: bool,
    server_address: String,
    manifest_path: Option<String>,
    sync_interval: Option<Duration>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Manifest {
    printers: Vec<ManifestPrinter>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct ManifestPrinter {
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

    println!("{:#?}", printers);

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
            match printer.install().await {
                Ok(()) => {
                    println!("Installed printer: {}", printer.name);
                }
                Err(e) => {
                    eprintln!("Failed to install printer: {}", e);
                }
            }
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
        config.dry_run = true;
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
