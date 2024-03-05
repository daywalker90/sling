use cln_rpc::primitives::Amount;
use cln_rpc::primitives::PublicKey;
use cln_rpc::primitives::Secret;
use cln_rpc::primitives::Sha256;
use cln_rpc::primitives::ShortChannelId;
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
                "Unknown variant {} for enum WaitsendpayErrorStatus",
                o
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
    #[serde(alias = "erring_index")]
    pub erring_index: u8,
    #[serde(alias = "erring_node")]
    pub erring_node: PublicKey,
    #[serde(alias = "erring_channel")]
    pub erring_channel: ShortChannelId,
    #[serde(alias = "erring_direction")]
    pub erring_direction: u8,
    #[serde(alias = "failcode")]
    pub failcode: u32,
    #[serde(alias = "failcodename")]
    pub failcodename: String,
}
