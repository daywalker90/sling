use std::path::Path;
use std::str::FromStr;

use crate::model::{JobMessage, JobState};
use crate::sling::sling;
use crate::util::{read_jobs, refresh_joblists, write_graph};
use crate::{make_rpc_path, PluginState, PLUGIN_NAME};
use anyhow::{anyhow, Error};
use cln_plugin::Plugin;

use cln_rpc::primitives::ShortChannelId;
use log::{debug, info, warn};

use serde_json::json;

pub async fn slinggo(
    p: Plugin<PluginState>,
    _v: serde_json::Value,
) -> Result<serde_json::Value, Error> {
    let peers = p.state().peers.lock().clone();
    let jobs = read_jobs(
        &Path::new(&p.configuration().lightning_dir).join(PLUGIN_NAME),
        &peers,
    )
    .await?;
    if jobs.len() == 0 {
        return Err(anyhow!("No jobs found"));
    }
    let joblists_clone = p.clone();
    refresh_joblists(joblists_clone).await?;
    // let peers = list_peers(&rpc_path).await?.peers;
    let mut job_states = p.state().job_state.lock();
    let mut spawn_count = 0;

    for (chan_id, job) in jobs {
        if !job_states.contains_key(&chan_id.to_string())
            || !job_states.get(&chan_id.to_string()).unwrap().is_active()
        {
            let plugin = p.clone();
            spawn_count += 1;
            debug!("{}: Spawning job.", chan_id.to_string());
            job_states.insert(chan_id.clone(), JobState::new(JobMessage::Starting));
            tokio::spawn(async move {
                match sling(
                    &make_rpc_path(&plugin),
                    ShortChannelId::from_str(&chan_id).unwrap(),
                    job,
                    &plugin,
                )
                .await
                {
                    Ok(()) => info!("{}: Spawned job exited.", chan_id.to_string()),
                    Err(e) => {
                        warn!("{}: Error in job: {}", chan_id, e.to_string());
                        let mut jobstates = plugin.state().job_state.lock();
                        let jobstate = jobstates.get_mut(&chan_id).unwrap();
                        jobstate.statechange(JobMessage::Error);
                        jobstate.set_active(false);
                    }
                };
            });
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
        let mut job_states = p.state().job_state.lock();
        match args {
            serde_json::Value::Array(a) => {
                if a.len() > 1 {
                    return Err(anyhow!(
                        "Please provide exactly one short_channel_id or nothing"
                    ));
                } else if a.len() == 1 {
                    match a.first().unwrap() {
                        serde_json::Value::String(i) => {
                            if job_states.contains_key(i) {
                                stopped_count = 1;
                                let jobstate = job_states.get_mut(i).unwrap();
                                jobstate.stop();
                                jobstate.statechange(JobMessage::Stopping);
                                jobstate.set_active(false);
                                debug!("{}: Stopping job...", i.to_string());
                            } else {
                                return Err(anyhow!("no job running for {}", i));
                            }
                        }
                        _ => return Err(anyhow!("invalid short_channel_id")),
                    };
                } else {
                    stopped_count = job_states.len();
                    for (chan_id, jobstate) in job_states.iter_mut() {
                        jobstate.stop();
                        jobstate.statechange(JobMessage::Stopping);
                        jobstate.set_active(false);
                        debug!("{}: Stopping job...", chan_id.to_string());
                    }
                }
            }
            _ => return Err(anyhow!("invalid arguments")),
        };
    }
    write_graph(p.clone()).await?;
    Ok(json!({ "stopped_count": stopped_count }))
}
