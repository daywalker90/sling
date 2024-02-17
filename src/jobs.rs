use std::cmp::Ordering;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use crate::model::{JobMessage, JobState, PluginState, Task, PLUGIN_NAME};
use crate::slings::sling;
use crate::util::{make_rpc_path, read_jobs, refresh_joblists, write_graph};
use anyhow::{anyhow, Error};
use cln_plugin::Plugin;

use cln_rpc::primitives::ShortChannelId;
use log::{debug, info, warn};

use serde_json::json;
use tokio::time;

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

    let config = p.state().config.lock().clone();

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
            None => config.paralleljobs.value,
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
                        match sling(
                            &make_rpc_path(&plugin),
                            &job_clone,
                            &Task {
                                chan_id,
                                task_id: i,
                            },
                            &plugin,
                        )
                        .await
                        {
                            Ok(()) => info!("{}/{}: Spawned job exited.", chan_id, i),
                            Err(e) => {
                                warn!("{}/{}: Error in job: {}", chan_id, e.to_string(), i);
                                let mut jobstates = plugin.state().job_state.lock();
                                let jobstate = jobstates.get_mut(&chan_id).unwrap();
                                let job_task = jobstate.iter_mut().find(|item| item.id() == i);

                                match job_task {
                                    Some(jt) => {
                                        jt.statechange(JobMessage::Error);
                                        jt.set_active(false);
                                    }
                                    None => warn!("{}/{}: Job not found after error", chan_id, i),
                                }
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
                            let mut job_states = p.state().job_state.lock();
                            if job_states.contains_key(&scid) {
                                let jobstate = job_states.get_mut(&scid).unwrap();
                                stopped_count = jobstate.len();
                                for jt in jobstate {
                                    jt.stop();
                                    jt.statechange(JobMessage::Stopping);
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
                        let mut job_states = p.state().job_state.lock();
                        stopped_count = job_states.iter().fold(0, |acc, (_, vec)| acc + vec.len());
                        for (chan_id, jobstate) in job_states.iter_mut() {
                            stopped_ids.push(*chan_id);
                            for jt in jobstate {
                                jt.stop();
                                jt.statechange(JobMessage::Stopping);
                                debug!("{}/{}: Stopping job...", chan_id, jt.id());
                            }
                        }
                    }
                    loop {
                        {
                            let mut job_states = p.state().job_state.lock().clone();
                            job_states.retain(|chan, _state| stopped_ids.contains(chan));
                            let mut all_stopped = true;
                            for (_chan_id, jobstate) in job_states.iter_mut() {
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
