use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use anyhow::{Error, anyhow};
use bitcoin::{
    consensus::encode::serialize_hex,
    secp256k1::hashes::{Hash, HashEngine},
};
use cln_plugin::Plugin;
use cln_rpc::{
    model::responses::ListpeerchannelsChannels,
    primitives::{
        Amount,
        ChannelState,
        PublicKey,
        Secret,
        Sha256,
        ShortChannelId,
        ShortChannelIdDir,
    },
};
use rand::{Rng, rng};
use sling::{Job, SatDirection};
use tokio::{
    fs::{self, File},
    time::{self, Instant},
};

use crate::{
    ShortChannelIdDirState,
    gossip::InboundFee,
    model::{
        EXCEPTS_CHANS_FILE_NAME,
        EXCEPTS_PEERS_FILE_NAME,
        JOB_FILE_NAME,
        JobMessage,
        LIQUIDITY_FILE_NAME,
        Liquidity,
        PLUGIN_NAME,
        PluginState,
        TaskIdentifier,
    },
};

pub(crate) fn parse_jobs_content(content: &str) -> Result<BTreeMap<ShortChannelId, Job>, Error> {
    if content.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    serde_json::from_str(content).map_err(|e| anyhow!("Failed to parse {JOB_FILE_NAME}: {e}"))
}

pub async fn read_jobs(
    sling_dir: &PathBuf,
    plugin: Plugin<PluginState>,
) -> Result<BTreeMap<ShortChannelId, Job>, Error> {
    let jobfile = sling_dir.join(JOB_FILE_NAME);
    let jobfilecontent = fs::read_to_string(jobfile.clone()).await;
    let mut jobs: BTreeMap<ShortChannelId, Job>;

    create_sling_dir(sling_dir).await?;
    match jobfilecontent {
        Ok(file) => jobs = parse_jobs_content(&file)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            log::warn!(
                "Couldn't open {}: {}. First time using sling? Creating new file.",
                jobfile.display(),
                e
            );
            File::create(jobfile.clone()).await?;
            jobs = BTreeMap::new();
        }
        Err(e) => {
            return Err(anyhow!("Could not read {}: {e}", jobfile.display()));
        }
    }
    let peer_channels = plugin.state().peer_channels.lock().clone();
    if !peer_channels.is_empty() {
        let channels = get_all_normal_channels_from_listpeerchannels(&peer_channels);
        jobs.retain(|c, _j| channels.contains_key(c));
    }

    refresh_job_excepts(plugin, sling_dir, &jobs).await?;

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
        log::info!("{job_change} job for {chan_id}");
    } else {
        my_job = job.unwrap();
        log::info!("{job_change} job for {chan_id}:");
        log::info!("{my_job}");
        jobs.insert(chan_id, my_job);
    }
    let peer_channels = plugin.state().peer_channels.lock().clone();
    let mut jobs_to_remove = HashSet::new();
    if !peer_channels.is_empty() {
        for chan_id in jobs.keys() {
            if get_normal_channel_from_listpeerchannels(&peer_channels, *chan_id).is_err() {
                jobs_to_remove.insert(*chan_id);
            }
        }
    }
    jobs.retain(|i, _j| !jobs_to_remove.contains(i));
    let tmp_jobfile = sling_dir.join(format!("{JOB_FILE_NAME}.tmp"));
    fs::write(&tmp_jobfile, serde_json::to_string_pretty(&jobs)?).await?;
    fs::rename(&tmp_jobfile, sling_dir.join(JOB_FILE_NAME)).await?;

    refresh_job_excepts(plugin, &sling_dir, &jobs).await?;

    Ok(jobs)
}

async fn refresh_job_excepts(
    plugin: Plugin<PluginState>,
    sling_dir: &PathBuf,
    jobs: &BTreeMap<ShortChannelId, Job>,
) -> Result<(), Error> {
    let static_excepts = read_except_chans(sling_dir).await?;
    let mut config = plugin.state().config.lock();
    config.exclude_chans_pull.clear();
    config.exclude_chans_push.clear();
    for (scid, job) in jobs {
        match job.sat_direction {
            SatDirection::Pull => config.exclude_chans_pull.insert(*scid),
            SatDirection::Push => config.exclude_chans_push.insert(*scid),
        };
    }
    for except in static_excepts {
        config.exclude_chans_pull.insert(except);
        config.exclude_chans_push.insert(except);
    }

    Ok(())
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
                    log::warn!("could not read liquidity: {e}");
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
    }

    Ok(liquidity)
}
pub async fn write_liquidity(plugin: Plugin<PluginState>) -> Result<(), Error> {
    let graph_string = serde_json::to_string(&*plugin.state().liquidity.lock())?;
    let sling_dir = Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME);
    let now = Instant::now();
    fs::write(sling_dir.join(LIQUIDITY_FILE_NAME), graph_string).await?;
    log::debug!("Wrote liquidity to disk in {}ms", now.elapsed().as_millis());
    Ok(())
}

pub async fn create_sling_dir(sling_dir: &PathBuf) -> Result<(), Error> {
    match fs::create_dir(sling_dir).await {
        Ok(()) => Ok(()),
        Err(e) => match e.kind() {
            io::ErrorKind::AlreadyExists => Ok(()),
            _ => Err(anyhow!("error: {e}, could not create sling folder")),
        },
    }
}

pub fn get_preimage_paymend_hash_pair() -> (String, Sha256) {
    let mut preimage = [0u8; 32];
    rng().fill_bytes(&mut preimage[..]);

    let pi_str = serialize_hex(&preimage);

    let mut hasher = Sha256::engine();
    hasher.input(&preimage);
    let payment_hash = Sha256::from_engine(hasher);
    (pi_str, payment_hash)
}

pub fn get_payment_secret() -> Secret {
    let mut secret = [0u8; 32];
    rng().fill_bytes(&mut secret[..]);
    Secret::try_from(secret.to_vec()).expect("32 bytes is always a valid Secret")
}

pub fn get_total_htlc_count(channel: &ListpeerchannelsChannels) -> u64 {
    match &channel.htlcs {
        Some(htlcs) => htlcs.len() as u64,
        None => 0,
    }
}

pub fn div_ceil_i64(a: i64, b: i64) -> i64 {
    debug_assert!(b > 0);
    let q = a / b;
    let r = a % b;
    if r > 0 { q + 1 } else { q }
}

pub fn edge_cost(
    edge: &ShortChannelIdDirState,
    inbound_fee: Option<InboundFee>,
    amount: u64,
) -> u64 {
    let Some(outbound_fee) = amount
        .checked_mul(u64::from(edge.fee_per_millionth))
        .map(|p| p.div_ceil(1_000_000))
        .and_then(|f| f.checked_add(u64::from(edge.base_fee_millisatoshi)))
    else {
        return u64::MAX;
    };

    let Some(net_received_amt) = amount.checked_add(outbound_fee) else {
        return u64::MAX;
    };

    let inbound_fee_msat: i64 = match inbound_fee {
        Some(inf) => {
            let Ok(net_i64) = i64::try_from(net_received_amt) else {
                return u64::MAX;
            };
            let Some(prop_fee) = net_i64
                .checked_mul(i64::from(inf.proportional_millionths))
                .map(|p| div_ceil_i64(p, 1_000_000))
            else {
                return u64::MAX;
            };
            let Some(fee) = prop_fee.checked_add(i64::from(inf.base_msat)) else {
                return u64::MAX;
            };
            fee
        }
        None => 0,
    };

    let total_fee = if inbound_fee_msat >= 0 {
        let Some(fee) = outbound_fee.checked_add(inbound_fee_msat as u64) else {
            return u64::MAX;
        };
        fee
    } else {
        outbound_fee.saturating_sub(inbound_fee_msat.unsigned_abs())
    };

    total_fee
        .checked_mul(1_000_000)
        .map_or(u64::MAX, |p| p.div_ceil(amount))
        .max(2)
}

pub fn feeppm_effective(feeppm: u32, basefee_msat: u32, amount_msat: u64) -> u64 {
    (fee_total_msat_precise(feeppm, basefee_msat, amount_msat) / amount_msat as f64 * 1_000_000.0)
        .ceil() as u64
}

pub fn fee_total_msat_precise(feeppm: u32, basefee_msat: u32, amount_msat: u64) -> f64 {
    f64::from(basefee_msat) + (f64::from(feeppm) / 1_000_000.0 * amount_msat as f64)
}

// pub fn amt_from_amt_sent_and_feeppm(amount_sent_msat: u64, feeppm: u32) -> u64 {
//     (amount_sent_msat as f64 / (1.0 + (feeppm as f64 / 1_000_000.0))).ceil() as u64
// }

// pub fn amt_from_amt_sent(amount_sent_msat: u64, feeppm: u32, basefee_msat: u32) -> u64 {
//     if (basefee_msat as u64) > amount_sent_msat {
//         panic!("CRITICAL ERROR: basefee_msat should be less than or equal to amount_sent_msat")
//     }
//     ((amount_sent_msat as f64 - basefee_msat as f64) / (1.0 + (feeppm as f64 / 1_000_000.0)))
//         .floor() as u64
// }

pub fn feeppm_effective_from_amts(amount_sent_msat: u64, amount_msat: u64) -> u32 {
    assert!(
        amount_sent_msat >= amount_msat,
        "CRITICAL ERROR: amount_sent_msat should be greater than or equal to amount_msat"
    );
    ((amount_sent_msat - amount_msat) as f64 / amount_msat as f64 * 1_000_000.0).ceil() as u32
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
    chan_id: ShortChannelId,
) -> Result<ListpeerchannelsChannels, Error> {
    if let Some(chan) = peer_channels.get(&chan_id) {
        match is_channel_normal(chan) {
            Ok(()) => Ok(chan.clone()),
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
    log::debug!("{task_ident}: Starting sleeper for {seconds}s");
    let timer = Instant::now();
    while timer.elapsed() < Duration::from_secs(seconds) {
        time::sleep(Duration::from_secs(1)).await;
        {
            if let Some(o) = plugin.state().tasks.lock().get_task(task_ident) {
                if o.should_stop() {
                    break;
                }
            } else {
                break;
            }
        }
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
                log::info!("{task}: graph is still empty. Sleeping...");
                task.set_state(JobMessage::GraphEmpty);
                if task.should_stop() {
                    break;
                }
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
    amount_msat: u64,
) -> Result<u64, Error> {
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
        u32::try_from(Amount::msat(&chan_updates.fee_base_msat))?,
        amount_msat,
    );
    Ok(chan_in_ppm)
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
        Err(e) => {
            if e.kind() == io::ErrorKind::NotFound {
                log::info!("{} not found. Creating...", excepts_file.display());
                File::create(excepts_file.clone()).await?;
                excepts_tostring = Vec::new();
            } else {
                log::warn!("Could not open {}: {}.", excepts_file.to_str().unwrap(), e);
                return Err(anyhow!(
                    "Could not open {}: {}.",
                    excepts_file.to_str().unwrap(),
                    e
                ));
            }
        }
    }

    for except in excepts_tostring {
        match T::from_str(&except) {
            Ok(id) => {
                excepts.insert(id);
            }
            Err(_e) => {
                log::warn!("excepts file contains invalid short_channel_id/node_id: {except}");
            }
        }
    }
    Ok(excepts)
}
