use futures::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use std::{
    num,
    time::{Duration, Instant},
};
use tide::Request;
use tide::prelude::*;
use windows::Win32::Graphics::{Printing::DRIVER_INFO_1A, *};

#[derive(Deserialize, Serialize, Default)]
struct ClientConfig {
    server_address: String,
    manifest_path: Option<String>,
    sync_interval: Option<Duration>,
}

#[derive(Deserialize, Serialize, Default)]
struct ServerConfig {
    manifest_path: String,
}

#[derive(Deserialize, Serialize)]
struct Manifest {
    printers: Vec<ManifestPrinter>,
}

#[derive(Deserialize, Serialize)]
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

#[derive(Debug)]
struct Driver {
    name: String,
    path: String,
    data_file: String,
    config_file: String,
    version: u32,
}

impl Driver {
    fn get_all() -> Result<Vec<Self>, windows::core::Error> {
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
                data_file: pstr_to_string(info.pDataFile).unwrap_or_default(),
                config_file: pstr_to_string(info.pConfigFile).unwrap_or_default(),
                version: info.cVersion,
            })
            .collect::<Vec<Self>>();

        std::mem::forget(driver_infos);

        Ok(drivers)
    }
}

#[derive(Debug)]
struct Printer {
    name: String,
    port_name: String,
    driver_name: String,
    host_name: Option<String>,
}

impl Printer {
    fn get_all() -> Result<Vec<Self>, windows::core::Error> {
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
            Vec::from_raw_parts(
                buffer.as_ptr() as *mut Printing::PRINTER_INFO_2A,
                num_returned as usize,
                num_returned as usize,
            )
        };

        let printers = printer_infos
            .iter()
            .map(|info| {
                return Printer {
                    name: unsafe { info.pPrinterName.to_string().unwrap_or_default() },
                    port_name: if info.pPortName.is_null() {
                        String::new()
                    } else {
                        unsafe { info.pPortName.to_string().unwrap_or_default() }
                    },
                    driver_name: if info.pDriverName.is_null() {
                        String::new()
                    } else {
                        unsafe { info.pDriverName.to_string().unwrap_or_default() }
                    },
                    host_name: if info.pPortName.is_null() {
                    	None
                    } else {
                    	let path = format!(r"SYSTEM\CurrentControlSet\Control\Print\Monitors\Standard TCP/IP Port\Ports\{}", unsafe { info.pPortName.to_string().unwrap_or_default() });
                    	windows_registry::LOCAL_MACHINE.open(path).and_then(|key| key.get_string("HostName")).ok()
                    }
                };
            })
            .collect::<Vec<Printer>>();

        std::mem::forget(printer_infos);

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

    // let printers_to_add = manifest.printers.iter().filter(|p| {
    //     printers
    //         .iter()
    //         .find(|o| o.host_name.as_ref().is_some_and(|v| v == &p.host_name))
    //         .is_some()
    // });

    println!("Drivers: {:#?}", drivers);
    println!("Printers: {:#?}", printers);

    Ok(())
}

async fn serve(config: &ServerConfig) -> Result<(), Box<dyn std::error::Error>> {
    let mut app = tide::new();
    app.at("/manifest").serve_file(&config.manifest_path)?;
    Ok(())
}

#[tokio::main]
async fn main() {
    println!("Launching Printer Sync Tool");

    // Stuff configurable by args
    let mut is_server = true;
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

    config.manifest_path = Some(manifest_path);

    match sync_printers(&config).await {
        Ok(()) => {}
        Err(e) => eprintln!("Error syncing printers: {}", e),
    };

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
