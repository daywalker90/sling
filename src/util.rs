use bitcoin::secp256k1::hashes::Hash;
use bitcoin::secp256k1::hashes::HashEngine;
use cln_rpc::model::responses::ListpeerchannelsChannels;
use cln_rpc::primitives::Amount;
use cln_rpc::primitives::ChannelState;
use cln_rpc::primitives::PublicKey;
use cln_rpc::primitives::Sha256;
use cln_rpc::primitives::ShortChannelIdDir;
use rand::Rng;
use sling::SatDirection;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::io;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;
use std::{collections::HashMap, path::Path};

use crate::model::Liquidity;
use crate::model::PluginState;
use crate::model::PubKeyBytes;
use crate::model::TaskIdentifier;
use crate::model::EXCEPTS_CHANS_FILE_NAME;
use crate::model::EXCEPTS_PEERS_FILE_NAME;
use crate::model::JOB_FILE_NAME;
use crate::model::LIQUIDITY_FILE_NAME;
use crate::model::PLUGIN_NAME;
use crate::model::{JobMessage, LnGraph};
use crate::ShortChannelIdDirState;
use sling::Job;

use anyhow::{anyhow, Error};
use bitcoin::consensus::encode::serialize_hex;
use cln_plugin::Plugin;

use cln_rpc::primitives::ShortChannelId;

use rand::rng;
use tokio::fs::{self, File};

use tokio::time::{self, Instant};

pub async fn read_jobs(
    sling_dir: &PathBuf,
    plugin: Plugin<PluginState>,
) -> Result<BTreeMap<ShortChannelId, Job>, Error> {
    let jobfile = sling_dir.join(JOB_FILE_NAME);
    let jobfilecontent = fs::read_to_string(jobfile.clone()).await;
    let mut jobs: BTreeMap<ShortChannelId, Job>;

    create_sling_dir(sling_dir).await?;
    match jobfilecontent {
        Ok(file) => jobs = serde_json::from_str(&file).unwrap_or(BTreeMap::new()),
        Err(e) => {
            log::warn!(
                "Couldn't open {}: {}. First time using sling? Creating new file.",
                jobfile.to_str().unwrap(),
                e
            );
            File::create(jobfile.clone()).await?;
            jobs = BTreeMap::new();
        }
    };
    let peer_channels = plugin.state().peer_channels.lock();
    let channels = get_all_normal_channels_from_listpeerchannels(&peer_channels);

    jobs.retain(|c, _j| channels.contains_key(c));
    let mut config = plugin.state().config.lock();
    for (scid, job) in jobs.iter() {
        match job.sat_direction {
            SatDirection::Pull => config.exclude_chans_pull.insert(*scid),
            SatDirection::Push => config.exclude_chans_push.insert(*scid),
        };
    }
    Ok(jobs)
}

pub async fn write_job(
    plugin: Plugin<PluginState>,
    sling_dir: PathBuf,
    chan_id: ShortChannelId,
    job: Option<Job>,
    remove: bool,
) -> Result<BTreeMap<ShortChannelId, Job>, Error> {
    let mut jobs = read_jobs(&sling_dir, plugin.clone()).await?;
    let job_change;
    let my_job;
    {
        if jobs.contains_key(&chan_id) {
            if remove {
                jobs.remove(&chan_id);
                job_change = "Removing";
            } else {
                job_change = "Updating";
            }
        } else {
            job_change = "Creating";
        }
    }
    if remove {
        log::info!("{} job for {}", job_change, &chan_id);
    } else {
        my_job = job.unwrap();
        log::info!("{} job for {}:", job_change, &chan_id);
        log::info!("{}", my_job);
        jobs.insert(chan_id, my_job);
    }
    let peer_channels = plugin.state().peer_channels.lock().clone();
    let mut jobs_to_remove = HashSet::new();
    if !peer_channels.is_empty() {
        for chan_id in jobs.keys() {
            if get_normal_channel_from_listpeerchannels(&peer_channels, chan_id).is_err() {
                jobs_to_remove.insert(*chan_id);
            }
        }
    }
    jobs.retain(|i, _j| !jobs_to_remove.contains(i));
    fs::write(
        sling_dir.join(JOB_FILE_NAME),
        serde_json::to_string_pretty(&jobs)?,
    )
    .await?;
    Ok(jobs)
}

pub async fn write_excepts<T: ToString>(
    excepts: HashSet<T>,
    file: &str,
    sling_dir: &Path,
) -> Result<(), Error> {
    let excepts_tostring = excepts
        .into_iter()
        .map(|x| x.to_string())
        .collect::<Vec<_>>();

    fs::write(
        sling_dir.join(file),
        serde_json::to_string(&excepts_tostring)?,
    )
    .await?;

    Ok(())
}

pub async fn read_liquidity(
    sling_dir: &PathBuf,
) -> Result<HashMap<ShortChannelIdDir, Liquidity>, Error> {
    let liquidity_file = sling_dir.join(LIQUIDITY_FILE_NAME);
    let liquidity_file_content = fs::read_to_string(liquidity_file.clone()).await;
    let liquidity: HashMap<ShortChannelIdDir, Liquidity>;

    create_sling_dir(sling_dir).await?;
    match liquidity_file_content {
        Ok(file) => {
            liquidity = match serde_json::from_str(&file) {
                Ok(o) => o,
                Err(e) => {
                    log::warn!("could not read liquidity: {}", e);
                    HashMap::new()
                }
            }
        }
        Err(e) => {
            log::warn!(
                "Could not open {}: {}. First time using sling? Creating new file.",
                liquidity_file.to_str().unwrap(),
                e
            );
            File::create(liquidity_file.clone()).await?;
            liquidity = HashMap::new();
        }
    };

    Ok(liquidity)
}
pub async fn write_liquidity(plugin: Plugin<PluginState>) -> Result<(), Error> {
    let graph_string = serde_json::to_string(&*plugin.state().liquidity.lock())?;
    let sling_dir = Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME);
    let now = Instant::now();
    fs::write(sling_dir.join(LIQUIDITY_FILE_NAME), graph_string).await?;
    log::debug!("Wrote graph to disk in {}ms", now.elapsed().as_millis());
    Ok(())
}

pub async fn create_sling_dir(sling_dir: &PathBuf) -> Result<(), Error> {
    match fs::create_dir(sling_dir).await {
        Ok(_) => Ok(()),
        Err(e) => match e.kind() {
            io::ErrorKind::AlreadyExists => Ok(()),
            _ => Err(anyhow!("error: {}, could not create sling folder", e)),
        },
    }
}

pub fn get_preimage_paymend_hash_pair() -> (String, Sha256) {
    let mut preimage = [0u8; 32];
    rng().fill(&mut preimage[..]);

    let pi_str = serialize_hex(&preimage);

    let mut hasher = Sha256::engine();
    hasher.input(&preimage);
    let payment_hash = Sha256::from_engine(hasher);
    (pi_str, payment_hash)
}

pub fn get_total_htlc_count(channel: &ListpeerchannelsChannels) -> u64 {
    match &channel.htlcs {
        Some(htlcs) => htlcs.len() as u64,
        None => 0,
    }
}

pub fn edge_cost(edge: &ShortChannelIdDirState, amount: u64) -> u64 {
    // debug!(
    //     "edge cost for {} source:{} is {}",
    //     edge.short_channel_id.to_string(),
    //     edge.source,
    //     (edge.base_fee_millisatoshi as f64
    //         + edge.fee_per_millionth as f64 / 1_000_000.0 * amount as f64) as u64
    // );
    std::cmp::max(
        fee_total_msat_precise(edge.fee_per_millionth, edge.base_fee_millisatoshi, amount).ceil()
            as u64,
        1,
    )
}

pub fn feeppm_effective(feeppm: u32, basefee_msat: u32, amount_msat: u64) -> u64 {
    (fee_total_msat_precise(feeppm, basefee_msat, amount_msat) / amount_msat as f64 * 1_000_000.0)
        .ceil() as u64
}

pub fn fee_total_msat_precise(feeppm: u32, basefee_msat: u32, amount_msat: u64) -> f64 {
    basefee_msat as f64 + (feeppm as f64 / 1_000_000.0 * amount_msat as f64)
}

pub fn feeppm_effective_from_amts(amount_msat_start: u64, amount_msat_end: u64) -> u32 {
    if amount_msat_start < amount_msat_end {
        panic!(
            "CRITICAL ERROR: amount_msat_start should be greater than or equal to amount_msat_end"
        )
    }
    ((amount_msat_start - amount_msat_end) as f64 / amount_msat_end as f64 * 1_000_000.0).ceil()
        as u32
}

pub fn is_channel_normal(channel: &ListpeerchannelsChannels) -> Result<(), Error> {
    if !matches!(
        channel.state,
        ChannelState::CHANNELD_NORMAL | ChannelState::CHANNELD_AWAITING_SPLICE
    ) {
        return Err(anyhow!(
            "Not in CHANNELD_NORMAL or CHANNELD_AWAITING_SPLICE state!"
        ));
    }

    Ok(())
}

pub fn get_normal_channel_from_listpeerchannels(
    peer_channels: &HashMap<ShortChannelId, ListpeerchannelsChannels>,
    chan_id: &ShortChannelId,
) -> Result<ListpeerchannelsChannels, Error> {
    if let Some(chan) = peer_channels.get(chan_id) {
        match is_channel_normal(chan) {
            Ok(_) => Ok(chan.clone()),
            Err(e) => Err(e),
        }
    } else {
        Err(anyhow!("Channel not found"))
    }
}

pub fn get_all_normal_channels_from_listpeerchannels(
    peer_channels: &HashMap<ShortChannelId, ListpeerchannelsChannels>,
) -> HashMap<ShortChannelId, PublicKey> {
    let mut scid_peer_map = HashMap::new();
    for channel in peer_channels.values() {
        if is_channel_normal(channel).is_ok() {
            scid_peer_map.insert(channel.short_channel_id.unwrap(), channel.peer_id);
        }
    }
    scid_peer_map
}

pub async fn my_sleep(plugin: Plugin<PluginState>, seconds: u64, task_ident: &TaskIdentifier) {
    log::debug!("{}: Starting sleeper for {}s", task_ident, seconds);
    let timer = Instant::now();
    while timer.elapsed() < Duration::from_secs(seconds) {
        {
            if let Some(o) = plugin.state().tasks.lock().get_task(task_ident) {
                if o.should_stop() {
                    break;
                }
            } else {
                break;
            };
        }
        time::sleep(Duration::from_secs(1)).await;
    }
}

pub async fn wait_for_gossip(
    plugin: Plugin<PluginState>,
    task_ident: &TaskIdentifier,
) -> Result<(), Error> {
    loop {
        {
            let graph = plugin.state().graph.lock();

            if graph.is_empty() {
                let mut tasks = plugin.state().tasks.lock();
                let task = tasks
                    .get_task_mut(task_ident)
                    .ok_or_else(|| anyhow!("Task not found"))?;
                log::info!("{}: graph is still empty. Sleeping...", task);
                task.set_state(JobMessage::GraphEmpty);
            } else {
                break;
            }
        }
        my_sleep(plugin.clone(), 600, task_ident).await;
    }
    Ok(())
}

pub fn get_remote_feeppm_effective(
    channel: &ListpeerchannelsChannels,
    graph: &LnGraph,
    scid: ShortChannelId,
    amount_msat: u64,
    version: &str,
) -> Result<u64, Error> {
    if at_or_above_version(version, "24.02")? {
        let chan_updates = if let Some(updates) = &channel.updates {
            if let Some(remote) = &updates.remote {
                remote
            } else {
                return Err(anyhow!("No remote gossip in listpeerchannels"));
            }
        } else {
            return Err(anyhow!("No gossip in listpeerchannels"));
        };
        let chan_in_ppm = feeppm_effective(
            chan_updates.fee_proportional_millionths,
            Amount::msat(&chan_updates.fee_base_msat) as u32,
            amount_msat,
        );
        Ok(chan_in_ppm)
    } else {
        let (_scid_dir, chan_from_peer) = match graph
            .get_state_no_direction(&PubKeyBytes::from_pubkey(&channel.peer_id), &scid)
        {
            Ok(chan) => chan,
            Err(_) => return Err(anyhow!("No gossip for {} in graph", scid)),
        };
        let chan_in_ppm = feeppm_effective(
            chan_from_peer.fee_per_millionth,
            chan_from_peer.base_fee_millisatoshi,
            amount_msat,
        );
        Ok(chan_in_ppm)
    }
}

pub fn get_direction_from_nodes(
    source: PublicKey,
    destination: PublicKey,
) -> Result<u32, anyhow::Error> {
    if source < destination {
        return Ok(0);
    }
    if source > destination {
        return Ok(1);
    }
    Err(anyhow!("Nodes are equal"))
}

pub async fn read_except_chans(sling_dir: &PathBuf) -> Result<HashSet<ShortChannelId>, Error> {
    let excepts_chan_file = sling_dir.join(EXCEPTS_CHANS_FILE_NAME);
    let excepts_chan_file_content = fs::read_to_string(excepts_chan_file.clone()).await;

    create_sling_dir(sling_dir).await?;

    parse_excepts(excepts_chan_file_content, excepts_chan_file).await
}
pub async fn read_except_peers(sling_dir: &PathBuf) -> Result<HashSet<PublicKey>, Error> {
    let excepts_peers_file = sling_dir.join(EXCEPTS_PEERS_FILE_NAME);
    let excepts_peers_file_content = fs::read_to_string(excepts_peers_file.clone()).await;

    create_sling_dir(sling_dir).await?;

    parse_excepts(excepts_peers_file_content, excepts_peers_file).await
}
async fn parse_excepts<T: FromStr + std::hash::Hash + Eq>(
    content: Result<String, io::Error>,
    excepts_file: PathBuf,
) -> Result<HashSet<T>, Error> {
    let excepts_tostring: Vec<String>;
    let mut excepts: HashSet<T> = HashSet::new();

    match content {
        Ok(file) => excepts_tostring = serde_json::from_str(&file).unwrap_or(Vec::new()),
        Err(e) => match e.kind() {
            io::ErrorKind::NotFound => {
                log::info!("{} not found. Creating...", excepts_file.display());
                File::create(excepts_file.clone()).await?;
                excepts_tostring = Vec::new();
            }
            _ => {
                log::warn!("Could not open {}: {}.", excepts_file.to_str().unwrap(), e);
                return Err(anyhow!(
                    "Could not open {}: {}.",
                    excepts_file.to_str().unwrap(),
                    e
                ));
            }
        },
    };

    for except in excepts_tostring {
        match T::from_str(&except) {
            Ok(id) => {
                excepts.insert(id);
            }
            Err(_e) => log::warn!(
                "excepts file contains invalid short_channel_id/node_id: {}",
                except
            ),
        }
    }
    Ok(excepts)
}

pub fn at_or_above_version(my_version: &str, min_version: &str) -> Result<bool, Error> {
    let clean_start_my_version = my_version
        .split_once('v')
        .ok_or_else(|| anyhow!("Could not find v in version string"))?
        .1;
    let full_clean_my_version: String = clean_start_my_version
        .chars()
        .take_while(|x| x.is_ascii_digit() || *x == '.')
        .collect();

    let my_version_parts: Vec<&str> = full_clean_my_version.split('.').collect();
    let min_version_parts: Vec<&str> = min_version.split('.').collect();

    if my_version_parts.len() <= 1 || my_version_parts.len() > 3 {
        return Err(anyhow!("Version string parse error: {}", my_version));
    }
    for (my, min) in my_version_parts.iter().zip(min_version_parts.iter()) {
        let my_num: u32 = my.parse()?;
        let min_num: u32 = min.parse()?;

        if my_num != min_num {
            return Ok(my_num > min_num);
        }
    }

    Ok(my_version_parts.len() >= min_version_parts.len())
}
