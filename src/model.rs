use std::{
    collections::{HashMap, HashSet},
    fmt::{self, Display, Formatter},
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Error};
use cln_rpc::{
    model::responses::ListpeerchannelsChannels,
    primitives::{Amount, PublicKey, ShortChannelId},
};
use log::{info, warn};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tabled::Tabled;
use tokio::{
    fs::{self, File, OpenOptions},
    io::{self, AsyncWriteExt},
};

use crate::{
    create_sling_dir,
    gossip::{ChannelAnnouncement, ChannelUpdate},
    OPT_CANDIDATES_MIN_AGE, OPT_DEPLETEUPTOAMOUNT, OPT_DEPLETEUPTOPERCENT, OPT_MAXHOPS,
    OPT_MAX_HTLC_COUNT, OPT_PARALLELJOBS, OPT_REFRESH_ALIASMAP_INTERVAL,
    OPT_REFRESH_GOSSMAP_INTERVAL, OPT_REFRESH_PEERS_INTERVAL, OPT_RESET_LIQUIDITY_INTERVAL,
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
    pub peer_channels: Arc<Mutex<HashMap<ShortChannelId, ListpeerchannelsChannels>>>,
    pub graph: Arc<Mutex<LnGraph>>,
    pub pays: Arc<RwLock<HashMap<String, String>>>,
    pub alias_peer_map: Arc<Mutex<HashMap<PublicKey, String>>>,
    pub pull_jobs: Arc<Mutex<HashSet<ShortChannelId>>>,
    pub push_jobs: Arc<Mutex<HashSet<ShortChannelId>>>,
    pub excepts_chans: Arc<Mutex<HashSet<ShortChannelId>>>,
    pub excepts_peers: Arc<Mutex<HashSet<PublicKey>>>,
    pub tempbans: Arc<Mutex<HashMap<ShortChannelId, u64>>>,
    pub job_state: Arc<Mutex<HashMap<ShortChannelId, Vec<JobState>>>>,
    pub blockheight: Arc<Mutex<u32>>,
    pub gossip_store_anns: Arc<Mutex<HashMap<ShortChannelId, ChannelAnnouncement>>>,
    pub gossip_store_amts: Arc<Mutex<HashMap<ShortChannelId, u64>>>,
}
impl PluginState {
    pub fn new(
        pubkey: PublicKey,
        rpc_path: PathBuf,
        sling_dir: PathBuf,
        network_dir: PathBuf,
        version: String,
    ) -> PluginState {
        PluginState {
            config: Arc::new(Mutex::new(Config::new(
                pubkey,
                rpc_path,
                sling_dir,
                network_dir,
                version,
            ))),
            peer_channels: Arc::new(Mutex::new(HashMap::new())),
            graph: Arc::new(Mutex::new(LnGraph::new())),
            pays: Arc::new(RwLock::new(HashMap::new())),
            alias_peer_map: Arc::new(Mutex::new(HashMap::new())),
            pull_jobs: Arc::new(Mutex::new(HashSet::new())),
            push_jobs: Arc::new(Mutex::new(HashSet::new())),
            excepts_chans: Arc::new(Mutex::new(HashSet::new())),
            excepts_peers: Arc::new(Mutex::new(HashSet::new())),
            tempbans: Arc::new(Mutex::new(HashMap::new())),
            job_state: Arc::new(Mutex::new(HashMap::new())),
            blockheight: Arc::new(Mutex::new(0)),
            gossip_store_anns: Arc::new(Mutex::new(HashMap::new())),
            gossip_store_amts: Arc::new(Mutex::new(HashMap::new())),
        }
    }
    pub async fn read_excepts(&self) -> Result<(), Error> {
        let sling_dir = self.config.lock().sling_dir.clone();
        let excepts_chan_file = sling_dir.join(EXCEPTS_CHANS_FILE_NAME);
        let excepts_peers_file = sling_dir.join(EXCEPTS_PEERS_FILE_NAME);
        let excepts_chan_file_content = fs::read_to_string(excepts_chan_file.clone()).await;
        let excepts_peers_file_content = fs::read_to_string(excepts_peers_file.clone()).await;

        create_sling_dir(&sling_dir).await?;

        *self.excepts_chans.lock() =
            PluginState::parse_excepts(excepts_chan_file_content, excepts_chan_file).await?;
        *self.excepts_peers.lock() =
            PluginState::parse_excepts(excepts_peers_file_content, excepts_peers_file).await?;
        Ok(())
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
                warn!(
                    "Could not open {}: {}. First time using sling? Creating new file.",
                    excepts_file.to_str().unwrap(),
                    e.to_string()
                );
                File::create(excepts_file.clone()).await?;
                excepts_tostring = Vec::new();
            }
        };

        for except in excepts_tostring {
            match T::from_str(&except) {
                Ok(id) => {
                    excepts.insert(id);
                }
                Err(_e) => warn!(
                    "excepts file contains invalid short_channel_id/node_id: {}",
                    except
                ),
            }
        }
        Ok(excepts)
    }
}

#[derive(Clone, Debug)]
pub struct Task {
    pub chan_id: ShortChannelId,
    pub task_id: u8,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub pubkey: PublicKey,
    pub rpc_path: PathBuf,
    pub sling_dir: PathBuf,
    pub network_dir: PathBuf,
    pub version: String,
    pub utf8: DynamicConfigOption<bool>,
    pub refresh_peers_interval: DynamicConfigOption<u64>,
    pub refresh_aliasmap_interval: DynamicConfigOption<u64>,
    pub refresh_gossmap_interval: DynamicConfigOption<u64>,
    pub reset_liquidity_interval: DynamicConfigOption<u64>,
    pub depleteuptopercent: DynamicConfigOption<f64>,
    pub depleteuptoamount: DynamicConfigOption<u64>,
    pub maxhops: DynamicConfigOption<u8>,
    pub candidates_min_age: DynamicConfigOption<u32>,
    pub paralleljobs: DynamicConfigOption<u8>,
    pub timeoutpay: DynamicConfigOption<u16>,
    pub max_htlc_count: DynamicConfigOption<u64>,
    pub stats_delete_failures_age: DynamicConfigOption<u64>,
    pub stats_delete_failures_size: DynamicConfigOption<u64>,
    pub stats_delete_successes_age: DynamicConfigOption<u64>,
    pub stats_delete_successes_size: DynamicConfigOption<u64>,
    pub cltv_delta: u32,
}
impl Config {
    pub fn new(
        pubkey: PublicKey,
        rpc_path: PathBuf,
        sling_dir: PathBuf,
        network_dir: PathBuf,
        version: String,
    ) -> Config {
        Config {
            pubkey,
            rpc_path,
            sling_dir,
            network_dir,
            version,
            utf8: DynamicConfigOption {
                name: OPT_UTF8,
                value: true,
            },
            refresh_peers_interval: DynamicConfigOption {
                name: OPT_REFRESH_PEERS_INTERVAL,
                value: 1,
            },
            refresh_aliasmap_interval: DynamicConfigOption {
                name: OPT_REFRESH_ALIASMAP_INTERVAL,
                value: 3600,
            },
            refresh_gossmap_interval: DynamicConfigOption {
                name: OPT_REFRESH_GOSSMAP_INTERVAL,
                value: 10,
            },
            reset_liquidity_interval: DynamicConfigOption {
                name: OPT_RESET_LIQUIDITY_INTERVAL,
                value: 360,
            },
            depleteuptopercent: DynamicConfigOption {
                name: OPT_DEPLETEUPTOPERCENT,
                value: 0.2,
            },
            depleteuptoamount: DynamicConfigOption {
                name: OPT_DEPLETEUPTOAMOUNT,
                value: 2_000_000_000,
            },
            maxhops: DynamicConfigOption {
                name: OPT_MAXHOPS,
                value: 8,
            },
            candidates_min_age: DynamicConfigOption {
                name: OPT_CANDIDATES_MIN_AGE,
                value: 0,
            },
            paralleljobs: DynamicConfigOption {
                name: OPT_PARALLELJOBS,
                value: 1,
            },
            timeoutpay: DynamicConfigOption {
                name: OPT_TIMEOUTPAY,
                value: 120,
            },
            max_htlc_count: DynamicConfigOption {
                name: OPT_MAX_HTLC_COUNT,
                value: 5,
            },
            stats_delete_failures_age: DynamicConfigOption {
                name: OPT_STATS_DELETE_FAILURES_AGE,
                value: 30,
            },
            stats_delete_failures_size: DynamicConfigOption {
                name: OPT_STATS_DELETE_FAILURES_SIZE,
                value: 10_000,
            },
            stats_delete_successes_age: DynamicConfigOption {
                name: OPT_STATS_DELETE_SUCCESSES_AGE,
                value: 30,
            },
            stats_delete_successes_size: DynamicConfigOption {
                name: OPT_STATS_DELETE_SUCCESSES_SIZE,
                value: 10_000,
            },
            cltv_delta: 144,
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

    fn statechange(&mut self, latest_state: JobMessage) {
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
    fn set_active(&mut self, active: bool) {
        self.active = active;
    }
    pub fn id(&self) -> u8 {
        self.id
    }
}

pub fn channel_jobstate_update(
    jobstates: Arc<Mutex<HashMap<ShortChannelId, Vec<JobState>>>>,
    task: &Task,
    latest_state: &JobMessage,
    active: bool,
    should_stop: bool,
) -> Result<(), Error> {
    let mut jobstates_lock = jobstates.lock();
    let jobstates = jobstates_lock.get_mut(&task.chan_id);
    let jobstate = if let Some(jss) = jobstates {
        if let Some(js) = jss.iter_mut().find(|jt| jt.id() == task.task_id) {
            js
        } else {
            return Err(anyhow!("channel_jobstate_update: Could not find task id"));
        }
    } else {
        return Err(anyhow!("channel_jobstate_update: Could not find scid"));
    };
    jobstate.statechange(*latest_state);

    if jobstate.should_stop() {
        if !active {
            jobstate.set_active(active);
        }
        return Ok(());
    }

    jobstate.set_active(active);
    if should_stop {
        jobstate.stop()
    }
    Ok(())
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

#[derive(Debug, Clone, Copy)]
pub struct DijkstraNode<'a> {
    pub score: u64,
    pub channel_state: &'a DirectedChannelState,
    pub short_channel_id: ShortChannelId,
    pub destination: PublicKey,
    pub hops: u8,
}
impl<'a> PartialEq for DijkstraNode<'a> {
    fn eq(&self, other: &Self) -> bool {
        self.score == other.score
            && self.hops == other.hops
            && self.channel_state.source == other.channel_state.source
            && self.channel_state.destination == other.channel_state.destination
            && self.short_channel_id == other.short_channel_id
            && self.destination == other.destination
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct DirectedChannel {
    pub short_channel_id: ShortChannelId,
    pub direction: u32,
}
impl Serialize for DirectedChannel {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("{}/{}", self.short_channel_id, self.direction))
    }
}

impl<'de> Deserialize<'de> for DirectedChannel {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let data = <&str>::deserialize(deserializer)?;
        let mut parts = data.splitn(2, '/');
        let short_channel_id = ShortChannelId::from_str(parts.next().unwrap())
            .map_err(|_| Error::custom("Could not parse short_channel_id"))?;
        let direction: u32 = parts
            .next()
            .ok_or_else(|| Error::custom("request must contain dash"))?
            .parse()
            .map_err(Error::custom)?;
        Ok(Self {
            short_channel_id,
            direction,
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct DirectedChannelState {
    pub source: PublicKey,
    pub destination: PublicKey,
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scid_alias: Option<ShortChannelId>,
    pub fee_per_millionth: u32,
    pub base_fee_millisatoshi: u32,
    pub htlc_maximum_msat: Amount,
    pub htlc_minimum_msat: Amount,
    pub amount_msat: Amount,
    pub delay: u32,
    pub last_update: u32,
    pub liquidity: u64,
    pub liquidity_age: u64,
}
impl DirectedChannelState {
    pub fn update(&mut self, channel_update: &ChannelUpdate) {
        self.active = channel_update.active;
        self.last_update = channel_update.last_update;
        self.base_fee_millisatoshi = channel_update.base_fee_millisatoshi;
        self.fee_per_millionth = channel_update.fee_per_millionth;
        self.delay = channel_update.delay;
        self.htlc_minimum_msat = channel_update.htlc_minimum_msat;
        self.htlc_maximum_msat = channel_update.htlc_maximum_msat;
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

#[derive(Debug, Serialize, Deserialize)]
pub struct LnGraph {
    pub graph: HashMap<PublicKey, HashMap<DirectedChannel, DirectedChannelState>>,
}
impl LnGraph {
    pub fn new() -> Self {
        LnGraph {
            graph: HashMap::new(),
        }
    }
    pub fn refresh_liquidity(&mut self, interval: u64) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut count = 0;
        for (_node, channels) in self.graph.iter_mut() {
            for channel_state in channels.values_mut() {
                if channel_state.liquidity_age <= now - interval * 60 {
                    channel_state.liquidity = Amount::msat(&channel_state.htlc_maximum_msat) / 2;
                    channel_state.liquidity_age = now;
                    count += 1;
                }
            }
        }
        info!("Reset liquidity belief on {} channels!", count);
    }
    pub fn get_channel(
        &self,
        source: &PublicKey,
        scid: &ShortChannelId,
    ) -> Result<&DirectedChannelState, Error> {
        if let Some(node_channels) = self.graph.get(source) {
            if let Some(dir_0) = node_channels.get(&DirectedChannel {
                short_channel_id: *scid,
                direction: 0,
            }) {
                Ok(dir_0)
            } else if let Some(dir_1) = node_channels.get(&DirectedChannel {
                short_channel_id: *scid,
                direction: 1,
            }) {
                Ok(dir_1)
            } else {
                Err(anyhow!("Channel {} not found in graph", scid))
            }
        } else {
            Err(anyhow!("Could not find channel in lngraph: {}", scid))
        }
    }

    pub fn edges(
        &self,
        keypair: &PublicKeyPair,
        exclude_graph: &ExcludeGraph,
        amount: &u64,
        candidatelist: &[ShortChannelId],
        tempbans: &HashMap<ShortChannelId, u64>,
    ) -> Vec<(&DirectedChannel, &DirectedChannelState)> {
        if let Some(node_channels) = self.graph.get(&keypair.other_pubkey) {
            let twow_ago = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                - 60 * 60 * 24 * 14;
            node_channels
                .iter()
                .filter(|(scid, chan_state)| {
                    chan_state.active
                        && chan_state.last_update >= (twow_ago as u32)
                        && !exclude_graph.exclude_chans.contains(&scid.short_channel_id)
                        && !tempbans.contains_key(&scid.short_channel_id)
                        && chan_state.liquidity >= *amount
                        && Amount::msat(&chan_state.htlc_minimum_msat) <= *amount
                        && Amount::msat(&chan_state.htlc_maximum_msat) >= *amount
                        && !exclude_graph.exclude_peers.contains(&chan_state.source)
                        && !exclude_graph
                            .exclude_peers
                            .contains(&chan_state.destination)
                        && if chan_state.source == keypair.my_pubkey
                            || chan_state.destination == keypair.my_pubkey
                        {
                            candidatelist.iter().any(|c| c == &scid.short_channel_id)
                        } else {
                            true
                        }
                })
                .collect::<Vec<(&DirectedChannel, &DirectedChannelState)>>()
        } else {
            Vec::<(&DirectedChannel, &DirectedChannelState)>::new()
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
