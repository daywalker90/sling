use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt::{self, Display, Formatter},
    path::Path,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Error};
use cln_rpc::{
    model::responses::{ListchannelsChannels, ListpeerchannelsChannels},
    primitives::{Amount, PublicKey, ShortChannelId},
};
use log::debug;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use tabled::Tabled;
use tokio::{fs::OpenOptions, io::AsyncWriteExt};

use crate::{
    OPT_CANDIDATES_MIN_AGE, OPT_DEPLETEUPTOAMOUNT, OPT_DEPLETEUPTOPERCENT, OPT_LIGHTNING_CONF,
    OPT_MAXHOPS, OPT_MAX_HTLC_COUNT, OPT_PARALLELJOBS, OPT_REFRESH_ALIASMAP_INTERVAL,
    OPT_REFRESH_GRAPH_INTERVAL, OPT_REFRESH_PEERS_INTERVAL, OPT_RESET_LIQUIDITY_INTERVAL,
    OPT_STATS_DELETE_FAILURES_AGE, OPT_STATS_DELETE_FAILURES_SIZE, OPT_STATS_DELETE_SUCCESSES_AGE,
    OPT_STATS_DELETE_SUCCESSES_SIZE, OPT_TIMEOUTPAY, OPT_UTF8,
};

pub const SUCCESSES_SUFFIX: &str = "_successes.json";
pub const FAILURES_SUFFIX: &str = "_failures.json";
pub const NO_ALIAS_SET: &str = "NO_ALIAS_SET";

pub const PLUGIN_NAME: &str = "sling";
pub const GRAPH_FILE_NAME: &str = "graph.json";
pub const JOB_FILE_NAME: &str = "jobs.json";
pub const EXCEPTS_CHANS_FILE_NAME: &str = "excepts.json";
pub const EXCEPTS_PEERS_FILE_NAME: &str = "excepts_peers.json";

#[derive(Clone)]
pub struct PluginState {
    pub config: Arc<Mutex<Config>>,
    pub peer_channels: Arc<tokio::sync::Mutex<BTreeMap<ShortChannelId, ListpeerchannelsChannels>>>,
    pub graph: Arc<tokio::sync::Mutex<LnGraph>>,
    pub pays: Arc<RwLock<HashMap<String, String>>>,
    pub alias_peer_map: Arc<Mutex<HashMap<PublicKey, String>>>,
    pub pull_jobs: Arc<Mutex<HashSet<ShortChannelId>>>,
    pub push_jobs: Arc<Mutex<HashSet<ShortChannelId>>>,
    pub excepts_chans: Arc<Mutex<HashSet<ShortChannelId>>>,
    pub excepts_peers: Arc<Mutex<HashSet<PublicKey>>>,
    pub tempbans: Arc<Mutex<HashMap<ShortChannelId, u64>>>,
    pub job_state: Arc<Mutex<HashMap<ShortChannelId, Vec<JobState>>>>,
    pub blockheight: Arc<Mutex<u32>>,
}
impl PluginState {
    pub fn new() -> PluginState {
        PluginState {
            config: Arc::new(Mutex::new(Config::new())),
            peer_channels: Arc::new(tokio::sync::Mutex::new(BTreeMap::new())),
            graph: Arc::new(tokio::sync::Mutex::new(LnGraph::new())),
            pays: Arc::new(RwLock::new(HashMap::new())),
            alias_peer_map: Arc::new(Mutex::new(HashMap::new())),
            pull_jobs: Arc::new(Mutex::new(HashSet::new())),
            push_jobs: Arc::new(Mutex::new(HashSet::new())),
            excepts_chans: Arc::new(Mutex::new(HashSet::new())),
            excepts_peers: Arc::new(Mutex::new(HashSet::new())),
            tempbans: Arc::new(Mutex::new(HashMap::new())),
            job_state: Arc::new(Mutex::new(HashMap::new())),
            blockheight: Arc::new(Mutex::new(0)),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Task {
    pub chan_id: ShortChannelId,
    pub task_id: u8,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub pubkey: Option<PublicKey>,
    pub utf8: DynamicConfigOption<bool>,
    pub refresh_peers_interval: DynamicConfigOption<u64>,
    pub refresh_aliasmap_interval: DynamicConfigOption<u64>,
    pub refresh_graph_interval: DynamicConfigOption<u64>,
    pub reset_liquidity_interval: DynamicConfigOption<u64>,
    pub depleteuptopercent: DynamicConfigOption<f64>,
    pub depleteuptoamount: DynamicConfigOption<u64>,
    pub maxhops: DynamicConfigOption<u8>,
    pub candidates_min_age: DynamicConfigOption<u32>,
    pub paralleljobs: DynamicConfigOption<u8>,
    pub timeoutpay: DynamicConfigOption<u16>,
    pub max_htlc_count: DynamicConfigOption<u64>,
    pub lightning_conf: DynamicConfigOption<String>,
    pub stats_delete_failures_age: DynamicConfigOption<u64>,
    pub stats_delete_failures_size: DynamicConfigOption<u64>,
    pub stats_delete_successes_age: DynamicConfigOption<u64>,
    pub stats_delete_successes_size: DynamicConfigOption<u64>,
    pub cltv_delta: DynamicConfigOption<Option<u16>>,
}
impl Config {
    pub fn new() -> Config {
        Config {
            pubkey: None,
            utf8: DynamicConfigOption {
                name: OPT_UTF8.name,
                value: true,
            },
            refresh_peers_interval: DynamicConfigOption {
                name: OPT_REFRESH_PEERS_INTERVAL.name,
                value: 1,
            },
            refresh_aliasmap_interval: DynamicConfigOption {
                name: OPT_REFRESH_ALIASMAP_INTERVAL.name,
                value: 3600,
            },
            refresh_graph_interval: DynamicConfigOption {
                name: OPT_REFRESH_GRAPH_INTERVAL.name,
                value: 600,
            },
            reset_liquidity_interval: DynamicConfigOption {
                name: OPT_RESET_LIQUIDITY_INTERVAL.name,
                value: 360,
            },
            depleteuptopercent: DynamicConfigOption {
                name: OPT_DEPLETEUPTOPERCENT.name,
                value: 0.2,
            },
            depleteuptoamount: DynamicConfigOption {
                name: OPT_DEPLETEUPTOAMOUNT.name,
                value: 2_000_000_000,
            },
            maxhops: DynamicConfigOption {
                name: OPT_MAXHOPS.name,
                value: 8,
            },
            candidates_min_age: DynamicConfigOption {
                name: OPT_CANDIDATES_MIN_AGE.name,
                value: 0,
            },
            paralleljobs: DynamicConfigOption {
                name: OPT_PARALLELJOBS.name,
                value: 1,
            },
            timeoutpay: DynamicConfigOption {
                name: OPT_TIMEOUTPAY.name,
                value: 120,
            },
            max_htlc_count: DynamicConfigOption {
                name: OPT_MAX_HTLC_COUNT.name,
                value: 5,
            },
            lightning_conf: DynamicConfigOption {
                name: OPT_LIGHTNING_CONF.name,
                value: "".to_string(),
            },
            stats_delete_failures_age: DynamicConfigOption {
                name: OPT_STATS_DELETE_FAILURES_AGE.name,
                value: 30,
            },
            stats_delete_failures_size: DynamicConfigOption {
                name: OPT_STATS_DELETE_FAILURES_SIZE.name,
                value: 10_000,
            },
            stats_delete_successes_age: DynamicConfigOption {
                name: OPT_STATS_DELETE_SUCCESSES_AGE.name,
                value: 30,
            },
            stats_delete_successes_size: DynamicConfigOption {
                name: OPT_STATS_DELETE_SUCCESSES_SIZE.name,
                value: 10_000,
            },
            cltv_delta: DynamicConfigOption {
                name: "cltv-delta",
                value: None,
            },
        }
    }
}

#[derive(Clone, Debug)]
pub struct DynamicConfigOption<T> {
    pub name: &'static str,
    pub value: T,
}

#[derive(Debug, Clone)]
pub struct JobState {
    latest_state: JobMessage,
    active: bool,
    should_stop: bool,
    id: u8,
}
impl JobState {
    pub fn new(latest_state: JobMessage, id: u8) -> Self {
        JobState {
            latest_state,
            active: true,
            should_stop: false,
            id,
        }
    }
    pub fn missing() -> Self {
        JobState {
            latest_state: JobMessage::NoJob,
            active: false,
            should_stop: false,
            id: 0,
        }
    }
    pub fn statechange(&mut self, latest_state: JobMessage) {
        self.latest_state = latest_state;
    }
    pub fn state(&self) -> JobMessage {
        self.latest_state
    }
    pub fn stop(&mut self) {
        self.should_stop = true;
    }
    pub fn should_stop(&self) -> bool {
        self.should_stop
    }
    pub fn is_active(&self) -> bool {
        self.active
    }
    pub fn set_active(&mut self, active: bool) {
        self.active = active;
    }
    pub fn id(&self) -> u8 {
        self.id
    }
}

#[derive(Debug, Clone, Copy)]
pub enum JobMessage {
    Starting,
    Rebalancing,
    Balanced,
    NoCandidates,
    HTLCcapped,
    Disconnected,
    PeerNotFound,
    PeerNotReady,
    ChanNotNormal,
    GraphEmpty,
    ChanNotInGraph,
    NoRoute,
    TooExp,
    Stopping,
    Stopped,
    Error,
    NoJob,
}
impl Display for JobMessage {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            JobMessage::Starting => write!(f, "Starting"),
            JobMessage::Rebalancing => write!(f, "Rebalancing"),
            JobMessage::Balanced => write!(f, "Balanced"),
            JobMessage::NoCandidates => write!(f, "NoCandidates"),
            JobMessage::HTLCcapped => write!(f, "HTLCcapped"),
            JobMessage::Disconnected => write!(f, "Disconnected"),
            JobMessage::PeerNotFound => write!(f, "PeerNotFound"),
            JobMessage::PeerNotReady => write!(f, "PeerNotReady"),
            JobMessage::ChanNotNormal => write!(f, "ChanNotNormal"),
            JobMessage::GraphEmpty => write!(f, "GraphEmpty"),
            JobMessage::ChanNotInGraph => write!(f, "ChanNotInGraph"),
            JobMessage::NoRoute => write!(f, "NoRoutes"),
            JobMessage::TooExp => write!(f, "NoCheapRoute"),
            JobMessage::Stopping => write!(f, "Stopping"),
            JobMessage::Stopped => write!(f, "Stopped"),
            JobMessage::Error => write!(f, "Error"),
            JobMessage::NoJob => write!(f, "NoJob"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DijkstraNode {
    pub score: u64,
    pub channel: ListchannelsChannels,
    pub destination: PublicKey,
    pub hops: u8,
}
impl PartialEq for DijkstraNode {
    fn eq(&self, other: &Self) -> bool {
        self.score == other.score
            && self.hops == other.hops
            && self.channel.source == other.channel.source
            && self.channel.destination == other.channel.destination
            && self.channel.short_channel_id == other.channel.short_channel_id
            && self.destination == other.destination
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectedChannel {
    pub channel: ListchannelsChannels,
    pub liquidity: u64,
    pub timestamp: u64,
}
impl DirectedChannel {
    pub fn new(channel: ListchannelsChannels) -> DirectedChannel {
        DirectedChannel {
            liquidity: Amount::msat(&channel.htlc_maximum_msat.unwrap_or(channel.amount_msat)) / 2,
            channel,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PublicKeyPair {
    pub my_pubkey: PublicKey,
    pub other_pubkey: PublicKey,
}

#[derive(Clone, Debug)]
pub struct ExcludeGraph {
    pub exclude_chans: HashSet<ShortChannelId>,
    pub exclude_peers: HashSet<PublicKey>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LnGraph {
    pub graph: BTreeMap<PublicKey, Vec<DirectedChannel>>,
}
impl LnGraph {
    pub fn new() -> Self {
        LnGraph {
            graph: BTreeMap::new(),
        }
    }
    pub fn update(&mut self, new_graph: LnGraph) {
        for (new_node, new_channels) in new_graph.graph.iter() {
            let old_channels = self.graph.entry(*new_node).or_default();
            let new_short_channel_ids: HashSet<ShortChannelId> = new_channels
                .iter()
                .map(|c| c.channel.short_channel_id)
                .collect();
            old_channels.retain(|e| new_short_channel_ids.contains(&e.channel.short_channel_id));
            for new_channel in new_channels {
                let new_short_channel_id = &new_channel.channel.short_channel_id;
                let old_channel = old_channels
                    .iter_mut()
                    .find(|e| e.channel.short_channel_id == *new_short_channel_id);
                match old_channel {
                    Some(old_channel) => {
                        if (old_channel.channel.htlc_maximum_msat.is_some()
                            && new_channel.channel.htlc_maximum_msat.is_some()
                            && old_channel.channel.htlc_maximum_msat.unwrap()
                                != new_channel.channel.htlc_maximum_msat.unwrap())
                            || old_channel.channel.fee_per_millionth
                                != new_channel.channel.fee_per_millionth
                        {
                            old_channel.liquidity = new_channel.liquidity;
                            old_channel.timestamp = new_channel.timestamp;
                        }
                        old_channel.channel = new_channel.channel.clone();
                    }
                    None => {
                        old_channels.push(new_channel.clone());
                    }
                }
            }
        }

        let new_nodes: HashSet<&PublicKey> = new_graph.graph.keys().collect();
        self.graph.retain(|k, _| new_nodes.contains(k));
    }
    pub fn refresh_liquidity(&mut self, interval: u64) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut count = 0;
        for (_node, channels) in self.graph.iter_mut() {
            for channel in channels {
                if channel.timestamp <= now - interval * 60 {
                    channel.liquidity = Amount::msat(
                        &channel
                            .channel
                            .htlc_maximum_msat
                            .unwrap_or(channel.channel.amount_msat),
                    ) / 2;
                    channel.timestamp = now;
                    count += 1;
                }
            }
        }
        debug!("Reset liquidity belief on {} channels!", count);
    }
    pub fn get_channel(
        &self,
        source: &PublicKey,
        channel: &ShortChannelId,
    ) -> Result<DirectedChannel, Error> {
        match self.graph.get(source) {
            Some(e) => {
                let result = e
                    .iter()
                    .filter(|&i| i.channel.short_channel_id == *channel)
                    .collect::<Vec<&DirectedChannel>>();
                if result.len() != 1 {
                    Err(anyhow!("channel {} not found in graph", channel))
                } else {
                    Ok(result[0].clone())
                }
            }
            None => Err(anyhow!(
                "could not find channel in cached graph: {}",
                channel
            )),
        }
    }

    pub fn edges(
        &self,
        keypair: &PublicKeyPair,
        exclude_graph: &ExcludeGraph,
        amount: &u64,
        candidatelist: &[ShortChannelId],
        tempbans: &HashMap<ShortChannelId, u64>,
    ) -> Vec<&DirectedChannel> {
        match self.graph.get(&keypair.other_pubkey) {
            Some(e) => {
                e.iter()
                    .filter(|&i| {
                        // debug!(
                        //     "{}: liq:{} amt:{}",
                        //     i.channel.short_channel_id.to_string(),
                        //     i.liquidity,
                        //     amount
                        // );
                        !exclude_graph
                            .exclude_chans
                            .contains(&i.channel.short_channel_id)
                            && !tempbans.contains_key(&i.channel.short_channel_id)
                            && i.liquidity >= *amount
                            && Amount::msat(&i.channel.htlc_minimum_msat) <= *amount
                            && Amount::msat(
                                &i.channel.htlc_maximum_msat.unwrap_or(i.channel.amount_msat),
                            ) >= *amount
                            && !exclude_graph.exclude_peers.contains(&i.channel.source)
                            && !exclude_graph.exclude_peers.contains(&i.channel.destination)
                            && if i.channel.source == keypair.my_pubkey
                                || i.channel.destination == keypair.my_pubkey
                            {
                                candidatelist
                                    .iter()
                                    .any(|c| c == &i.channel.short_channel_id)
                            } else {
                                true
                            }
                    })
                    .collect::<Vec<&DirectedChannel>>()
            }
            None => Vec::<&DirectedChannel>::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SuccessReb {
    pub amount_msat: u64,
    pub fee_ppm: u32,
    pub channel_partner: ShortChannelId,
    pub hops: u8,
    pub completed_at: u64,
}
impl SuccessReb {
    pub async fn write_to_file(
        &self,
        chan_id: ShortChannelId,
        sling_dir: &Path,
    ) -> Result<(), Error> {
        let serialized = serde_json::to_string(self)?;
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(sling_dir.join(chan_id.to_string() + SUCCESSES_SUFFIX))
            .await?;
        file.write_all(format!("{}\n", serialized).as_bytes())
            .await?;
        Ok(())
    }

    pub async fn read_from_file(
        sling_dir: &Path,
        chan_id: &ShortChannelId,
    ) -> Result<Vec<SuccessReb>, Error> {
        let contents =
            tokio::fs::read_to_string(sling_dir.join(chan_id.to_string() + SUCCESSES_SUFFIX))
                .await?;
        let mut vec = vec![];
        for line in contents.lines() {
            vec.push(serde_json::from_str(line)?);
        }
        Ok(vec)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FailureReb {
    pub amount_msat: u64,
    pub failure_reason: String,
    pub failure_node: PublicKey,
    pub channel_partner: ShortChannelId,
    pub hops: u8,
    pub created_at: u64,
}
impl FailureReb {
    pub async fn write_to_file(
        &self,
        chan_id: ShortChannelId,
        sling_dir: &Path,
    ) -> Result<(), Error> {
        let serialized = serde_json::to_string(self)?;
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(sling_dir.join(chan_id.to_string() + FAILURES_SUFFIX))
            .await?;
        file.write_all(format!("{}\n", serialized).as_bytes())
            .await?;
        Ok(())
    }

    pub async fn read_from_file(
        sling_dir: &Path,
        chan_id: &ShortChannelId,
    ) -> Result<Vec<FailureReb>, Error> {
        let contents =
            tokio::fs::read_to_string(sling_dir.join(chan_id.to_string() + FAILURES_SUFFIX))
                .await?;
        let mut vec = vec![];
        for line in contents.lines() {
            vec.push(serde_json::from_str(line)?);
        }
        Ok(vec)
    }
}

#[derive(Debug, Tabled)]
pub struct StatSummary {
    pub alias: String,
    pub scid: ShortChannelId,
    pub pubkey: PublicKey,
    pub status: String,
    pub rebamount: String,
    pub w_feeppm: u64,
    pub last_route_taken: String,
    pub last_success_reb: String,
}
