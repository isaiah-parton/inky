#[derive(Debug, Default, Clone)]
pub struct Port {
    name: String,
    host_name: String,
}

impl Port {
    pub fn new(name: impl Into<String>, host_name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            host_name: host_name.into(),
        }
    }

    pub fn create(self: Self) -> Result<Self, Box<dyn std::error::Error>> {
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

    pub fn update(self: &Self) -> Result<(), Box<dyn std::error::Error>> {
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

    pub fn get_all() -> Result<Vec<Self>, Box<dyn std::error::Error>> {
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
