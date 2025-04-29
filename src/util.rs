use bitcoin::secp256k1::hashes::Hash;
use bitcoin::secp256k1::hashes::HashEngine;
use cln_rpc::model::responses::ListpeerchannelsChannels;
use cln_rpc::primitives::Amount;
use cln_rpc::primitives::ChannelState;
use cln_rpc::primitives::PublicKey;
use cln_rpc::primitives::Sha256;
use parking_lot::Mutex;
use rand::Rng;
use sling::SatDirection;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::{collections::HashMap, path::Path};

use crate::channel_jobstate_update;
use crate::model::PluginState;
use crate::model::Task;
use crate::model::GRAPH_FILE_NAME;
use crate::model::JOB_FILE_NAME;
use crate::model::PLUGIN_NAME;
use crate::model::{JobMessage, JobState, LnGraph};
use crate::slingstop;
use crate::DirectedChannelState;
use sling::Job;

use crate::tasks::refresh_listpeerchannels;
use anyhow::{anyhow, Error};
use bitcoin::consensus::encode::serialize_hex;
use cln_plugin::Plugin;

use cln_rpc::primitives::ShortChannelId;

use rand::rng;
use tokio::fs::{self, File};

use tokio::time::{self, Instant};

pub async fn refresh_joblists(p: Plugin<PluginState>) -> Result<(), Error> {
    let now = Instant::now();
    refresh_listpeerchannels(&p).await?;
    let jobs = read_jobs(
        &Path::new(&p.configuration().lightning_dir).join(PLUGIN_NAME),
        &p,
    )
    .await?;
    let mut pull_jobs = p.state().pull_jobs.lock();
    let mut push_jobs = p.state().push_jobs.lock();
    pull_jobs.clear();
    push_jobs.clear();

    p.state()
        .job_state
        .lock()
        .retain(|k, _v| jobs.contains_key(k));

    for (chan_id, job) in jobs {
        match job.sat_direction {
            SatDirection::Pull => pull_jobs.insert(chan_id),
            SatDirection::Push => push_jobs.insert(chan_id),
        };
    }
    log::debug!(
        "Read {} pull jobs and {} push jobs in {}ms",
        pull_jobs.len(),
        push_jobs.len(),
        now.elapsed().as_millis(),
    );

    Ok(())
}

pub async fn read_jobs(
    sling_dir: &PathBuf,
    plugin: &Plugin<PluginState>,
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
    let channels = channels.keys().collect::<Vec<&ShortChannelId>>();

    jobs.retain(|c, _j| channels.contains(&c));
    Ok(jobs)
}

pub async fn write_job(
    p: Plugin<PluginState>,
    sling_dir: PathBuf,
    chan_id: ShortChannelId,
    job: Option<Job>,
    remove: bool,
) -> Result<BTreeMap<ShortChannelId, Job>, Error> {
    let mut jobs = read_jobs(&sling_dir, &p).await?;
    let job_change;
    let my_job;
    let jobstates = p.state().job_state.lock().clone();
    if jobstates.contains_key(&chan_id)
        && jobstates
            .get(&chan_id)
            .unwrap()
            .iter()
            .any(|j| j.is_active())
    {
        slingstop(
            p.clone(),
            serde_json::Value::Array(vec![serde_json::Value::String(chan_id.to_string())]),
        )
        .await?;
    }
    {
        let mut job_states = p.state().job_state.lock();
        if jobs.contains_key(&chan_id) {
            if remove {
                jobs.remove(&chan_id);
                job_states.remove(&chan_id);
                job_change = "Removing";
            } else {
                job_states.remove(&chan_id);
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
        log::info!(
            "{} job for {} with amount: {}msat, maxppm: {}, outppm: {:?}, target: {:?},\
            maxhops: {:?}, candidatelist: {:?},\
            depleteuptopercent: {:?}, depleteuptoamount: {:?}, paralleljobs: {:?}",
            job_change,
            &chan_id,
            &my_job.amount_msat,
            &my_job.maxppm,
            &my_job.outppm,
            &my_job.target,
            &my_job.maxhops,
            &my_job.candidatelist,
            &my_job.depleteuptopercent,
            &my_job.depleteuptoamount,
            &my_job.paralleljobs,
        );
        jobs.insert(chan_id, my_job);
    }
    let peer_channels = p.state().peer_channels.lock().clone();
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
    refresh_joblists(p.clone()).await?;
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

pub async fn read_graph(sling_dir: &PathBuf) -> Result<LnGraph, Error> {
    let graphfile = sling_dir.join(GRAPH_FILE_NAME);
    let graphfilecontent = fs::read_to_string(graphfile.clone()).await;
    let graph: LnGraph;

    create_sling_dir(sling_dir).await?;
    match graphfilecontent {
        Ok(file) => {
            graph = match serde_json::from_str(&file) {
                Ok(o) => o,
                Err(e) => {
                    log::warn!("could not read graph: {}", e);
                    LnGraph::new()
                }
            }
        }
        Err(e) => {
            log::warn!(
                "Could not open {}: {}. First time using sling? Creating new file.",
                graphfile.to_str().unwrap(),
                e
            );
            File::create(graphfile.clone()).await?;
            graph = LnGraph::new();
        }
    };

    Ok(graph)
}
pub async fn write_graph(plugin: Plugin<PluginState>) -> Result<(), Error> {
    let graph_string = serde_json::to_string(&*plugin.state().graph.lock())?;
    let sling_dir = Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME);
    let now = Instant::now();
    fs::write(sling_dir.join(GRAPH_FILE_NAME), graph_string).await?;
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

pub fn edge_cost(edge: &DirectedChannelState, amount: u64) -> u64 {
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

    if let Some(private) = channel.private {
        if private {
            let aliases = channel
                .alias
                .as_ref()
                .ok_or_else(|| anyhow!("Missing aliases for private channel"))?;
            if aliases.local.is_none() {
                return Err(anyhow!("Missing local channel alias for private channel"));
            }
            if aliases.remote.is_none() {
                return Err(anyhow!("Missing remote alias for private channel"));
            }
            Ok(())
        } else {
            Ok(())
        }
    } else {
        Ok(())
    }
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

pub async fn my_sleep(
    seconds: u64,
    job_state: Arc<Mutex<HashMap<ShortChannelId, Vec<JobState>>>>,
    task: &Task,
) {
    log::debug!(
        "{}/{}: Starting sleeper for {}s",
        task.chan_id,
        task.task_id,
        seconds
    );
    let timer = Instant::now();
    while timer.elapsed() < Duration::from_secs(seconds) {
        {
            let job_state_lock = job_state.lock();
            let job_states = job_state_lock.get(&task.chan_id);
            if let Some(js) = job_states {
                if let Some(job_state) = js.iter().find(|jt| jt.id() == task.task_id) {
                    if job_state.should_stop() {
                        break;
                    }
                } else {
                    log::warn!(
                        "{}/{}: my_sleep: task id not found",
                        task.chan_id,
                        task.task_id
                    );
                    break;
                }
            } else {
                log::warn!(
                    "{}/{}: my_sleep: scid not found",
                    task.chan_id,
                    task.task_id
                );
                break;
            }
        }
        time::sleep(Duration::from_secs(1)).await;
    }
}

pub async fn wait_for_gossip(plugin: &Plugin<PluginState>, task: &Task) -> Result<(), Error> {
    loop {
        {
            let graph = plugin.state().graph.lock();

            if graph.graph.is_empty() {
                log::info!(
                    "{}/{}: graph is still empty. Sleeping...",
                    task.chan_id,
                    task.task_id
                );
                channel_jobstate_update(
                    plugin.state().job_state.clone(),
                    task,
                    &JobMessage::GraphEmpty,
                    true,
                    false,
                )?;
            } else {
                break;
            }
        }
        my_sleep(600, plugin.state().job_state.clone(), task).await;
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
        let chan_from_peer = match graph.get_channel(&channel.peer_id, &scid) {
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
