use std::{
    cmp::Ordering, collections::BTreeMap, path::Path, str::FromStr, sync::Arc, time::Duration,
};

use anyhow::anyhow;
use bitcoin::secp256k1::PublicKey;
use cln_plugin::{Error, Plugin};
use cln_rpc::primitives::ShortChannelId;
use parking_lot::Mutex;
use serde_json::json;
use sling::Job;
use tokio::{fs, time};

use crate::{
    get_normal_channel_from_listpeerchannels,
    model::{PubKeyBytes, TaskIdentifier},
    parse::{parse_job, parse_once_job},
    read_jobs,
    slings::sling,
    tasks::refresh_listpeerchannels,
    util::{read_except_chans, read_except_peers, write_liquidity},
    write_excepts, write_job, JobMessage, PluginState, Task, EXCEPTS_CHANS_FILE_NAME,
    EXCEPTS_PEERS_FILE_NAME, JOB_FILE_NAME, PLUGIN_NAME,
};

pub async fn slingjob(
    plugin: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<serde_json::Value, Error> {
    let _rpc_lock = plugin.state().rpc_lock.lock().await;
    let sling_dir = Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME);

    let config = plugin.state().config.lock().clone();

    let (chan_id, job) = parse_job(args, &config).await?;

    let _res = refresh_listpeerchannels(plugin.clone()).await;
    let peer_channels = plugin.state().peer_channels.lock().clone();
    let _our_listpeers_channel =
        get_normal_channel_from_listpeerchannels(&peer_channels, &chan_id)?;

    {
        let tasks = plugin.state().tasks.lock();
        if let Some(t) = tasks.get_scid_tasks(&chan_id) {
            if t.values().any(|ta| ta.is_once()) && tasks.is_any_active(&chan_id) {
                return Err(anyhow!("Once-job is currently running for this channel"));
            }
        }
    }

    let _res = stop_job(
        plugin.clone(),
        serde_json::Value::Array(vec![serde_json::to_value(chan_id)?]),
    )
    .await;

    write_job(plugin.clone(), sling_dir, chan_id, Some(job), false).await?;
    Ok(json!({"result":"success"}))
}

pub async fn slinggo(
    plugin: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<serde_json::Value, Error> {
    let _rpc_lock = plugin.state().rpc_lock.lock().await;
    let mut jobs = read_jobs(
        &Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME),
        plugin.clone(),
    )
    .await?;
    if jobs.is_empty() {
        return Err(anyhow!("No jobs found"));
    }

    let mut spawn_count = 0;
    let mut spawn_fail_count = 0;

    let config = plugin.state().config.lock().clone();

    match args {
        serde_json::Value::Array(a) => match a.len().cmp(&(1_usize)) {
            Ordering::Greater => {
                return Err(anyhow!(
                    "Please provide exactly one ShortChannelId or nothing"
                ))
            }
            Ordering::Equal => match a.first().unwrap() {
                serde_json::Value::String(start_id) => {
                    let scid = ShortChannelId::from_str(start_id)?;
                    jobs.retain(|chanid, _j| chanid == &scid)
                }
                _ => return Err(anyhow!("invalid ShortChannelId")),
            },
            Ordering::Less => (),
        },
        serde_json::Value::Object(o) => {
            if let Some(serde_json::Value::String(start_id)) = o.get("scid") {
                let scid = ShortChannelId::from_str(start_id.as_str())?;
                jobs.retain(|chanid, _j| chanid == &scid)
            } else if o.is_empty() {
            } else {
                return Err(anyhow!("invalid scid"));
            }
        }
        e => {
            return Err(anyhow!(
                "sling-go: invalid arguments, expected array or object with `scid`, got: {}",
                e
            ))
        }
    }

    if jobs.is_empty() {
        return Err(anyhow!("Shortchannelid not found in jobs"));
    }

    let _res = refresh_listpeerchannels(plugin.clone()).await;
    let peer_channels = plugin.state().peer_channels.lock().clone();

    for (chan_id, job) in jobs {
        let other_peer = PubKeyBytes::from_pubkey(
            &peer_channels
                .get(&chan_id)
                .ok_or(anyhow!("other_peer: channel not found"))?
                .peer_id,
        );
        let parallel_jobs = match job.sat_direction {
            sling::SatDirection::Pull => job.get_paralleljobs(config.paralleljobs),
            sling::SatDirection::Push => std::cmp::min(
                job.get_paralleljobs(config.paralleljobs),
                config.max_htlc_count as u16,
            ),
        };
        {
            let tasks = plugin.state().tasks.lock();
            if let Some(t) = tasks.get_scid_tasks(&chan_id) {
                if t.values().any(|ta| ta.is_once()) && tasks.is_any_active(&chan_id) {
                    log::info!("Once-job is currently running for {chan_id}");
                    spawn_fail_count += parallel_jobs;
                    continue;
                }
            }
        }
        for i in 1..=parallel_jobs {
            {
                let task_ident = TaskIdentifier::new(chan_id, i);
                let mut tasks = plugin.state().tasks.lock();
                let task = tasks.get_task_mut(&task_ident);

                if task.is_none() || !task.as_ref().unwrap().is_active() {
                    let plugin = plugin.clone();
                    let job_clone = job.clone();
                    spawn_count += 1;
                    log::debug!("{chan_id}/{i}: Spawning job.");
                    match task {
                        Some(jts) => {
                            jts.set_state(JobMessage::Starting);
                            jts.set_active(true);
                        }
                        None => {
                            tasks.insert_task(
                                chan_id,
                                i,
                                Task::new(chan_id, i, JobMessage::Starting, false, other_peer),
                            )?;
                        }
                    }
                    tokio::spawn(async move {
                        match sling(&job_clone, task_ident, plugin.clone()).await {
                            Ok(_o) => {
                                log::info!("{chan_id}/{i}: Spawned job exited.");
                                let mut tasks = plugin.state().tasks.lock();
                                let task = tasks.get_task_mut(&task_ident);
                                if let Some(t) = task {
                                    t.set_state(JobMessage::Stopped);
                                    t.set_active(false);
                                }
                            }
                            Err(e) => {
                                log::warn!("{chan_id}/{i}: Error in job: {e}");
                                let mut tasks = plugin.state().tasks.lock();
                                let task = tasks.get_task_mut(&task_ident);
                                if let Some(t) = task {
                                    t.set_state(JobMessage::Error);
                                    t.set_active(false);
                                }
                            }
                        };
                    });
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    Ok(json!({ "tasks_started": spawn_count , "tasks_failed_start:": spawn_fail_count }))
}

pub async fn slingstop(
    plugin: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<serde_json::Value, Error> {
    let _rpc_lock = plugin.state().rpc_lock.lock().await;

    let stopped_count = stop_job(plugin.clone(), args).await?;

    Ok(json!({ "stopped_count": stopped_count }))
}

async fn stop_job(plugin: Plugin<PluginState>, args: serde_json::Value) -> Result<usize, Error> {
    let mut stopped_count: usize = 0;
    let mut scid = None;

    match args {
        serde_json::Value::Array(a) => match a.len().cmp(&(1_usize)) {
            Ordering::Greater => {
                return Err(anyhow!(
                    "Please provide exactly one ShortChannelId or nothing"
                ))
            }
            Ordering::Equal => match a.first().unwrap() {
                serde_json::Value::String(stop_id) => {
                    scid = Some(ShortChannelId::from_str(stop_id)?);
                }
                _ => return Err(anyhow!("invalid ShortChannelId")),
            },
            Ordering::Less => {}
        },
        serde_json::Value::Object(o) => match o.get("scid") {
            Some(serde_json::Value::String(stop_id)) => {
                scid = Some(ShortChannelId::from_str(stop_id)?);
            }
            None => {}
            _ => return Err(anyhow!("invalid scid")),
        },
        e => {
            return Err(anyhow!(
                "sling-stop: invalid arguments, expected array or object with `scid`, got: {}",
                e
            ))
        }
    };
    {
        if let Some(s) = scid {
            {
                let mut tasks = plugin.state().tasks.lock();
                let task_map = tasks.get_scid_tasks_mut(&s);
                if let Some(tm) = task_map {
                    stopped_count += tm.len();
                    for task in tm.values_mut() {
                        task.set_state(JobMessage::Stopping);
                        task.stop();
                        log::debug!("{}: Stopping job...", task.get_identifier());
                    }
                }
            }
            loop {
                {
                    let tasks = plugin.state().tasks.lock();
                    if !tasks.is_any_active(&s) {
                        break;
                    }
                }
                log::trace!("Waiting for task to stop...");
                time::sleep(Duration::from_millis(200)).await;
            }
        } else {
            let mut stopped_ids = Vec::new();
            {
                let mut tasks = plugin.state().tasks.lock();
                let all_tasks_states = tasks.get_all_tasks_mut();
                for (chan_id, task_map) in all_tasks_states {
                    stopped_count += task_map.len();
                    stopped_ids.push(*chan_id);
                    for task in task_map.values_mut() {
                        task.set_state(JobMessage::Stopping);
                        task.stop();
                        log::debug!("{}: Stopping job...", task.get_identifier());
                    }
                }
            }
            loop {
                {
                    let tasks = plugin.state().tasks.lock();
                    let mut all_stopped = true;
                    for chan_id in stopped_ids.iter() {
                        let active = tasks.is_any_active(chan_id);
                        if active {
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
    }

    log::trace!("Stopped {stopped_count} tasks");
    write_liquidity(plugin.clone()).await?;
    Ok(stopped_count)
}

pub async fn slingonce(
    plugin: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<serde_json::Value, Error> {
    let _rpc_lock = plugin.state().rpc_lock.lock().await;

    let config_clone = plugin.state().config.lock().clone();
    let (chan_id, job) = parse_once_job(args, &config_clone).await?;

    let _res = refresh_listpeerchannels(plugin.clone()).await;
    let peer_channels = plugin.state().peer_channels.lock().clone();
    let our_listpeers_channel = get_normal_channel_from_listpeerchannels(&peer_channels, &chan_id)?;
    let other_peer = PubKeyBytes::from_pubkey(
        &peer_channels
            .get(&chan_id)
            .ok_or(anyhow!("other_peer: channel not found"))?
            .peer_id,
    );

    match job.sat_direction {
        sling::SatDirection::Pull => {
            if our_listpeers_channel
                .receivable_msat
                .ok_or_else(|| anyhow!("Missing receivable_msat field for channel"))?
                .msat()
                < job.onceamount_msat.unwrap()
            {
                return Err(anyhow!(
                    "Channel {} has not enough capacity to pull {}msat",
                    chan_id,
                    job.onceamount_msat.unwrap()
                ));
            }
        }
        sling::SatDirection::Push => {
            if our_listpeers_channel
                .spendable_msat
                .ok_or_else(|| anyhow!("Missing spendable_msat field for channel"))?
                .msat()
                < job.onceamount_msat.unwrap()
            {
                return Err(anyhow!(
                    "Channel {} has not enough capacity to push {}msat",
                    chan_id,
                    job.onceamount_msat.unwrap()
                ));
            }
        }
    }

    let sling_dir = Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME);
    let jobs = read_jobs(&sling_dir, plugin.clone()).await?;
    if jobs.contains_key(&chan_id) {
        return Err(anyhow!("There is already a job for that scid!"));
    }

    let parallel_jobs;
    {
        let mut config = plugin.state().config.lock();
        match job.sat_direction {
            sling::SatDirection::Pull => config.exclude_chans_pull.insert(chan_id),
            sling::SatDirection::Push => config.exclude_chans_push.insert(chan_id),
        };
        parallel_jobs = match job.sat_direction {
            sling::SatDirection::Pull => job.get_paralleljobs(config.paralleljobs),
            sling::SatDirection::Push => std::cmp::min(
                job.get_paralleljobs(config.paralleljobs),
                config.max_htlc_count as u16,
            ),
        };
    }

    for i in 1..=parallel_jobs {
        let task_ident = TaskIdentifier::new(chan_id, i);
        let mut tasks = plugin.state().tasks.lock();
        let task = tasks.get_task_mut(&task_ident);
        if let Some(t) = task {
            if !t.is_once() {
                return Err(anyhow!(
                    "There is already a job for that scid! Please delete it first."
                ));
            }
            if t.is_active() {
                return Err(anyhow!("There is already a job for that scid running!"));
            }
        }
    }

    let p_clone = plugin.clone();

    tokio::spawn(async move {
        let total_rebalanced = Arc::new(Mutex::new(0));
        for i in 1..=parallel_jobs {
            let plugin = p_clone.clone();
            let job_clone = job.clone();
            let total_rebalanced = total_rebalanced.clone();
            tokio::spawn(async move {
                let task_ident = TaskIdentifier::new(chan_id, i);
                {
                    let mut tasks = plugin.state().tasks.lock();
                    let task = tasks.get_task_mut(&task_ident);
                    if task.is_none() || !task.as_ref().unwrap().is_active() {
                        match task {
                            Some(jts) => {
                                jts.set_state(JobMessage::Starting);
                                jts.set_active(true);
                            }
                            None => {
                                match tasks.insert_task(
                                    chan_id,
                                    i,
                                    Task::new(chan_id, i, JobMessage::Starting, true, other_peer),
                                ) {
                                    Ok(_) => {}
                                    Err(e) => {
                                        log::error!("Error inserting task: {e}");
                                        return;
                                    }
                                };
                            }
                        }
                    }
                }
                loop {
                    {
                        let mut tasks = plugin.state().tasks.lock();
                        let task = tasks.get_task_mut(&task_ident).unwrap();
                        let mut total_rebalanced = total_rebalanced.lock();
                        if task.should_stop() {
                            log::debug!("{chan_id}/{i}: Spawned once-job exited.");
                            task.set_state(JobMessage::Stopped);
                            task.set_active(false);
                            let mut config = plugin.state().config.lock();
                            match job.sat_direction {
                                sling::SatDirection::Pull => {
                                    config.exclude_chans_pull.remove(&chan_id)
                                }
                                sling::SatDirection::Push => {
                                    config.exclude_chans_push.remove(&chan_id)
                                }
                            };
                            break;
                        }
                        if *total_rebalanced + job.amount_msat > job.onceamount_msat.unwrap() {
                            log::debug!("{chan_id}/{i}: Done rebalancing.");
                            task.set_state(JobMessage::Balanced);
                            task.set_active(false);
                            let mut config = plugin.state().config.lock();
                            match job.sat_direction {
                                sling::SatDirection::Pull => {
                                    config.exclude_chans_pull.remove(&chan_id)
                                }
                                sling::SatDirection::Push => {
                                    config.exclude_chans_push.remove(&chan_id)
                                }
                            };
                            break;
                        } else {
                            *total_rebalanced += job.amount_msat;
                        }
                    }
                    log::debug!("{chan_id}/{i}: Spawning once-job.");
                    match sling(&job_clone, task_ident, plugin.clone()).await {
                        Ok(o) => {
                            if o == 0 {
                                *total_rebalanced.lock() -= job.amount_msat;
                            }
                        }
                        Err(e) => {
                            log::warn!("{chan_id}/{e}: Error in once-job: {i}");
                            let mut tasks = plugin.state().tasks.lock();
                            let task = tasks.get_task_mut(&task_ident).unwrap();
                            task.set_state(JobMessage::Error);
                            *total_rebalanced.lock() -= job.amount_msat;
                        }
                    };

                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            });
        }
    });
    Ok(json!({ "result": "started" }))
}

pub async fn slingjobsettings(
    plugin: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<serde_json::Value, Error> {
    let _rpc_lock = plugin.state().rpc_lock.lock().await;
    let sling_dir = Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME);
    let jobs = read_jobs(&sling_dir, plugin.clone()).await?;
    let mut json_jobs: BTreeMap<ShortChannelId, Job> = BTreeMap::new();
    match args {
        serde_json::Value::Array(a) => {
            if a.len() > 1 {
                return Err(anyhow!(
                    "Please provide exactly one ShortChannelId or nothing for all"
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
        serde_json::Value::Object(o) => {
            if o.len() > 1 {
                return Err(anyhow!(
                    "Please provide exactly one ShortChannelId or nothing for all"
                ));
            }
            if o.is_empty() {
                for (id, job) in jobs {
                    json_jobs.insert(id, job);
                }
            } else if let Some(s) = o.get("scid") {
                let scid_str = s.as_str().ok_or(anyhow!("invalid scid, not a string"))?;
                let scid = ShortChannelId::from_str(scid_str)?;
                let job = jobs.get(&scid).ok_or(anyhow!("channel not found"))?;
                json_jobs.insert(scid, job.clone());
            } else {
                return Err(anyhow!("Expected object with scid field"));
            }
        }
        _ => {
            return Err(anyhow!(
                "Invalid: Please provide exactly one ShortChannelId or nothing for all"
            ))
        }
    }

    Ok(json!(json_jobs))
}

pub async fn slingdeletejob(
    plugin: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<serde_json::Value, Error> {
    let _rpc_lock = plugin.state().rpc_lock.lock().await;
    let sling_dir = Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME);
    let input = match args {
        serde_json::Value::Array(a) => {
            if a.len() != 1 {
                return Err(anyhow!(
                    "Please provide exactly one ShortChannelId or `all`"
                ));
            } else {
                match a.first().unwrap() {
                    serde_json::Value::String(i) => i.clone(),
                    _ => return Err(anyhow!("invalid string for deleting job(s)")),
                }
            }
        }
        serde_json::Value::Object(o) => {
            if o.len() != 1 {
                return Err(anyhow!(
                    "Please provide exactly one ShortChannelId or `all`"
                ));
            } else {
                match o.get("job") {
                    Some(serde_json::Value::String(i)) => i.clone(),
                    _ => return Err(anyhow!("invalid string for deleting job(s)")),
                }
            }
        }
        e => {
            return Err(anyhow!(
                "sling-deletejob: invalid arguments, expected array or object with `job`, got {}",
                e
            ))
        }
    };

    match input {
        inp if inp.eq("all") => {
            stop_job(plugin.clone(), serde_json::Value::Array(vec![])).await?;
            let jobfile = sling_dir.join(JOB_FILE_NAME);
            fs::remove_file(jobfile).await?;
            plugin.state().tasks.lock().remove_all_tasks();
            log::info!("Deleted all jobs");
            let except_chans = read_except_chans(&sling_dir).await?;
            let mut config = plugin.state().config.lock();
            config.exclude_chans_pull = except_chans.clone();
            config.exclude_chans_push = except_chans;
        }
        _ => {
            let scid = ShortChannelId::from_str(&input)?;
            stop_job(
                plugin.clone(),
                serde_json::Value::Array(vec![serde_json::to_value(scid)?]),
            )
            .await?;
            write_job(plugin.clone(), sling_dir, scid, None, true).await?;
            plugin.state().tasks.lock().remove_task(&scid);
            let mut config = plugin.state().config.lock();
            config.exclude_chans_pull.remove(&scid);
            config.exclude_chans_push.remove(&scid);
        }
    };

    Ok(json!({ "result": "success" }))
}

pub async fn slingexceptchan(
    plugin: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<serde_json::Value, Error> {
    let _rpc_lock = plugin.state().rpc_lock.lock().await;

    let (command, scid) = match args {
        serde_json::Value::Array(a) => {
            if a.len() > 2 || a.is_empty() {
                return Err(anyhow!(
                    "Invalid amount of arguments. Please either provide `add`/`remove` \
                    and a ShortChannelId or just `list`"
                ));
            }
            let command = match a.first().unwrap() {
                serde_json::Value::String(i) => i.clone(),
                _ => {
                    return Err(anyhow!(
                        "Not a string: Use `add`/`remove` and a ShortChannelId or just `list`"
                    ))
                }
            };
            if command == "list" && a.len() == 1 {
                (command, None)
            } else if a.len() == 2 {
                let scid = match a.get(1).unwrap() {
                    serde_json::Value::String(s) => ShortChannelId::from_str(s)?,
                    o => return Err(anyhow!("not a vaild string: {}", o)),
                };
                (command, Some(scid))
            } else {
                return Err(anyhow!(
                    "Invalid amount of arguments. Please either provide `add`/`remove` \
                    and a ShortChannelId or just `list`"
                ));
            }
        }
        serde_json::Value::Object(o) => {
            let command = match o.get("command") {
                Some(serde_json::Value::String(i)) => i.clone(),
                _ => {
                    return Err(anyhow!(
                        "Not a string: Use `add`/`remove` and a ShortChannelId or just `list`"
                    ))
                }
            };
            match o.get("scid") {
                Some(serde_json::Value::String(s)) => (command, Some(ShortChannelId::from_str(s)?)),
                None => (command, None),
                o => return Err(anyhow!("not a vaild string for `scid`: {:?}", o)),
            }
        }
        e => {
            return Err(anyhow!(
                "sling-exceptchan: invalid arguments, expected array or object, got {}",
                e
            ))
        }
    };

    let sling_dir = Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME);
    let peer_channels = plugin.state().peer_channels.lock().clone();
    let mut static_excepts = read_except_chans(&sling_dir).await?;
    if let Some(s) = scid {
        {
            let jobs = read_jobs(&sling_dir, plugin.clone()).await?;
            let mut config = plugin.state().config.lock();

            let mut contains = false;
            if config.exclude_chans_pull.contains(&s) || config.exclude_chans_push.contains(&s) {
                contains = true;
            }

            match command {
                opt if opt.eq("add") => {
                    if contains {
                        return Err(anyhow!("{} is already in excepts", s));
                    }
                    if jobs.contains_key(&s) {
                        return Err(anyhow!(
                            "this channel has a job already and can't be an except too"
                        ));
                    }
                    if peer_channels.contains_key(&s) {
                        return Err(anyhow!(
                            "You can't except your own channels. Use the candidate list of a \
                            job to restrict those."
                        ));
                    }
                    config.exclude_chans_pull.insert(s);
                    config.exclude_chans_push.insert(s);
                    static_excepts.insert(s);
                }
                opt if opt.eq("remove") => {
                    if contains {
                        config.exclude_chans_pull.remove(&s);
                        config.exclude_chans_push.remove(&s);
                        static_excepts.remove(&s);
                    } else {
                        return Err(anyhow!(
                            "ShortChannelId {} not in excepts, nothing to remove",
                            s
                        ));
                    }
                }
                _ => {
                    return Err(anyhow!(
                        "Use `add`/`remove` and a ShortChannelId or just `list`"
                    ))
                }
            }
        }
        write_excepts(static_excepts, EXCEPTS_CHANS_FILE_NAME, &sling_dir).await?;
        Ok(json!({ "result": "success" }))
    } else {
        match command {
            opt if opt.eq("list") => Ok(json!(static_excepts)),
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
    let _rpc_lock = plugin.state().rpc_lock.lock().await;

    let (command, pubkey_bytes) = match args {
        serde_json::Value::Array(a) => {
            if a.len() > 2 || a.is_empty() {
                return Err(anyhow!(
                    "Invalid amount of arguments. Either provide `add`/`remove` \
                    and a peer `id` or just `list`"
                ));
            }
            let com = match a.first().unwrap() {
                serde_json::Value::String(c) => c.clone(),
                _ => {
                    return Err(anyhow!(
                        "Not a string. Use `add`/`remove` and a peer `id` or `list`"
                    ))
                }
            };
            if com == "list" && a.len() == 1 {
                (com, None)
            } else if a.len() == 2 {
                let pb = match a.get(1).unwrap() {
                    serde_json::Value::String(s) => PubKeyBytes::from_str(s)?,
                    o => return Err(anyhow!("node_id is not a string: {}", o)),
                };
                (com, Some(pb))
            } else {
                return Err(anyhow!(
                    "Invalid amount of arguments. Please either provide `add`/`remove` \
                    and a peer `id` or just `list`"
                ));
            }
        }
        serde_json::Value::Object(o) => {
            let command = match o.get("command") {
                Some(serde_json::Value::String(i)) => i.clone(),
                _ => {
                    return Err(anyhow!(
                        "Not a string: Use `add`/`remove` and a peer `id` or just `list`"
                    ))
                }
            };
            match o.get("id") {
                Some(serde_json::Value::String(s)) => (command, Some(PubKeyBytes::from_str(s)?)),
                None => (command, None),
                o => return Err(anyhow!("not a vaild string for peer `id`: {:?}", o)),
            }
        }
        e => {
            return Err(anyhow!(
                "sling-exceptpeer: invalid arguments, expected array or object, got {}",
                e
            ))
        }
    };

    let _res = refresh_listpeerchannels(plugin.clone()).await;
    let peer_channels = plugin.state().peer_channels.lock().clone();
    let sling_dir = Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME);
    let mut static_excepts = read_except_peers(&sling_dir).await?;
    if let Some(pb) = pubkey_bytes {
        let pubkey = pb.to_pubkey();
        {
            let jobs = read_jobs(&sling_dir, plugin.clone()).await?;
            let mut config = plugin.state().config.lock();
            match command {
                opt if opt.eq("add") => {
                    if config.exclude_peers.contains(&pb) {
                        return Err(anyhow!("{} is already in excepts", pubkey));
                    }
                    if config.pubkey_bytes == pb {
                        return Err(anyhow!("Can't exclude yourself"));
                    }
                    for scid in jobs.keys() {
                        if let Some(peer_chan) = peer_channels.get(scid) {
                            if peer_chan.peer_id == pubkey {
                                return Err(anyhow!(
                                    "this peer has a job already and can't be an except too"
                                ));
                            }
                        };
                    }
                    config.exclude_peers.insert(pb);
                    static_excepts.insert(pubkey);
                }
                opt if opt.eq("remove") => {
                    if static_excepts.contains(&pubkey) {
                        static_excepts.remove(&pubkey);
                        config.exclude_peers.remove(&pb);
                    } else {
                        return Err(anyhow!(
                            "peer `id` {} not in excepts, nothing to remove",
                            pubkey
                        ));
                    }
                }
                _ => {
                    return Err(anyhow!(
                        "Unknown commmand. Use `add`/`remove` and a peer `id` or `list`"
                    ))
                }
            }
        }
        write_excepts::<PublicKey>(static_excepts, EXCEPTS_PEERS_FILE_NAME, &sling_dir).await?;
        Ok(json!({ "result": "success" }))
    } else {
        match command {
            opt if opt.eq("list") => Ok(json!(static_excepts)),
            _ => Err(anyhow!(
                "unknown commmand, use `list` or forgot the peer `id`?"
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
