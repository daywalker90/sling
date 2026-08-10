use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Error, anyhow};
use cln_plugin::Plugin;
use cln_rpc::hooks::events::HtlcAcceptedEvent;
use serde_json::json;

use crate::model::PluginState;

pub async fn htlc_handler(
    plugin: Plugin<PluginState>,
    v: serde_json::Value,
) -> Result<serde_json::Value, Error> {
    let hook: HtlcAcceptedEvent = match serde_json::from_value(v) {
        Ok(h) => h,
        Err(e) => {
            log::error!("Error deserializing htlc_accepted hook: {e}");
            return Ok(json!({"result": "continue"}));
        }
    };
    if hook.forward_to.is_some() {
        return Ok(json!({"result": "continue"}));
    }

    let payment_hash = hook.htlc.payment_hash.to_string();

    let mut pays = plugin.state().pays.write();
    if let Some(pi) = pays.remove(&payment_hash) {
        let matching_scid = hook.htlc.short_channel_id == pi.incoming_scid
            || (pi.incoming_alias.is_some()
                && hook.htlc.short_channel_id == pi.incoming_alias.unwrap());
        let expected_amount = hook.htlc.amount_msat.msat() == pi.amount_msat;
        if matching_scid && expected_amount {
            log::debug!("resolving htlc. payment_hash: {payment_hash}");
            Ok(json!({"result":"resolve","payment_key":pi.preimage}))
        } else if let Some(peer) = plugin
            .state()
            .peer_channels
            .lock()
            .get(&hook.htlc.short_channel_id)
        {
            if !matching_scid {
                log::info!(
                    "NOT resolving HTLC from {}: WRONG SCID: {} EXPECTED: {}. \
                    payment_hash: {payment_hash}",
                    peer.peer_id,
                    hook.htlc.short_channel_id,
                    pi.incoming_scid
                );
            }

            if !expected_amount {
                log::warn!(
                    "NOT resolving HTLC from {}: WRONG AMOUNT: {} EXPECTED: {}. \
                    payment_hash: {payment_hash}",
                    peer.peer_id,
                    hook.htlc.amount_msat.msat(),
                    pi.amount_msat
                );
            }

            plugin.state().bad_fwd_nodes.lock().insert(
                peer.peer_id,
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            );

            pays.insert(payment_hash, pi);

            Ok(json!({"result": "fail", "failure_message": "1007"}))
        } else {
            Ok(json!({"result": "fail", "failure_message": "1007"}))
        }
    } else {
        Ok(json!({"result": "continue"}))
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
