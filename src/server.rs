mod printer;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tide::listener::ToListener;
use tide::prelude::*;
use tide::{Request, Response};
use tokio::sync::Mutex;
use windows::{
    Win32::{Graphics::*, System::Threading::*},
    core::{PCSTR, PSTR},
};

use crate::printer::*;

#[derive(Deserialize, Serialize, Default, Clone)]
pub struct ServerConfig {
    manifest_path: String,
}

#[derive(Clone)]
pub struct Server {
    config: ServerConfig,
    printers_by_machine: Mutex<HashMap<String, Vec<Printer>>>,
}

impl Server {
    pub fn new(config: ServerConfig) -> Self {
        Self {
            config: config,
            printers_by_machine: Mutex::new(HashMap::new()),
        }
    }

    pub async fn start<L>(self: Self, address: L) -> Result<(), Box<dyn std::error::Error>>
    where
        L: ToListener<Arc<Server>>,
    {
        let manifest_path = self.config.manifest_path.clone();
        let mut app = tide::with_state(Arc::new(self));

        app.at("/manifest").serve_file(manifest_path)?;
        app.at("/my_id").get(handle_get_my_id);
        app.at("/:id/printers").put(handle_update_printers);
        app.listen(address).await?;

        Ok(())
    }
}

#[derive(Deserialize)]
struct UpdatePrintersRequest {
    printers: Vec<Printer>,
}

async fn handle_update_printers(mut req: Request<Arc<Server>>) -> tide::Result {
    let data = req.body_json::<UpdatePrintersRequest>().await?;
    let id = req.param("id")?;
    req.state()
        .printers_by_machine
        .lock()
        .await
        .insert(id.to_string(), data.printers);
    Ok(Response::new(200))
}
