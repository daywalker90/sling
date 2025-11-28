use std::{
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Error, Ok};
use cln_plugin::Plugin;
use cln_rpc::primitives::ShortChannelId;
use serde_json::json;

use crate::model::PluginState;

pub async fn htlc_handler(
    plugin: Plugin<PluginState>,
    v: serde_json::Value,
) -> Result<serde_json::Value, Error> {
    if v.get("forward_to").is_some() {
        return Ok(json!({"result": "continue"}));
    }
    match v.get("htlc") {
        Some(htlc) => {
            let payment_hash = match htlc.get("payment_hash") {
                Some(ph) => ph.as_str().unwrap(),
                None => return Ok(json!({"result": "continue"})),
            };
            let scid = match htlc.get("short_channel_id") {
                Some(scid) => ShortChannelId::from_str(scid.as_str().unwrap()).unwrap(),
                None => return Ok(json!({"result": "continue"})),
            };

            let mut pays = plugin.state().pays.write();
            if let Some(pi) = pays.remove(payment_hash) {
                if scid == pi.incoming_scid
                    || (pi.incoming_alias.is_some() && scid == pi.incoming_alias.unwrap())
                {
                    log::debug!("resolving htlc. payment_hash: {payment_hash}");
                    Ok(json!({"result":"resolve","payment_key":pi.preimage}))
                } else if let Some(peer) = plugin.state().peer_channels.lock().get(&scid) {
                    log::info!(
                        "NOT resolving HTLC from {}: WRONG SCID: {scid} EXPECTED: {}. \
                    payment_hash: {payment_hash}",
                        peer.peer_id,
                        pi.incoming_scid
                    );

                    plugin.state().bad_fwd_nodes.lock().insert(
                        peer.peer_id,
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_secs(),
                    );

                    pays.insert(payment_hash.to_owned(), pi);

                    Ok(json!({"result": "fail", "failure_message": "1007"}))
                } else {
                    Ok(json!({"result": "fail", "failure_message": "1007"}))
                }
            } else {
                Ok(json!({"result": "continue"}))
            }
        }
        None => Ok(json!({"result": "continue"})),
    }
}

pub async fn block_added(plugin: Plugin<PluginState>, v: serde_json::Value) -> Result<(), Error> {
    let block = if let Some(b) = v.get("block") {
        b
    } else if let Some(b) = v.get("block_added") {
        b
    } else {
        return Err(anyhow!("could not read block notification"));
    };
    if let Some(h) = block.get("height") {
        *plugin.state().blockheight.lock() = u32::try_from(h.as_u64().unwrap())?;
    } else {
        return Err(anyhow!("could not find height for block"));
    }

    Ok(())
}
