use std::{cmp::Ordering, collections::BTreeMap, path::Path, str::FromStr, time::Duration};

use anyhow::anyhow;
use bitcoin::secp256k1::PublicKey;
use cln_plugin::{Error, Plugin};
use cln_rpc::primitives::ShortChannelId;
use log::{debug, info, warn};
use serde_json::json;
use sling::Job;
use tokio::{fs, time};

use crate::{
    channel_jobstate_update, get_normal_channel_from_listpeerchannels, parse::parse_job, read_jobs,
    refresh_joblists, slings::sling, write_excepts, write_graph, write_job, JobMessage, JobState,
    PluginState, Task, EXCEPTS_CHANS_FILE_NAME, EXCEPTS_PEERS_FILE_NAME, JOB_FILE_NAME,
    PLUGIN_NAME,
};

pub async fn slingjob(
    p: Plugin<PluginState>,
    v: serde_json::Value,
) -> Result<serde_json::Value, Error> {
    let sling_dir = Path::new(&p.configuration().lightning_dir).join(PLUGIN_NAME);

    let (chan_id, job) = parse_job(v).await?;

    let peer_channels = p.state().peer_channels.lock().await.clone();
    let our_listpeers_channel = get_normal_channel_from_listpeerchannels(&peer_channels, &chan_id);

    if our_listpeers_channel.is_some() {
        write_job(p.clone(), sling_dir, chan_id, Some(job), false).await?;
        Ok(json!({"result":"success"}))
    } else {
        Err(anyhow!(
            "Could not find channel or not in CHANNELD_NORMAL state: {}",
            chan_id
        ))
    }
}

pub async fn slinggo(
    p: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<serde_json::Value, Error> {
    let mut jobs = read_jobs(
        &Path::new(&p.configuration().lightning_dir).join(PLUGIN_NAME),
        &p,
    )
    .await?;
    if jobs.is_empty() {
        return Err(anyhow!("No jobs found"));
    }
    let joblists_clone = p.clone();
    refresh_joblists(joblists_clone).await?;
    // let peers = list_peers(&rpc_path).await?.peers;

    let mut spawn_count = 0;

    let default_paralleljobs;
    {
        default_paralleljobs = p.state().config.lock().paralleljobs.value
    }

    match args {
        serde_json::Value::Array(a) => match a.len().cmp(&(1_usize)) {
            Ordering::Greater => {
                return Err(anyhow!(
                    "Please provide exactly one short_channel_id or nothing"
                ))
            }
            Ordering::Equal => match a.first().unwrap() {
                serde_json::Value::String(start_id) => {
                    let scid = ShortChannelId::from_str(start_id)?;
                    jobs.retain(|chanid, _j| chanid == &scid)
                }
                _ => return Err(anyhow!("invalid short_channel_id")),
            },
            Ordering::Less => (),
        },
        _ => return Err(anyhow!("invalid arguments")),
    }

    if jobs.is_empty() {
        return Err(anyhow!("Shortchannelid not found in jobs"));
    }

    for (chan_id, job) in jobs {
        let parallel_jobs = match job.paralleljobs {
            Some(pj) => pj,
            None => default_paralleljobs,
        };
        for i in 1..=parallel_jobs {
            {
                let mut job_states = p.state().job_state.lock();
                if !job_states.contains_key(&chan_id)
                    || match job_states
                        .get(&chan_id)
                        .unwrap()
                        .iter()
                        .find(|jt| jt.id() == i)
                    {
                        Some(jobstate) => !jobstate.is_active(),
                        None => true,
                    }
                {
                    let plugin = p.clone();
                    let job_clone = job.clone();
                    spawn_count += 1;
                    debug!("{}/{}: Spawning job.", chan_id, i);
                    match job_states.get_mut(&chan_id) {
                        Some(jts) => match jts.iter_mut().find(|jt| jt.id() == i) {
                            Some(jobstate) => *jobstate = JobState::new(JobMessage::Starting, i),
                            None => jts.push(JobState::new(JobMessage::Starting, i)),
                        },
                        None => {
                            job_states
                                .insert(chan_id, vec![JobState::new(JobMessage::Starting, i)]);
                        }
                    }
                    tokio::spawn(async move {
                        let task = Task {
                            chan_id,
                            task_id: i,
                        };
                        match sling(&job_clone, &task, &plugin).await {
                            Ok(()) => info!("{}/{}: Spawned job exited.", chan_id, i),
                            Err(e) => {
                                warn!("{}/{}: Error in job: {}", chan_id, e.to_string(), i);
                                match channel_jobstate_update(
                                    plugin.state().job_state.clone(),
                                    &task,
                                    &JobMessage::Error,
                                    false,
                                    true,
                                ) {
                                    Ok(_) => (),
                                    Err(e) => {
                                        warn!("{}/{}: Error updating jobstate: {}", chan_id, i, e)
                                    }
                                };
                            }
                        };
                    });
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    Ok(json!({ "jobs_started": spawn_count }))
}

pub async fn slingstop(
    p: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<serde_json::Value, Error> {
    let stopped_count;
    {
        match args {
            serde_json::Value::Array(a) => match a.len().cmp(&(1_usize)) {
                Ordering::Greater => {
                    return Err(anyhow!(
                        "Please provide exactly one short_channel_id or nothing"
                    ))
                }
                Ordering::Equal => match a.first().unwrap() {
                    serde_json::Value::String(stop_id) => {
                        let scid = ShortChannelId::from_str(stop_id)?;
                        {
                            let mut job_states = p.state().job_state.lock().clone();
                            if job_states.contains_key(&scid) {
                                let jobstate = job_states.get_mut(&scid).unwrap();
                                stopped_count = jobstate.len();
                                for jt in jobstate {
                                    channel_jobstate_update(
                                        p.state().job_state.clone(),
                                        &Task {
                                            chan_id: scid,
                                            task_id: jt.id(),
                                        },
                                        &JobMessage::Stopping,
                                        true,
                                        true,
                                    )?;
                                    debug!("{}/{}: Stopping job...", scid, jt.id());
                                }
                            } else {
                                return Err(anyhow!("{}: No job running", scid));
                            }
                        }
                        loop {
                            {
                                let job_states = p.state().job_state.lock();
                                if job_states.get(&scid).is_none()
                                    || job_states
                                        .get(&scid)
                                        .unwrap()
                                        .iter()
                                        .all(|j| !j.is_active())
                                {
                                    break;
                                }
                            }
                            time::sleep(Duration::from_millis(200)).await;
                        }
                    }
                    _ => return Err(anyhow!("invalid short_channel_id")),
                },
                Ordering::Less => {
                    let mut stopped_ids = Vec::new();
                    {
                        let job_states = p.state().job_state.lock().clone();
                        stopped_count = job_states.iter().fold(0, |acc, (_, vec)| acc + vec.len());
                        for (chan_id, jobstate) in job_states.iter() {
                            stopped_ids.push(*chan_id);
                            for jt in jobstate {
                                channel_jobstate_update(
                                    p.state().job_state.clone(),
                                    &Task {
                                        chan_id: *chan_id,
                                        task_id: jt.id(),
                                    },
                                    &JobMessage::Stopping,
                                    true,
                                    true,
                                )?;
                                debug!("{}/{}: Stopping job...", chan_id, jt.id());
                            }
                        }
                    }
                    loop {
                        {
                            let mut job_states = p.state().job_state.lock();
                            job_states.retain(|chan, _state| stopped_ids.contains(chan));
                            let mut all_stopped = true;
                            for (_chan_id, jobstate) in job_states.iter() {
                                if jobstate.iter().any(|j| j.is_active()) {
                                    all_stopped = false;
                                }
                            }
                            if all_stopped {
                                break;
                            }
                        }
                        time::sleep(Duration::from_millis(200)).await;
                    }
                }
            },
            _ => return Err(anyhow!("invalid arguments")),
        };
    }
    write_graph(p.clone()).await?;
    Ok(json!({ "stopped_count": stopped_count }))
}

pub async fn slingjobsettings(
    p: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<serde_json::Value, Error> {
    let sling_dir = Path::new(&p.configuration().lightning_dir).join(PLUGIN_NAME);
    let jobs = read_jobs(&sling_dir, &p).await?;
    let mut json_jobs: BTreeMap<ShortChannelId, Job> = BTreeMap::new();
    match args {
        serde_json::Value::Array(a) => {
            if a.len() > 1 {
                return Err(anyhow!(
                    "Please provide exactly one short_channel_id or nothing for all"
                ));
            }
            if a.is_empty() {
                for (id, job) in jobs {
                    json_jobs.insert(id, job);
                }
            } else {
                let scid_str = a
                    .first()
                    .unwrap()
                    .as_str()
                    .ok_or(anyhow!("invalid input, not a string"))?;
                let scid = ShortChannelId::from_str(scid_str)?;
                let job = jobs.get(&scid).ok_or(anyhow!("channel not found"))?;
                json_jobs.insert(scid, job.clone());
            }
        }
        _ => {
            return Err(anyhow!(
                "Invalid: Please provide exactly one short_channel_id or nothing for all"
            ))
        }
    }

    Ok(json!(json_jobs))
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
                            let scid = ShortChannelId::from_str(i)?;
                            write_job(p, sling_dir, scid, None, true).await?;
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

pub async fn slingexceptchan(
    plugin: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<serde_json::Value, Error> {
    let peer_channels = plugin.state().peer_channels.lock().await;
    let input_array = match args {
        serde_json::Value::Array(a) => a,
        _ => return Err(anyhow!("invalid arguments")),
    };
    if input_array.len() > 2 || input_array.is_empty() {
        return Err(anyhow!(
            "Please either provide `add`/`remove` and a short_channel_id or just `list`"
        ));
    }
    let command = match input_array.first().unwrap() {
        serde_json::Value::String(i) => i,
        _ => {
            return Err(anyhow!(
                "Use `add`/`remove` and a short_channel_id or just `list`"
            ))
        }
    };
    if input_array.len() == 2 {
        let scid = match input_array.get(1).unwrap() {
            serde_json::Value::String(s) => ShortChannelId::from_str(s)?,
            o => return Err(anyhow!("not a vaild short_channel_id: {}", o)),
        };
        {
            let mut excepts = plugin.state().excepts_chans.lock();
            let mut contains = false;
            for chan_id in excepts.iter() {
                if chan_id == &scid {
                    contains = true;
                }
            }
            match command {
                opt if opt.eq("add") => {
                    if contains {
                        return Err(anyhow!("{} is already in excepts", scid));
                    }
                    let pull_jobs = plugin.state().pull_jobs.lock().clone();
                    let push_jobs = plugin.state().push_jobs.lock().clone();
                    if peer_channels.get(&scid).is_some() {
                        if pull_jobs.contains(&scid) || push_jobs.contains(&scid) {
                            return Err(anyhow!(
                                "this channel has a job already and can't be an except too"
                            ));
                        }
                        excepts.insert(scid);
                    } else {
                        excepts.insert(scid);
                    }
                }
                opt if opt.eq("remove") => {
                    if contains {
                        excepts.retain(|&x| x != scid);
                    } else {
                        return Err(anyhow!(
                            "short_channel_id {} not in excepts, nothing to remove",
                            scid
                        ));
                    }
                }
                _ => {
                    return Err(anyhow!(
                        "Use `add`/`remove` and a short_channel_id or just `list`"
                    ))
                }
            }
        }
        let excepts = plugin.state().excepts_chans.lock().clone();
        let sling_dir = Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME);
        write_excepts(excepts, EXCEPTS_CHANS_FILE_NAME, &sling_dir).await?;
        Ok(json!({ "result": "success" }))
    } else {
        let excepts = plugin.state().excepts_chans.lock();
        match command {
            opt if opt.eq("list") => Ok(json!(excepts.clone())),
            _ => Err(anyhow!(
                "unknown commmand, did you misspell `list` or forgot the scid?"
            )),
        }
    }
}

pub async fn slingexceptpeer(
    plugin: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<serde_json::Value, Error> {
    let peer_channels = plugin.state().peer_channels.lock().await;
    let array = match args {
        serde_json::Value::Array(a) => a,
        _ => return Err(anyhow!("invalid arguments")),
    };
    if array.len() > 2 || array.is_empty() {
        return Err(anyhow!(
            "Either provide `add`/`remove` and a node_id or just `list`"
        ));
    }
    let command = match array.first().unwrap() {
        serde_json::Value::String(c) => c,
        _ => {
            return Err(anyhow!(
                "Invalid command. Use `add`/`remove` <node_id> or `list`"
            ))
        }
    };
    if array.len() == 2 {
        let pubkey = match array.get(1).unwrap() {
            serde_json::Value::String(s) => PublicKey::from_str(s)?,
            o => return Err(anyhow!("invaild node_id: {}", o)),
        };
        {
            let mut excepts_peers = plugin.state().excepts_peers.lock();
            let contains = excepts_peers.contains(&pubkey);
            match command {
                opt if opt.eq("add") => {
                    if contains {
                        return Err(anyhow!("{} is already in excepts", pubkey));
                    }
                    let pull_jobs = plugin.state().pull_jobs.lock().clone();
                    let push_jobs = plugin.state().push_jobs.lock().clone();
                    let all_jobs: Vec<ShortChannelId> =
                        pull_jobs.into_iter().chain(push_jobs.into_iter()).collect();
                    let mut all_job_peers: Vec<PublicKey> = vec![];
                    debug!("{:?}", all_jobs);
                    for job in &all_jobs {
                        match peer_channels.get(job) {
                            Some(peer) => all_job_peers.push(peer.peer_id.unwrap()),
                            None => return Err(anyhow!("peer not found")),
                        };
                    }
                    if all_job_peers.contains(&pubkey) {
                        return Err(anyhow!(
                            "this peer has a job already and can't be an except too"
                        ));
                    }
                    excepts_peers.insert(pubkey);
                }
                opt if opt.eq("remove") => {
                    if contains {
                        excepts_peers.retain(|&x| x != pubkey);
                    } else {
                        return Err(anyhow!(
                            "node_id {} not in excepts, nothing to remove",
                            pubkey
                        ));
                    }
                }
                _ => {
                    return Err(anyhow!(
                        "Unknown commmand. Use `add`/`remove` <node_id> or `list`"
                    ))
                }
            }
        }
        let excepts = plugin.state().excepts_peers.lock().clone();
        let sling_dir = Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME);
        write_excepts::<PublicKey>(excepts, EXCEPTS_PEERS_FILE_NAME, &sling_dir).await?;
        Ok(json!({ "result": "success" }))
    } else {
        let excepts = plugin.state().excepts_peers.lock();
        match command {
            opt if opt.eq("list") => Ok(json!(excepts.clone())),
            _ => Err(anyhow!(
                "unknown commmand, use `list` or forgot the node_id?"
            )),
        }
    }
}

pub async fn slingversion(
    _p: Plugin<PluginState>,
    _args: serde_json::Value,
) -> Result<serde_json::Value, Error> {
    Ok(json!({ "version": format!("v{}",env!("CARGO_PKG_VERSION")) }))
}
