use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Error, anyhow};
use cln_plugin::Plugin;
use cln_rpc::primitives::{Amount, PublicKey, Secret, Sha256, ShortChannelId};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::model::PluginState;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HtlcAcceptedEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forward_to: Option<Sha256>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_id: Option<PublicKey>,
    pub htlc: HtlcAcceptedHtlc,
    pub onion: HtlcAcceptedOnion,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HtlcAcceptedHtlc {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_tlvs: Option<String>,
    pub amount_msat: Amount,
    pub cltv_expiry: u32,
    pub cltv_expiry_relative: i64,
    pub id: u64,
    pub payment_hash: Sha256,
    pub short_channel_id: ShortChannelId,
}

/// ['Indicates that the payload is TLV formatted.', 'Only present if the payload was successfully parsed.']
#[derive(Copy, Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[allow(non_camel_case_types, clippy::upper_case_acronyms)]
pub enum HtlcAcceptedOnionType {
    #[serde(rename = "tlv")]
    TLV = 0,
}

impl TryFrom<i32> for HtlcAcceptedOnionType {
    type Error = anyhow::Error;
    fn try_from(c: i32) -> Result<HtlcAcceptedOnionType, anyhow::Error> {
        match c {
            0 => Ok(HtlcAcceptedOnionType::TLV),
            o => Err(anyhow::anyhow!(
                "Unknown variant {} for enum HtlcAcceptedOnionType",
                o
            )),
        }
    }
}

#[allow(clippy::to_string_trait_impl)]
impl ToString for HtlcAcceptedOnionType {
    fn to_string(&self) -> String {
        match self {
            HtlcAcceptedOnionType::TLV => "TLV",
        }
        .to_string()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HtlcAcceptedOnion {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forward_msat: Option<Amount>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub item_type: Option<HtlcAcceptedOnionType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_node_id: Option<PublicKey>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outgoing_cltv_value: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payment_metadata: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payment_secret: Option<Secret>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short_channel_id: Option<ShortChannelId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_msat: Option<Amount>,
    pub next_onion: String,
    pub payload: String,
    pub shared_secret: Secret,
}

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
    let cltv_final = i64::from(plugin.state().config.lock().cltv_final);

    let mut pays = plugin.state().pays.write();
    if let Some(pi) = pays.get(&payment_hash) {
        let matching_scid = hook.htlc.short_channel_id == pi.incoming_scid
            || (pi.incoming_alias.is_some()
                && hook.htlc.short_channel_id == pi.incoming_alias.unwrap());
        let expected_amount = hook.htlc.amount_msat.msat() == pi.amount_msat;
        let matching_cltv = hook
            .onion
            .outgoing_cltv_value
            .is_some_and(|outgoing_cltv| hook.htlc.cltv_expiry >= outgoing_cltv);
        let sufficient_cltv = hook.htlc.cltv_expiry_relative >= cltv_final;
        let is_final_hop = hook.onion.short_channel_id.is_none()
            && hook.onion.next_node_id.is_none()
            && hook.onion.item_type == Some(HtlcAcceptedOnionType::TLV);
        let matching_onion_amount = hook
            .onion
            .forward_msat
            .is_some_and(|amt| amt.msat() == pi.amount_msat)
            && hook
                .onion
                .total_msat
                .is_some_and(|amt| amt.msat() == pi.amount_msat);
        let matching_payment_secret = hook
            .onion
            .payment_secret
            .is_some_and(|ps| ps.to_vec() == pi.payment_secret.to_vec());

        if matching_scid
            && expected_amount
            && matching_cltv
            && sufficient_cltv
            && is_final_hop
            && matching_onion_amount
            && matching_payment_secret
        {
            let pi = pays.remove(&payment_hash).unwrap();
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

            if !matching_cltv {
                log::warn!(
                    "NOT resolving HTLC from {}: CLTV EXPIRY TOO LOW FOR ONION: {} EXPECTED >= {:?}. \
                    payment_hash: {payment_hash}",
                    peer.peer_id,
                    hook.htlc.cltv_expiry,
                    hook.onion.outgoing_cltv_value
                );
            }

            if !sufficient_cltv {
                log::warn!(
                    "NOT resolving HTLC from {}: CLTV EXPIRY RELATIVE TOO LOW: {} EXPECTED >= {}. \
                    payment_hash: {payment_hash}",
                    peer.peer_id,
                    hook.htlc.cltv_expiry_relative,
                    cltv_final
                );
            }

            if !is_final_hop {
                log::warn!(
                    "NOT resolving HTLC from {}: INVALID FINAL HOP ONION (item_type={:?}, \
                    short_channel_id={:?}, next_node_id={:?}). payment_hash: {payment_hash}",
                    peer.peer_id,
                    hook.onion.item_type,
                    hook.onion.short_channel_id,
                    hook.onion.next_node_id
                );
            }

            if !matching_onion_amount {
                log::warn!(
                    "NOT resolving HTLC from {}: WRONG ONION AMOUNT (forward_msat={:?}, \
                    total_msat={:?}, EXPECTED={}). payment_hash: {payment_hash}",
                    peer.peer_id,
                    hook.onion.forward_msat,
                    hook.onion.total_msat,
                    pi.amount_msat
                );
            }

            if !matching_payment_secret {
                log::warn!(
                    "NOT resolving HTLC from {}: WRONG OR MISSING PAYMENT SECRET. \
                    payment_hash: {payment_hash}",
                    peer.peer_id
                );
            }

            plugin.state().bad_fwd_nodes.lock().insert(
                peer.peer_id,
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            );

            Ok(json!({"result": "fail", "failure_message": "1007"}))
        } else {
            log::warn!(
                "NOT resolving HTLC on unknown channel {}: matching_scid={matching_scid}, \
                expected_amount={expected_amount}, matching_cltv={matching_cltv}, \
                sufficient_cltv={sufficient_cltv}, is_final_hop={is_final_hop}, \
                matching_onion_amount={matching_onion_amount}, \
                matching_payment_secret={matching_payment_secret}. payment_hash: {payment_hash}",
                hook.htlc.short_channel_id
            );
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
