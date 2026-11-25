use cln_rpc::primitives::{Amount, PublicKey, Secret, Sha256, ShortChannelId};
use serde::{Deserialize, Serialize};

// status of the payment
#[derive(Copy, Clone, Debug, Deserialize, Serialize)]
pub enum WaitsendpayErrorStatus {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "failed")]
    Failed,
}

impl TryFrom<i32> for WaitsendpayErrorStatus {
    type Error = anyhow::Error;
    fn try_from(c: i32) -> Result<WaitsendpayErrorStatus, anyhow::Error> {
        match c {
            1 => Ok(WaitsendpayErrorStatus::Pending),
            -1 => Ok(WaitsendpayErrorStatus::Failed),
            o => Err(anyhow::anyhow!(
                "Unknown variant {o} for enum WaitsendpayErrorStatus"
            )),
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WaitsendpayErrorData {
    #[serde(alias = "id")]
    pub id: u64,
    #[serde(alias = "groupid", skip_serializing_if = "Option::is_none")]
    pub groupid: Option<u64>,
    #[serde(alias = "payment_hash")]
    pub payment_hash: Sha256,
    // Path `WaitSendPay.status`
    #[serde(rename = "status")]
    pub status: WaitsendpayErrorStatus,
    #[serde(alias = "amount_msat", skip_serializing_if = "Option::is_none")]
    pub amount_msat: Option<Amount>,
    #[serde(alias = "destination", skip_serializing_if = "Option::is_none")]
    pub destination: Option<PublicKey>,
    #[serde(alias = "created_at")]
    pub created_at: u64,
    #[serde(alias = "completed_at", skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<f64>,
    #[serde(alias = "amount_sent_msat")]
    pub amount_sent_msat: Amount,
    #[serde(alias = "label", skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(alias = "partid", skip_serializing_if = "Option::is_none")]
    pub partid: Option<u64>,
    #[serde(alias = "bolt11", skip_serializing_if = "Option::is_none")]
    pub bolt11: Option<String>,
    #[serde(alias = "bolt12", skip_serializing_if = "Option::is_none")]
    pub bolt12: Option<String>,
    #[serde(alias = "payment_preimage", skip_serializing_if = "Option::is_none")]
    pub payment_preimage: Option<Secret>,
    #[serde(alias = "erring_index", skip_serializing_if = "Option::is_none")]
    pub erring_index: Option<u8>,
    #[serde(alias = "erring_node", skip_serializing_if = "Option::is_none")]
    pub erring_node: Option<PublicKey>,
    #[serde(alias = "erring_channel", skip_serializing_if = "Option::is_none")]
    pub erring_channel: Option<ShortChannelId>,
    #[serde(alias = "erring_direction", skip_serializing_if = "Option::is_none")]
    pub erring_direction: Option<u8>,
    #[serde(alias = "failcode", skip_serializing_if = "Option::is_none")]
    pub failcode: Option<u32>,
    #[serde(alias = "failcodename", skip_serializing_if = "Option::is_none")]
    pub failcodename: Option<String>,
}
