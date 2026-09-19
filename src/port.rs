use serde::{Deserialize, Serialize};
use std::fmt::Write;
use tokio::process::Command;
use windows_registry::LOCAL_MACHINE;

use crate::ManifestPort;

pub struct PortBuilder {
    port: Port,
}

impl PortBuilder {
    pub fn new() -> Self {
        Self {
            port: Port::default(),
        }
    }

    pub fn name(self: &mut Self, name: &str) -> &mut Self {
        self.port.name = name.to_owned();
        self
    }

    pub fn host_name(self: &mut Self, host_name: &str) -> &mut Self {
        self.port.host_name = host_name.to_string();
        self
    }

    pub fn lpr(self: &mut Self) -> &mut Self {
        self.port.port_type = PortType::Lpr;
        self
    }
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PortType {
    #[default]
    Raw,
    Lpr,
}

impl TryFrom<u32> for PortType {
    type Error = String;

    fn try_from(v: u32) -> Result<Self, Self::Error> {
        match v {
            1 => Ok(PortType::Raw),
            2 => Ok(PortType::Lpr),
            _ => Err(format!("Invalid port protocol index: {}", v)),
        }
    }
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct Port {
    pub name: String,
    pub host_name: String,
    pub port_type: PortType,
    pub snmp_community: Option<String>,
    pub snmp_index: Option<u32>,
    pub lpr_queue_name: Option<String>,
    pub port_number: Option<u16>,
    pub snmp_enabled: bool,
}

impl Port {
    pub fn new(name: impl Into<String>, host_name: impl Into<String>) -> Self {
        let mut result = Self::default();
        result.name = name.into();
        result.host_name = host_name.into();
        result
    }

    pub fn from_config(config: &ManifestPort) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            name: config.name.as_ref().unwrap_or(&config.host_name).to_owned(),
            port_number: config.port_number,
            snmp_enabled: config.snmp_enabled.unwrap_or_default(),
            snmp_index: config.snmp_index,
            snmp_community: config.snmp_community.to_owned(),
            lpr_queue_name: config.lpr_queue_name.to_owned(),
            port_type: config.port_type.to_owned().unwrap_or_default(),
            host_name: config.host_name.to_owned(),
        })
    }

    fn get_command_args(self: &Self) -> String {
        let mut args = String::new();
        write!(args, "-Name '{}'", &self.name).unwrap();
        match self.port_type {
            PortType::Raw => {
                write!(args, "-PrinterHostAddress '{}'", &self.host_name).unwrap();
            }
            PortType::Lpr => {
                write!(
                    args,
                    "-LprQueueName '{}' -LprHostAddress '{}' -LprByteCounting",
                    &self.lpr_queue_name.to_owned().unwrap_or("LPR".to_string()),
                    &self.host_name
                )
                .unwrap();
            }
        }
        if self.snmp_enabled {
            write!(
                args,
                "-SNMP {} -SNMPCommunity '{}'",
                self.snmp_index.unwrap_or(1),
                self.snmp_community
                    .to_owned()
                    .unwrap_or("public".to_string())
            )
            .unwrap();
        }
        args
    }

    pub async fn create(self: Self) -> Result<Self, Box<dyn std::error::Error>> {
        let status = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "Add-PrinterPort",
                &self.get_command_args(),
            ])
            .status()
            .await?;
        if status.success() {
            Ok(self)
        } else {
            Err(format!("Add-PrinterPort failed: {status}").into())
        }
    }

    pub async fn update(self: Self) -> Result<Self, Box<dyn std::error::Error>> {
        let status = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "Set-PrinterPort",
                &self.get_command_args(),
            ])
            .status()
            .await?;
        if status.success() {
            Ok(self)
        } else {
            Err(format!("Set-PrinterPort failed: {status}").into())
        }
    }

    pub fn get(name: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let base_key =
            r"SYSTEM\CurrentControlSet\Control\Print\Monitors\Standard TCP/IP Port\Ports";
        let port_key = LOCAL_MACHINE.open(format!(r"{}\{}", base_key, name))?;
        Ok(Port {
            name: name.to_string(),
            port_type: port_key.get_u32("Protocol")?.try_into()?,
            host_name: port_key.get_string("HostName")?,
            snmp_index: port_key.get_u32("SNMP Index").ok(),
            snmp_enabled: port_key
                .get_u32("SNMP Enabled")
                .and_then(|n| Ok(n != 0))
                .unwrap_or_default(),
            snmp_community: port_key.get_string("SNMP Community").ok(),
            port_number: port_key
                .get_u32("PortNumber")
                .and_then(|n| Ok(n as u16))
                .ok(),
            lpr_queue_name: port_key.get_string("Queue").ok(),
        })
    }

    pub fn get_all() -> Result<Vec<Self>, Box<dyn std::error::Error>> {
        let base_key =
            r"SYSTEM\CurrentControlSet\Control\Print\Monitors\Standard TCP/IP Port\Ports";
        let ports_key = LOCAL_MACHINE.open(base_key)?;
        let ports: Vec<Self> = ports_key
            .keys()?
            .map(|name| -> Result<Self, Box<dyn std::error::Error>> {
                let port_key = LOCAL_MACHINE.open(format!(r"{}\{}", base_key, name))?;
                Ok(Port {
                    name: name,
                    port_type: port_key.get_u32("Protocol")?.try_into()?,
                    host_name: port_key.get_string("HostName")?,
                    snmp_index: port_key.get_u32("SNMP Index").ok(),
                    snmp_enabled: port_key
                        .get_u32("SNMP Enabled")
                        .and_then(|n| Ok(n != 0))
                        .unwrap_or_default(),
                    snmp_community: port_key.get_string("SNMP Community").ok(),
                    port_number: port_key
                        .get_u32("PortNumber")
                        .and_then(|n| Ok(n as u16))
                        .ok(),
                    lpr_queue_name: port_key.get_string("Queue").ok(),
                })
            })
            .collect::<Result<Vec<Self>, _>>()?;
        Ok(ports)
    }
}
