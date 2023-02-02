use bitcoin::hashes::Hash;
use bitcoin::hashes::HashEngine;
use cln_rpc::model::ListchannelsChannels;
use cln_rpc::model::ListpeersPeersChannelsState;
use cln_rpc::primitives::PublicKey;
use cln_rpc::primitives::Sha256;
use parking_lot::Mutex;
use rand::Rng;
use std::io;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::{collections::HashMap, path::Path};

use crate::jobs::slingstop;
use crate::model::PluginState;
use crate::model::{Job, JobMessage, JobState, LnGraph, SatDirection};

use crate::{list_peers, EXCEPTS_FILE_NAME, GRAPH_FILE_NAME, JOB_FILE_NAME, PLUGIN_NAME};
use anyhow::{anyhow, Error};
use bitcoin::consensus::encode::serialize_hex;
use cln_plugin::Plugin;

use cln_rpc::model::{
    ListpeersPeers, ListpeersPeersChannels, ListpeersPeersChannelsHtlcsDirection,
};
use cln_rpc::primitives::ShortChannelId;
use log::{debug, info, warn};

use rand::thread_rng;
use serde_json::json;
use tokio::fs::{self, File};

use tokio::time::Instant;

pub async fn slingjob(
    p: Plugin<PluginState>,
    v: serde_json::Value,
) -> Result<serde_json::Value, Error> {
    let sling_dir = Path::new(&p.configuration().lightning_dir).join(PLUGIN_NAME);

    match v {
        serde_json::Value::Array(ar) => {
            if ar.len() < 5 {
                return Err(anyhow!("Missing arguments"));
            }

            let chan_id = ShortChannelId::from_str(
                &ar.get(0)
                    .unwrap()
                    .as_str()
                    .ok_or(anyhow!("invalid string for channel id"))?,
            )?;

            let sat_direction = SatDirection::from_str(
                &ar.get(1)
                    .unwrap()
                    .as_str()
                    .ok_or(anyhow!("invalid string for direction"))?,
            )?;

            //also convert to msat
            let amount = ar
                .get(2)
                .unwrap()
                .as_u64()
                .ok_or(anyhow!("amount must be a positive integer"))?
                * 1_000;
            if amount == 0 {
                return Err(anyhow!("amount must be greater than 0"));
            }

            let maxppm =
                ar.get(3)
                    .unwrap()
                    .as_u64()
                    .ok_or(anyhow!("maxppm must be a positive integer"))? as u32;

            let outppm = match ar.get(4) {
                Some(o) => o.as_u64(),
                None => None,
            };

            let target = match ar.get(5) {
                Some(t) => t.as_f64(),
                None => None,
            };

            let maxhops = match ar.get(6) {
                Some(h) => h.as_u64(),
                None => None,
            };

            let candidatelist = {
                let mut tmpcandidatelist = Vec::new();
                match ar.get(7) {
                    Some(candidates) => {
                        for candidate in candidates
                            .as_array()
                            .ok_or(anyhow!("Invalid array for candidate list"))?
                        {
                            tmpcandidatelist.push(ShortChannelId::from_str(
                                candidate.as_str().ok_or(anyhow!(
                                    "invalid string for channel id in candidate list"
                                ))?,
                            )?);
                        }
                        Some(tmpcandidatelist)
                    }
                    None => None,
                }
            };
            if outppm.is_none() && candidatelist.is_none() {
                return Err(anyhow!(
                    "Atleast one of outppm and candidatelist need to be set"
                ));
            }
            let peers = p.state().peers.lock().clone();
            let job = Job {
                sat_direction,
                amount,
                outppm,
                maxppm,
                candidatelist,
                target,
                maxhops,
            };
            let our_listpeers_channel = get_normal_channel_from_listpeers(&peers, chan_id);
            if let Some(_channel) = our_listpeers_channel {
                write_job(p.clone(), sling_dir, chan_id.to_string(), Some(job), false).await?;
                Ok(json!({"result":"success"}))
            } else {
                Err(anyhow!(
                    "Could not find channel or not in CHANNELD_NORMAL state: {}",
                    chan_id.to_string()
                ))
            }
        }
        other => Err(anyhow!("Invalid arguments: {}", other.to_string())),
    }
}

pub async fn refresh_joblists(p: Plugin<PluginState>) -> Result<(), Error> {
    let now = Instant::now();
    let peers = list_peers(&make_rpc_path(&p.clone())).await?.peers;
    let jobs = read_jobs(
        &Path::new(&p.configuration().lightning_dir).join(PLUGIN_NAME),
        &peers,
    )
    .await?;
    let mut pull_jobs = p.state().pull_jobs.lock();
    let mut push_jobs = p.state().push_jobs.lock();

    for (chan_id, job) in jobs {
        match job.sat_direction {
            SatDirection::Pull => pull_jobs.insert(chan_id.to_string()),
            SatDirection::Push => push_jobs.insert(chan_id.to_string()),
        };
    }
    debug!(
        "Read {} pull jobs and {} push jobs in {}ms",
        pull_jobs.len(),
        push_jobs.len(),
        now.elapsed().as_millis().to_string(),
    );

    Ok(())
}

pub async fn slingdeletejob(
    p: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<serde_json::Value, Error> {
    let sling_dir = Path::new(&p.configuration().lightning_dir).join(PLUGIN_NAME);
    match args {
        serde_json::Value::Array(a) => {
            if a.len() != 1 {
                return Err(anyhow!(
                    "Please provide exactly one short_channel_id or `all`"
                ));
            } else {
                match a.first().unwrap() {
                    serde_json::Value::String(i) => match i {
                        inp if inp.eq("all") => {
                            slingstop(p.clone(), serde_json::Value::Array(vec![])).await?;
                            let jobfile = sling_dir.join(JOB_FILE_NAME);
                            fs::remove_file(jobfile).await?;
                            info!("Deleted all jobs");
                        }
                        _ => {
                            write_job(p, sling_dir, i.clone(), None, true).await?;
                        }
                    },
                    _ => return Err(anyhow!("invalid string for deleting job(s)")),
                };
            }
        }
        _ => return Err(anyhow!("invalid arguments")),
    };

    Ok(json!({ "result": "success" }))
}

pub async fn read_jobs(
    sling_dir: &PathBuf,
    peers: &Vec<ListpeersPeers>,
) -> Result<HashMap<String, Job>, Error> {
    let jobfile = sling_dir.join(JOB_FILE_NAME);
    let jobfilecontent = fs::read_to_string(jobfile.clone()).await;
    let mut jobs: HashMap<String, Job>;

    create_sling_dir(sling_dir).await?;
    match jobfilecontent {
        Ok(file) => jobs = serde_json::from_str(&file).unwrap_or(HashMap::new()),
        Err(e) => {
            warn!(
                "Could not open {}: {}. Maybe this is the first time using sling? Creating new file.",
                jobfile.to_str().unwrap(), e.to_string()
            );
            File::create(jobfile.clone()).await?;
            jobs = HashMap::new();
        }
    };
    let channels = get_all_normal_channels_from_listpeers(peers);
    let channels = channels.keys().collect::<Vec<&String>>();

    jobs.retain(|c, _j| channels.contains(&c));
    Ok(jobs)
}

async fn write_job(
    p: Plugin<PluginState>,
    sling_dir: PathBuf,
    chan_id: String,
    job: Option<Job>,
    remove: bool,
) -> Result<HashMap<String, Job>, Error> {
    let peers = p.state().peers.lock().clone();
    let mut jobs = read_jobs(&sling_dir, &peers).await?;
    let job_change;
    let my_job;
    let jobstates = p.state().job_state.lock().clone();
    if jobstates.contains_key(&chan_id) && jobstates.get(&chan_id).unwrap().is_active() {
        slingstop(
            p.clone(),
            serde_json::Value::Array(vec![serde_json::Value::String(chan_id.clone())]),
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
                job_change = "Updating";
            }
        } else {
            job_change = "Creating";
        }
    }
    if remove {
        info!("{} job for {}", job_change, &chan_id);
    } else {
        my_job = job.unwrap();
        info!(
            "{} job for {} with amount {}msat, maxppm {}, outppm {:?}, target: {:?} and candidatelist {:?}",
            job_change,
            &chan_id,
            &my_job.amount,
            &my_job.maxppm,
            &my_job.outppm,
            &my_job.target,
            &my_job.candidatelist,
        );
        jobs.insert(chan_id, my_job);
    }
    let mut jobs_to_remove = Vec::new();
    if peers.len() > 0 {
        for (chan_id, _job) in &jobs {
            if let None =
                get_normal_channel_from_listpeers(&peers, ShortChannelId::from_str(&chan_id)?)
            {
                jobs_to_remove.push(chan_id.clone());
            }
        }
    }
    jobs.retain(|i, _j| !jobs_to_remove.contains(i));
    refresh_joblists(p.clone()).await?;
    fs::write(
        sling_dir.join(JOB_FILE_NAME),
        serde_json::to_string_pretty(&jobs)?,
    )
    .await?;
    Ok(jobs)
}

pub async fn slingexcept(
    plugin: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<serde_json::Value, Error> {
    let peers = plugin.state().peers.lock().clone();
    match args {
        serde_json::Value::Array(a) => {
            if a.len() != 2 {
                return Err(anyhow!(
                    "Please provide exactly two arguments: add/remove and a short_channel_id"
                ));
            } else {
                match a.get(0).unwrap() {
                    serde_json::Value::String(i) => {
                        let scid;
                        match a.get(1).unwrap() {
                            serde_json::Value::String(s) => {
                                scid = ShortChannelId::from_str(&s)?;
                            }
                            o => return Err(anyhow!("not a vaild short_channel_id: {}", o)),
                        }
                        let mut excepts = plugin.state().excepts.lock();
                        let mut contains = false;
                        for chan_id in excepts.clone() {
                            if chan_id.to_string() == scid.to_string() {
                                contains = true;
                            }
                        }
                        match i {
                            opt if opt.eq("add") => {
                                if contains {
                                    // excepts.retain(|&x| x.to_string() != scid.to_string());
                                    return Err(anyhow!(
                                        "{} is already in excepts",
                                        scid.to_string()
                                    ));
                                } else {
                                    let pull_jobs = plugin.state().pull_jobs.lock().clone();
                                    let push_jobs = plugin.state().push_jobs.lock().clone();
                                    let peer_channel = peers
                                        .iter()
                                        .flat_map(|peer| &peer.channels)
                                        .find(|channel| channel.short_channel_id == Some(scid));
                                    if let Some(_) = peer_channel {
                                        if pull_jobs.contains(&scid.to_string())
                                            || push_jobs.contains(&scid.to_string())
                                        {
                                            return Err(anyhow!(
                                        "this channel has a job already and can't be an except too"
                                    ));
                                        } else {
                                            excepts.push(scid);
                                        }
                                    } else {
                                        excepts.push(scid);
                                    }
                                }
                            }
                            opt if opt.eq("remove") => {
                                if contains {
                                    excepts.retain(|&x| x.to_string() != scid.to_string());
                                } else {
                                    return Err(anyhow!(
                                        "short_channel_id {} not in excepts, nothing to remove",
                                        scid.to_string()
                                    ));
                                }
                            }
                            _ => return Err(anyhow!("unknown commmand, add or remove only")),
                        }
                    }
                    _ => return Err(anyhow!("invalid command, add or remove only")),
                }
                write_excepts(plugin.clone()).await?;
                Ok(json!({ "result": "success" }))
            }
        }
        _ => Err(anyhow!("invalid arguments")),
    }
}

async fn write_excepts(plugin: Plugin<PluginState>) -> Result<(), Error> {
    let excepts = plugin.state().excepts.lock().clone();
    let sling_dir = Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME);
    let excepts_tostring = excepts
        .into_iter()
        .map(|x| x.to_string())
        .collect::<Vec<_>>();

    fs::write(
        sling_dir.join(EXCEPTS_FILE_NAME),
        serde_json::to_string(&excepts_tostring)?,
    )
    .await?;

    Ok(())
}
pub async fn read_excepts(plugin: Plugin<PluginState>) -> Result<(), Error> {
    let sling_dir = Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME);
    let exceptsfile = sling_dir.join(EXCEPTS_FILE_NAME);
    let exceptsfilecontent = fs::read_to_string(exceptsfile.clone()).await;
    let excepts_tostring: Vec<String>;
    let mut excepts: Vec<ShortChannelId> = Vec::new();

    create_sling_dir(&sling_dir).await?;
    match exceptsfilecontent {
        Ok(file) => excepts_tostring = serde_json::from_str(&file).unwrap_or(Vec::new()),
        Err(e) => {
            warn!(
                "Could not open {}: {}. Maybe this is the first time using sling? Creating new file.",
                exceptsfile.to_str().unwrap(), e.to_string()
            );
            File::create(exceptsfile.clone()).await?;
            excepts_tostring = Vec::new();
        }
    };

    for except in excepts_tostring {
        match ShortChannelId::from_str(&except) {
            Ok(id) => excepts.push(id),
            Err(_e) => warn!("excepts file contains invalid short_channel_id: {}", except),
        }
    }
    *plugin.state().excepts.lock() = excepts;
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
                    warn!("could not read graph: {}", e.to_string());
                    LnGraph::new()
                }
            }
        }
        Err(e) => {
            warn!(
                "Could not open {}: {}. Maybe this is the first time using sling? Creating new file.",
                graphfile.to_str().unwrap(), e.to_string()
            );
            File::create(graphfile.clone()).await?;
            graph = LnGraph::new();
        }
    };

    Ok(graph)
}
pub async fn write_graph(plugin: Plugin<PluginState>) -> Result<(), Error> {
    let graph = plugin.state().graph.lock().clone();
    let sling_dir = Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME);
    let now = Instant::now();
    fs::write(
        sling_dir.join(GRAPH_FILE_NAME),
        serde_json::to_string(&graph)?,
    )
    .await?;
    debug!(
        "Wrote graph to disk in {}ms",
        now.elapsed().as_millis().to_string()
    );
    Ok(())
}

async fn create_sling_dir(sling_dir: &PathBuf) -> Result<(), Error> {
    match fs::create_dir(sling_dir).await {
        Ok(_) => Ok(()),
        Err(e) => match e.kind() {
            io::ErrorKind::AlreadyExists => Ok(()),
            _ => Err(anyhow!("error: {}, could not create sling folder", e)),
        },
    }
}

pub fn channel_jobstate_update(
    jobstates: Arc<Mutex<HashMap<String, JobState>>>,
    chan_id: ShortChannelId,
    latest_state: JobMessage,
    active: Option<bool>,
    should_stop: Option<bool>,
) {
    let mut jobstates_mut = jobstates.lock();
    let jobstate = jobstates_mut.get_mut(&chan_id.to_string()).unwrap();
    jobstate.statechange(latest_state);
    match active {
        Some(a) => jobstate.set_active(a),
        None => (),
    }
    match should_stop {
        Some(s) => {
            if s {
                jobstate.stop()
            }
        }
        None => (),
    }
}

pub fn get_preimage_paymend_hash_pair() -> (String, Sha256) {
    let mut preimage = [0u8; 32];
    thread_rng().fill(&mut preimage[..]);

    let pi_str = serialize_hex(&preimage);

    let mut hasher = Sha256::engine();
    hasher.input(&preimage);
    let payment_hash = Sha256::from_engine(hasher);
    (pi_str, payment_hash)
}

pub fn get_out_htlc_count(channel: &ListpeersPeersChannels) -> u64 {
    match &channel.htlcs {
        Some(htlcs) => htlcs
            .into_iter()
            .filter(|htlc| match htlc.direction {
                ListpeersPeersChannelsHtlcsDirection::OUT => true,
                ListpeersPeersChannelsHtlcsDirection::IN => false,
            })
            .count() as u64,
        None => 0,
    }
}
pub fn get_in_htlc_count(channel: &ListpeersPeersChannels) -> u64 {
    match &channel.htlcs {
        Some(htlcs) => htlcs
            .into_iter()
            .filter(|htlc| match htlc.direction {
                ListpeersPeersChannelsHtlcsDirection::OUT => false,
                ListpeersPeersChannelsHtlcsDirection::IN => true,
            })
            .count() as u64,
        None => 0,
    }
}

pub fn get_peer_id_from_chan_id(
    peers: &Vec<ListpeersPeers>,
    channel: ShortChannelId,
) -> Result<PublicKey, Error> {
    let now = Instant::now();
    let peer = peers.iter().find(|peer| {
        peer.channels
            .iter()
            .any(|chan| chan.short_channel_id == Some(channel))
    });
    match peer {
        Some(p) => Ok(p.id),
        None => {
            debug!(
                "get_peer_id_from_chan_id in {}ms",
                now.elapsed().as_millis().to_string()
            );
            Err(anyhow!("{} disappeard from peers", channel.to_string()))
        }
    }
}

pub fn edge_cost(edge: &ListchannelsChannels, amount: u64) -> u64 {
    // debug!(
    //     "edge cost for {} source:{} is {}",
    //     edge.short_channel_id.to_string(),
    //     edge.source,
    //     (edge.base_fee_millisatoshi as f64
    //         + edge.fee_per_millionth as f64 / 1_000_000.0 * amount as f64) as u64
    // );
    fee_total_msat_precise(edge.fee_per_millionth, edge.base_fee_millisatoshi, amount).ceil() as u64
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
    chan_id: ShortChannelId,
) -> Option<ListpeersPeersChannels> {
    peers
        .iter()
        .flat_map(|peer| &peer.channels)
        .find(|channel| channel.short_channel_id == Some(chan_id) && is_channel_normal(channel))
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
