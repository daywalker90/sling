use anyhow::{Error, Ok};
use cln_plugin::Plugin;
use log::debug;
use serde_json::json;

use crate::PluginState;

pub async fn htlc_handler(
    plugin: Plugin<PluginState>,
    v: serde_json::Value,
) -> Result<serde_json::Value, Error> {
    match v.get("htlc") {
        Some(htlc) => match htlc.get("payment_hash") {
            Some(ph) => {
                let mut pays = plugin.state().pays.write();
                let ph_str = ph.as_str().unwrap();
                if pays.contains_key(ph_str) {
                    let pi = pays.remove(ph_str).unwrap();
                    debug!(
                        "htlc_handler: payment_hash: {} payment_preimage: {}",
                        ph_str, pi
                    );
                    Ok(json!({"result":"resolve","payment_key":pi}))
                } else {
                    Ok(json!({"result": "continue"}))
                }
            }

            None => Ok(json!({"result": "continue"})),
        },
        None => Ok(json!({"result": "continue"})),
    }
}
