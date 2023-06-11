use std::{path::PathBuf, str::FromStr};

use anyhow::{anyhow, Error};
use cln_plugin::ConfiguredPlugin;
use cln_rpc::{
    model::*,
    primitives::{PublicKey, Secret, ShortChannelId},
    ClnRpc,
};
use log::debug;
use serde_json::Value;
use tokio::{process::Command, time::Instant};

use crate::{
    errors::*,
    model::{PluginState, PLUGIN_NAME},
};
use cln_rpc::primitives::*;

pub async fn set_channel(
    rpc_path: &PathBuf,
    id: String,
    feebase: Option<Amount>,
    feeppm: Option<u32>,
    htlcmin: Option<Amount>,
    htlcmax: Option<Amount>,
    enforcedelay: Option<u32>,
) -> Result<SetchannelResponse, Error> {
    let mut rpc = ClnRpc::new(&rpc_path).await?;
    let set_channel_request = rpc
        .call(Request::SetChannel(SetchannelRequest {
            id,
            feebase,
            feeppm,
            htlcmin,
            htlcmax,
            enforcedelay,
        }))
        .await
        .map_err(|e| anyhow!("Error calling set_channel: {:?}", e))?;
    match set_channel_request {
        Response::SetChannel(info) => Ok(info),
        e => Err(anyhow!("Unexpected result in set_channel: {:?}", e)),
    }
}

pub async fn disconnect(rpc_path: &PathBuf, id: PublicKey) -> Result<DisconnectResponse, Error> {
    let mut rpc = ClnRpc::new(&rpc_path).await?;
    let disconnect_request = rpc
        .call(Request::Disconnect(DisconnectRequest {
            id,
            force: Some(true),
        }))
        .await
        .map_err(|e| anyhow!("Error calling disconnect: {:?}", e))?;
    match disconnect_request {
        Response::Disconnect(info) => Ok(info),
        e => Err(anyhow!("Unexpected result in disconnect: {:?}", e)),
    }
}

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
    route: Vec<SendpayRoute>,
    payment_hash: Sha256,
    payment_secret: Option<Secret>,
    label: Option<String>,
) -> Result<SendpayResponse, Error> {
    let mut rpc = ClnRpc::new(&rpc_path).await?;
    let sendpay_request = rpc
        .call(Request::SendPay(SendpayRequest {
            route,
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

pub async fn waitsendpay(
    lightning_dir: &PathBuf,
    lightning_cli: &String,
    payment_hash: Sha256,
    timeout: u16,
) -> Result<WaitsendpayResponse, WaitsendpayError> {
    // debug!("{}", lightning_dir.to_str().unwrap());
    let waitsendpay_request = Command::new(lightning_cli)
        .arg("--lightning-dir=".to_string() + lightning_dir.to_str().unwrap())
        .arg("waitsendpay")
        .arg((payment_hash).to_string())
        .arg(timeout.to_string())
        .output()
        .await;
    match waitsendpay_request {
        Ok(output) => match serde_json::from_str(&String::from_utf8_lossy(&output.stdout)) {
            Ok(o) => Ok(o),
            Err(_) => match serde_json::from_str::<WaitsendpayError>(&String::from_utf8_lossy(
                &output.stdout,
            )) {
                Ok(err) => Err(err),
                Err(e) => Err(WaitsendpayError {
                    code: None,
                    message: format!("{} {:?}", e.to_string(), output),
                    data: None,
                }),
            },
        },
        Err(e) => Err(WaitsendpayError {
            code: None,
            message: e.to_string(),
            data: None,
        }),
    }
}

pub async fn get_config_path(
    network_dir: &String,
    lightning_cli: String,
) -> Result<Option<String>, Error> {
    let network_dir = PathBuf::from_str(&network_dir).unwrap();
    let mut lightning_dir = network_dir.clone();
    lightning_dir.pop();
    debug!("{}  |  {}", lightning_dir.to_str().unwrap(), lightning_cli);
    let listconfigs_request = Command::new(lightning_cli)
        .arg("--lightning-dir=".to_string() + lightning_dir.to_str().unwrap())
        .arg("listconfigs")
        .output()
        .await;
    match listconfigs_request {
        Ok(output) => match serde_json::from_str::<Value>(&String::from_utf8_lossy(&output.stdout))
        {
            Ok(o) => match o.get("conf") {
                Some(c) => Ok(Some(c.as_str().unwrap().to_string())),
                None => Ok(None),
            },
            Err(e) => Err(anyhow!(
                "Could not read listconfigs response: {}",
                e.to_string()
            )),
        },
        Err(e) => Err(anyhow!(
            "Unexpected result in listconfigs: {}",
            e.to_string()
        )),
    }
}

pub async fn check_lightning_dir(
    plugin: &ConfiguredPlugin<PluginState, tokio::io::Stdin, tokio::io::Stdout>,
    state: PluginState,
) -> Result<(), Error> {
    let network_dir = PathBuf::from_str(&plugin.configuration().lightning_dir.clone()).unwrap();
    let mut lightning_dir = network_dir.clone();
    lightning_dir.pop();
    let config = state.config.lock();
    let getinfo_request = Command::new(config.lightning_cli.1.clone())
        .arg("--lightning-dir=".to_string() + lightning_dir.to_str().unwrap())
        .arg("getinfo")
        .output()
        .await;
    match getinfo_request {
        Ok(output) => {
            match serde_json::from_str::<GetinfoResponse>(&String::from_utf8_lossy(&output.stdout))
            {
                Ok(_o) => Ok(()),
                Err(e) => Err(anyhow!(
                    "Unexpected error in parsing GetinfoResponse: {} {:?}",
                    e,
                    output
                )),
            }
        }
        Err(e) => Err(anyhow!(
            "Your {}-lightning-cli is probably wrong!: {}",
            PLUGIN_NAME.to_string(),
            e
        )),
    }
}
