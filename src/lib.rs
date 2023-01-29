use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{anyhow, Error};
use cln_plugin::Plugin;
use cln_rpc::{
    model::*,
    primitives::{PublicKey, Secret, ShortChannelId},
    ClnRpc,
};
use log::debug;
use model::{JobState, LnGraph};
use parking_lot::{Mutex, RwLock};
use tokio::{process::Command, time::Instant};

use crate::config::Config;
use crate::errors::*;
use cln_rpc::primitives::*;
pub mod config;
pub mod errors;
pub mod htlc;
pub mod jobs;
pub mod model;
pub mod scored;
pub mod sling;
pub mod stats;
pub mod tasks;
pub mod util;

pub const NO_ALIAS_SET: &str = "NO_ALIAS_SET";
pub const NODE_GOSSIP_MISS: &str = "NODE_GOSSIP_MISS";

pub const PLUGIN_NAME: &str = "sling";
pub const GRAPH_FILE_NAME: &str = "graph.json";
pub const JOB_FILE_NAME: &str = "jobs.json";
pub const EXCEPTS_FILE_NAME: &str = "excepts.json";

#[cfg(test)]
mod tests;

#[derive(Clone)]
pub struct PluginState {
    pub config: Arc<Mutex<Config>>,
    pub peers: Arc<Mutex<Vec<ListpeersPeers>>>,
    pub graph: Arc<Mutex<LnGraph>>,
    pub pays: Arc<RwLock<HashMap<String, String>>>,
    pub alias_peer_map: Arc<Mutex<HashMap<PublicKey, String>>>,
    pub pull_jobs: Arc<Mutex<HashSet<String>>>,
    pub push_jobs: Arc<Mutex<HashSet<String>>>,
    pub excepts: Arc<Mutex<Vec<ShortChannelId>>>,
    pub tempbans: Arc<Mutex<HashMap<String, u64>>>,
    pub job_state: Arc<Mutex<HashMap<String, JobState>>>,
}
impl PluginState {
    pub fn new() -> PluginState {
        PluginState {
            config: Arc::new(Mutex::new(Config::new())),
            peers: Arc::new(Mutex::new(Vec::new())),
            graph: Arc::new(Mutex::new(LnGraph::new())),
            pays: Arc::new(RwLock::new(HashMap::new())),
            alias_peer_map: Arc::new(Mutex::new(HashMap::new())),
            pull_jobs: Arc::new(Mutex::new(HashSet::new())),
            push_jobs: Arc::new(Mutex::new(HashSet::new())),
            excepts: Arc::new(Mutex::new(Vec::new())),
            tempbans: Arc::new(Mutex::new(HashMap::new())),
            job_state: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

pub async fn list_funds(rpc_path: &PathBuf) -> Result<ListfundsResponse, Error> {
    let mut rpc = ClnRpc::new(&rpc_path).await?;
    let listfunds_request = rpc
        .call(Request::ListFunds(ListfundsRequest { spent: Some(false) }))
        .await
        .map_err(|e| anyhow!("Error calling list_funds: {:?}", e))?;
    match listfunds_request {
        Response::ListFunds(info) => Ok(info),
        e => Err(anyhow!("Unexpected result in list_funds: {:?}", e)),
    }
}

pub async fn list_peers(rpc_path: &PathBuf) -> Result<ListpeersResponse, Error> {
    let now = Instant::now();
    let mut rpc = ClnRpc::new(&rpc_path).await?;
    let listpeers_request = rpc
        .call(Request::ListPeers(ListpeersRequest {
            id: None,
            level: None,
        }))
        .await
        .map_err(|e| anyhow!("Error calling list_peers: {:?}", e))?;
    debug!("Listpeers:{:.3?}", now.elapsed());
    match listpeers_request {
        Response::ListPeers(info) => Ok(info),
        e => Err(anyhow!("Unexpected result in list_peers: {:?}", e)),
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
    debug!("Listchannels:{}ms", now.elapsed().as_millis().to_string());
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

pub async fn list_forwards(
    rpc_path: &PathBuf,
    status: Option<ListforwardsStatus>,
    in_channel: Option<ShortChannelId>,
    out_channel: Option<ShortChannelId>,
) -> Result<ListforwardsResponse, Error> {
    let mut rpc = ClnRpc::new(&rpc_path).await?;
    let listforwards_request = rpc
        .call(Request::ListForwards(ListforwardsRequest {
            status,
            in_channel,
            out_channel,
        }))
        .await
        .map_err(|e| anyhow!("Error calling list_forwards: {:?}", e))?;
    match listforwards_request {
        Response::ListForwards(info) => Ok(info),
        e => Err(anyhow!("Unexpected result in list_forwards: {:?}", e)),
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

pub async fn delpay(
    lightning_dir: &PathBuf,
    payment_hash: Sha256,
    status: &str,
) -> Result<DelpayResponse, Error> {
    let delpay_request = Command::new("/usr/local/bin/lightning-cli")
        .arg("--lightning-dir=".to_string() + lightning_dir.to_str().unwrap())
        .arg("delpay")
        .arg((payment_hash).to_string())
        .arg(status)
        .output()
        .await;
    match delpay_request {
        Ok(output) => match serde_json::from_str(&String::from_utf8_lossy(&output.stdout)) {
            Ok(o) => Ok(o),
            Err(e) => Err(anyhow!(
                "Unexpected error in parsing delpay response: {} {:?}",
                e,
                output
            )),
        },
        Err(e) => Err(anyhow!("Unexpected error in delpay: {}", e)),
    }
}

pub async fn waitsendpay(
    lightning_dir: &PathBuf,
    payment_hash: Sha256,
    _timeout: Option<u32>,
    _partid: Option<u64>,
) -> Result<WaitsendpayResponse, WaitsendpayError> {
    // debug!("{}", lightning_dir.to_str().unwrap());
    let waitsendpay_request = Command::new("/usr/local/bin/lightning-cli")
        .arg("--lightning-dir=".to_string() + lightning_dir.to_str().unwrap())
        .arg("waitsendpay")
        .arg((payment_hash).to_string())
        .arg("120")
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

pub fn make_rpc_path(plugin: &Plugin<PluginState>) -> PathBuf {
    Path::new(&plugin.configuration().lightning_dir).join(plugin.configuration().rpc_file)
}

pub fn is_channel_normal(channel: &ListpeersPeersChannels) -> bool {
    match channel.state {
        ListpeersPeersChannelsState::CHANNELD_NORMAL => true,
        _ => false,
    }
}

pub fn get_normal_channel_from_listpeers(
    peers: &Vec<ListpeersPeers>,
    chan_id: &ShortChannelId,
) -> Option<ListpeersPeersChannels> {
    peers
        .iter()
        .flat_map(|peer| &peer.channels)
        .find(|channel| channel.short_channel_id == Some(*chan_id) && is_channel_normal(channel))
        .cloned()
}
pub fn get_all_normal_channels_from_listpeers(
    peers: &Vec<ListpeersPeers>,
) -> HashMap<String, PublicKey> {
    let mut scid_peer_map = HashMap::new();
    for peer in peers {
        for channel in &peer.channels {
            if is_channel_normal(channel) {
                scid_peer_map.insert(
                    channel.short_channel_id.unwrap().to_string(),
                    peer.id.clone(),
                );
            }
        }
    }
    scid_peer_map
}
