use std::path::PathBuf;

use anyhow::{anyhow, Error};
use cln_rpc::{
    model::{requests::*, responses::*, *},
    primitives::{PublicKey, Secret, ShortChannelId},
    ClnRpc,
};
use log::debug;
use tokio::time::Instant;

use cln_rpc::primitives::*;

pub async fn list_peer_channels(rpc_path: &PathBuf) -> Result<ListpeerchannelsResponse, Error> {
    let mut rpc = ClnRpc::new(&rpc_path).await?;
    let list_peer_channels = rpc
        .call(Request::ListPeerChannels(ListpeerchannelsRequest {
            id: None,
        }))
        .await
        .map_err(|e| anyhow!("Error calling list_peer_channels: {}", e.to_string()))?;
    match list_peer_channels {
        Response::ListPeerChannels(info) => Ok(info),
        e => Err(anyhow!("Unexpected result in list_peer_channels: {:?}", e)),
    }
}

pub async fn list_nodes(
    rpc_path: &PathBuf,
    peer: Option<PublicKey>,
) -> Result<ListnodesResponse, Error> {
    let mut rpc = ClnRpc::new(&rpc_path).await?;
    let listnodes_request = rpc
        .call(Request::ListNodes(ListnodesRequest { id: peer }))
        .await
        .map_err(|e| anyhow!("Error calling list_nodes: {:?}", e))?;
    match listnodes_request {
        Response::ListNodes(info) => Ok(info),
        e => Err(anyhow!("Unexpected result in list_nodes: {:?}", e)),
    }
}

pub async fn list_channels(
    rpc_path: &PathBuf,
    short_channel_id: Option<ShortChannelId>,
    source: Option<PublicKey>,
    destination: Option<PublicKey>,
) -> Result<ListchannelsResponse, Error> {
    let now = Instant::now();
    let mut rpc = ClnRpc::new(&rpc_path).await?;
    let listchannels_request = rpc
        .call(Request::ListChannels(ListchannelsRequest {
            short_channel_id,
            source,
            destination,
        }))
        .await
        .map_err(|e| anyhow!("Error calling list_channels: {:?}", e))?;
    if short_channel_id.is_none() && source.is_none() && destination.is_none() {
        debug!("Listchannels:{}ms", now.elapsed().as_millis().to_string());
    }
    match listchannels_request {
        Response::ListChannels(info) => Ok(info),
        e => Err(anyhow!("Unexpected result in list_channels: {:?}", e)),
    }
}

pub async fn get_info(rpc_path: &PathBuf) -> Result<GetinfoResponse, Error> {
    let mut rpc = ClnRpc::new(&rpc_path).await?;
    let getinfo_request = rpc
        .call(Request::Getinfo(GetinfoRequest {}))
        .await
        .map_err(|e| anyhow!("Error calling get_info: {:?}", e))?;
    match getinfo_request {
        Response::Getinfo(info) => Ok(info),
        e => Err(anyhow!("Unexpected result in get_info: {:?}", e)),
    }
}

pub async fn slingsend(
    rpc_path: &PathBuf,
    route: &[SendpayRoute],
    payment_hash: Sha256,
    payment_secret: Option<Secret>,
    label: Option<String>,
) -> Result<SendpayResponse, Error> {
    let mut rpc = ClnRpc::new(&rpc_path).await?;
    let sendpay_request = rpc
        .call(Request::SendPay(SendpayRequest {
            route: route.to_vec(),
            payment_hash,
            label,
            amount_msat: None,
            bolt11: None,
            payment_secret,
            partid: None,
            localinvreqid: None,
            groupid: None,
        }))
        .await
        .map_err(|e| anyhow!("Error calling sendpay: {:?}", e))?;
    match sendpay_request {
        Response::SendPay(info) => Ok(info),
        e => Err(anyhow!("Unexpected result in sendpay: {:?}", e)),
    }
}

pub async fn waitsendpay2(
    rpc_path: &PathBuf,
    payment_hash: Sha256,
    timeout: u16,
) -> Result<WaitsendpayResponse, Error> {
    let mut rpc = ClnRpc::new(&rpc_path).await?;
    let sendpay_request = rpc
        .call(Request::WaitSendPay(WaitsendpayRequest {
            payment_hash,
            timeout: Some(timeout as u32),
            partid: None,
            groupid: None,
        }))
        .await?;
    match sendpay_request {
        Response::WaitSendPay(info) => Ok(info),
        e => Err(anyhow!("Unexpected result in waitsendpay2: {:?}", e)),
    }
}
