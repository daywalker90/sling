use std::{
    collections::{HashMap, HashSet},
    fmt::{self, Display, Formatter},
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
};

use anyhow::{anyhow, Error};
use cln_rpc::{
    model::responses::ListpeerchannelsChannels,
    primitives::{Amount, PublicKey, ShortChannelId, ShortChannelIdDir},
};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use sling::Job;
use tabled::Tabled;
use tokio::{
    fs::{self, File, OpenOptions},
    io::{self, AsyncWriteExt},
};

use crate::{
    create_sling_dir,
    gossip::{ChannelAnnouncement, ChannelUpdate},
};

pub const SUCCESSES_SUFFIX: &str = "_successes.json";
pub const FAILURES_SUFFIX: &str = "_failures.json";
pub const NO_ALIAS_SET: &str = "NO_ALIAS_SET";

pub const PLUGIN_NAME: &str = "sling";
pub const LIQUIDITY_FILE_NAME: &str = "liquidity.json";
pub const JOB_FILE_NAME: &str = "jobs.json";
pub const EXCEPTS_CHANS_FILE_NAME: &str = "excepts.json";
pub const EXCEPTS_PEERS_FILE_NAME: &str = "excepts_peers.json";

#[derive(Clone)]
pub struct PluginState {
    pub config: Arc<Mutex<Config>>,
    pub peer_channels: Arc<Mutex<HashMap<ShortChannelId, ListpeerchannelsChannels>>>,
    pub graph: Arc<Mutex<LnGraph>>,
    pub incomplete_channels: Arc<Mutex<IncompleteChannels>>,
    pub liquidity: Arc<Mutex<HashMap<ShortChannelIdDir, Liquidity>>>,
    pub pays: Arc<RwLock<HashMap<String, String>>>,
    pub alias_peer_map: Arc<Mutex<HashMap<PublicKey, String>>>,
    pub pull_jobs: Arc<Mutex<HashSet<ShortChannelId>>>,
    pub push_jobs: Arc<Mutex<HashSet<ShortChannelId>>>,
    pub excepts_chans: Arc<Mutex<HashSet<ShortChannelId>>>,
    pub excepts_peers: Arc<Mutex<HashSet<PublicKey>>>,
    pub tempbans: Arc<Mutex<HashMap<ShortChannelId, u64>>>,
    pub parrallel_bans: Arc<Mutex<HashMap<ShortChannelId, HashMap<u16, ShortChannelIdDir>>>>,
    pub job_state: Arc<Mutex<HashMap<ShortChannelId, Vec<JobState>>>>,
    pub blockheight: Arc<Mutex<u32>>,
}
impl PluginState {
    pub fn new(
        pubkey: PublicKey,
        rpc_path: PathBuf,
        sling_dir: PathBuf,
        version: String,
    ) -> PluginState {
        PluginState {
            config: Arc::new(Mutex::new(Config::new(
                pubkey, rpc_path, sling_dir, version,
            ))),
            peer_channels: Arc::new(Mutex::new(HashMap::new())),
            graph: Arc::new(Mutex::new(LnGraph::new())),
            incomplete_channels: Arc::new(Mutex::new(IncompleteChannels::new())),
            liquidity: Arc::new(Mutex::new(HashMap::new())),
            pays: Arc::new(RwLock::new(HashMap::new())),
            alias_peer_map: Arc::new(Mutex::new(HashMap::new())),
            pull_jobs: Arc::new(Mutex::new(HashSet::new())),
            push_jobs: Arc::new(Mutex::new(HashSet::new())),
            excepts_chans: Arc::new(Mutex::new(HashSet::new())),
            excepts_peers: Arc::new(Mutex::new(HashSet::new())),
            tempbans: Arc::new(Mutex::new(HashMap::new())),
            parrallel_bans: Arc::new(Mutex::new(HashMap::new())),
            job_state: Arc::new(Mutex::new(HashMap::new())),
            blockheight: Arc::new(Mutex::new(0)),
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
                log::warn!(
                    "Could not open {}: {}. First time using sling? Creating new file.",
                    excepts_file.to_str().unwrap(),
                    e
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
                Err(_e) => log::warn!(
                    "excepts file contains invalid short_channel_id/node_id: {}",
                    except
                ),
            }
        }
        Ok(excepts)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Copy)]
pub struct Task {
    pub chan_id: ShortChannelId,
    pub task_id: u16,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub pubkey: PublicKey,
    pub rpc_path: PathBuf,
    pub sling_dir: PathBuf,
    pub version: String,
    pub refresh_peers_interval: u64,
    pub refresh_aliasmap_interval: u64,
    pub refresh_gossmap_interval: u64,
    pub reset_liquidity_interval: u64,
    pub depleteuptopercent: f64,
    pub depleteuptoamount: u64,
    pub maxhops: u8,
    pub candidates_min_age: u32,
    pub paralleljobs: u16,
    pub timeoutpay: u16,
    pub max_htlc_count: u64,
    pub stats_delete_failures_age: u64,
    pub stats_delete_failures_size: u64,
    pub stats_delete_successes_age: u64,
    pub stats_delete_successes_size: u64,
    pub cltv_delta: u32,
    pub at_or_above_24_11: bool,
    pub inform_layers: Vec<String>,
}
impl Config {
    pub fn new(
        pubkey: PublicKey,
        rpc_path: PathBuf,
        sling_dir: PathBuf,
        version: String,
    ) -> Config {
        Config {
            pubkey,
            rpc_path,
            sling_dir,
            version,
            refresh_peers_interval: 1,
            refresh_aliasmap_interval: 3600,
            refresh_gossmap_interval: 10,
            reset_liquidity_interval: 60,
            depleteuptopercent: 0.2,
            depleteuptoamount: 2_000_000_000,
            maxhops: 8,
            candidates_min_age: 0,
            paralleljobs: 1,
            timeoutpay: 120,
            max_htlc_count: 5,
            stats_delete_failures_age: 30,
            stats_delete_failures_size: 10_000,
            stats_delete_successes_age: 30,
            stats_delete_successes_size: 10_000,
            cltv_delta: 144,
            at_or_above_24_11: false,
            inform_layers: vec!["xpay".to_string()],
        }
    }
}

#[derive(Debug, Clone)]
pub struct JobState {
    latest_state: JobMessage,
    active: bool,
    should_stop: bool,
    id: u16,
    once: bool,
}
impl JobState {
    pub fn new(latest_state: JobMessage, id: u16, once: bool) -> Self {
        JobState {
            latest_state,
            active: true,
            should_stop: false,
            id,
            once,
        }
    }
    pub fn missing() -> Self {
        JobState {
            latest_state: JobMessage::NoJob,
            active: false,
            should_stop: false,
            id: 0,
            once: false,
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
    pub fn id(&self) -> u16 {
        self.id
    }
    pub fn is_once(&self) -> bool {
        self.once
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
    pub channel_state: &'a ShortChannelIdDirState,
    pub short_channel_id: ShortChannelId,
    pub destination: PublicKey,
    pub hops: u8,
}
impl PartialEq for DijkstraNode<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.score == other.score
            && self.hops == other.hops
            && self.channel_state.source == other.channel_state.source
            && self.channel_state.destination == other.channel_state.destination
            && self.short_channel_id == other.short_channel_id
            && self.destination == other.destination
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Liquidity {
    pub liquidity_msat: u64,
    pub liquidity_age: u64,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ShortChannelIdDirState {
    pub source: PublicKey,
    pub destination: PublicKey,
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scid_alias: Option<ShortChannelId>,
    pub fee_per_millionth: u32,
    pub base_fee_millisatoshi: u32,
    pub htlc_maximum_msat: Amount,
    pub htlc_minimum_msat: Amount,
    pub delay: u32,
    pub last_update: u32,
    pub private: bool,
}
impl ShortChannelIdDirState {
    pub fn update(&mut self, channel_update: ChannelUpdate) {
        self.active = channel_update.active;
        self.last_update = channel_update.last_update;
        self.base_fee_millisatoshi = channel_update.base_fee_millisatoshi;
        self.fee_per_millionth = channel_update.fee_per_millionth;
        self.delay = channel_update.delay;
        self.htlc_minimum_msat = channel_update.htlc_minimum_msat;
        self.htlc_maximum_msat = channel_update.htlc_maximum_msat;
    }
}

#[derive(Debug, Serialize)]
pub struct ShortChannelIdDirStateBuilder {
    pub source: Option<PublicKey>,
    destination: Option<PublicKey>,
    active: Option<bool>,
    scid_alias: Option<ShortChannelId>,
    fee_per_millionth: Option<u32>,
    base_fee_millisatoshi: Option<u32>,
    htlc_maximum_msat: Option<Amount>,
    htlc_minimum_msat: Option<Amount>,
    delay: Option<u32>,
    last_update: Option<u32>,
    private: Option<bool>,
}
pub enum BuildResult {
    Success(ShortChannelIdDirState),
    Failure(ShortChannelIdDirStateBuilder),
}
impl ShortChannelIdDirStateBuilder {
    pub fn new() -> Self {
        Self {
            source: None,
            destination: None,
            active: None,
            scid_alias: None,
            fee_per_millionth: None,
            base_fee_millisatoshi: None,
            htlc_maximum_msat: None,
            htlc_minimum_msat: None,
            delay: None,
            last_update: None,
            private: Some(false),
        }
    }
    pub fn has_announcement(&self) -> bool {
        self.source.is_some()
    }
    pub fn add_announcement(
        &mut self,
        direction: u32,
        announcement: ChannelAnnouncement,
    ) -> Result<&mut Self, anyhow::Error> {
        let (source, destination) =
            get_node_order(direction, announcement.source, announcement.destination)?;
        self.source = Some(source);
        self.destination = Some(destination);
        Ok(self)
    }

    pub fn add_update(&mut self, update: ChannelUpdate) -> &mut Self {
        self.active = Some(update.active);
        self.last_update = Some(update.last_update);
        self.base_fee_millisatoshi = Some(update.base_fee_millisatoshi);
        self.fee_per_millionth = Some(update.fee_per_millionth);
        self.delay = Some(update.delay);
        self.htlc_minimum_msat = Some(update.htlc_minimum_msat);
        self.htlc_maximum_msat = Some(update.htlc_maximum_msat);
        self
    }

    pub fn build(self) -> BuildResult {
        let htlc_maximum_msat = match self.htlc_maximum_msat {
            Some(v) => v,
            None => return BuildResult::Failure(self),
        };
        let source = match self.source {
            Some(v) => v,
            None => return BuildResult::Failure(self),
        };
        let destination = match self.destination {
            Some(v) => v,
            None => return BuildResult::Failure(self),
        };
        let active = match self.active {
            Some(v) => v,
            None => return BuildResult::Failure(self),
        };
        let fee_per_millionth = match self.fee_per_millionth {
            Some(v) => v,
            None => return BuildResult::Failure(self),
        };
        let base_fee_millisatoshi = match self.base_fee_millisatoshi {
            Some(v) => v,
            None => return BuildResult::Failure(self),
        };
        let htlc_minimum_msat = match self.htlc_minimum_msat {
            Some(v) => v,
            None => return BuildResult::Failure(self),
        };
        let delay = match self.delay {
            Some(v) => v,
            None => return BuildResult::Failure(self),
        };
        let last_update = match self.last_update {
            Some(v) => v,
            None => return BuildResult::Failure(self),
        };
        let private = self.private.unwrap_or(false);
        let scid_alias = self.scid_alias;

        BuildResult::Success(ShortChannelIdDirState {
            source,
            destination,
            active,
            scid_alias,
            fee_per_millionth,
            base_fee_millisatoshi,
            htlc_maximum_msat,
            htlc_minimum_msat,
            delay,
            last_update,
            private,
        })
    }
}

fn get_node_order(
    direction: u32,
    node_1: PublicKey,
    node_2: PublicKey,
) -> Result<(PublicKey, PublicKey), Error> {
    if direction == 0 {
        if node_1 < node_2 {
            Ok((node_1, node_2))
        } else {
            Ok((node_2, node_1))
        }
    } else if direction == 1 {
        if node_1 < node_2 {
            Ok((node_2, node_1))
        } else {
            Ok((node_1, node_2))
        }
    } else {
        Err(anyhow!("gossip_reader: invalid direction:{}", direction))
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

#[derive(Debug, Serialize)]
pub struct IncompleteChannels {
    incomplete_channels: HashMap<ShortChannelIdDir, ShortChannelIdDirStateBuilder>,
    updated_channels: HashSet<ShortChannelIdDir>,
}
impl IncompleteChannels {
    pub fn new() -> Self {
        IncompleteChannels {
            incomplete_channels: HashMap::new(),
            updated_channels: HashSet::new(),
        }
    }
    pub fn len(&self) -> usize {
        self.incomplete_channels.len()
    }
    pub fn get_mut(
        &mut self,
        scid_dir: &ShortChannelIdDir,
    ) -> Option<&mut ShortChannelIdDirStateBuilder> {
        if let Some(state) = self.incomplete_channels.get_mut(scid_dir) {
            self.updated_channels.insert(*scid_dir);
            Some(state)
        } else {
            None
        }
    }
    pub fn insert(
        &mut self,
        scid_dir: ShortChannelIdDir,
        dir_chan_state: ShortChannelIdDirStateBuilder,
    ) -> Option<ShortChannelIdDirStateBuilder> {
        self.updated_channels.insert(scid_dir);
        self.incomplete_channels.insert(scid_dir, dir_chan_state)
    }
    pub fn remove(
        &mut self,
        scid_dir: &ShortChannelIdDir,
    ) -> Option<ShortChannelIdDirStateBuilder> {
        self.updated_channels.remove(scid_dir);
        self.incomplete_channels.remove(scid_dir)
    }
    pub fn update_graph(&mut self, graph: &mut LnGraph) {
        let mut count_built = 0;
        for updated_chan in self.updated_channels.iter() {
            if let Some(state) = self.incomplete_channels.remove(updated_chan) {
                match state.build() {
                    BuildResult::Success(state) => {
                        graph.insert(*updated_chan, state);
                        self.incomplete_channels.remove(updated_chan);
                        count_built += 1;
                    }
                    BuildResult::Failure(builder) => {
                        self.incomplete_channels.insert(*updated_chan, builder);
                    }
                }
            }
        }
        log::debug!(
            "read_gossip_file: built {}/{} new channels",
            count_built,
            self.updated_channels.len()
        );
        self.updated_channels.clear();
    }
}

#[derive(Debug, Serialize)]
pub struct LnGraph {
    channels: HashMap<ShortChannelIdDir, ShortChannelIdDirState>,
    graph: HashMap<PublicKey, HashSet<ShortChannelIdDir>>,
}
impl LnGraph {
    pub fn new() -> Self {
        LnGraph {
            channels: HashMap::with_capacity(70000),
            graph: HashMap::with_capacity(7000),
        }
    }

    pub fn node_count(&self) -> usize {
        self.graph.len()
    }

    pub fn is_empty(&self) -> bool {
        self.graph.is_empty()
    }

    pub fn public_channel_count(&self) -> usize {
        self.channels.iter().filter(|(_, v)| !v.private).count()
    }

    pub fn private_channel_count(&self) -> usize {
        self.channels.iter().filter(|(_, v)| v.private).count()
    }

    pub fn retain<F>(&mut self, predicate: F)
    where
        F: Fn(&ShortChannelIdDir, &ShortChannelIdDirState) -> bool,
    {
        let keys_to_remove: Vec<ShortChannelIdDir> = self
            .channels
            .iter()
            .filter_map(|(key, value)| {
                if !predicate(key, value) {
                    Some(*key)
                } else {
                    None
                }
            })
            .collect();

        for key in keys_to_remove {
            self.remove(&key);
        }
    }

    pub fn insert(
        &mut self,
        scid_dir: ShortChannelIdDir,
        dir_chan_state: ShortChannelIdDirState,
    ) -> Option<ShortChannelIdDirState> {
        self.graph
            .entry(dir_chan_state.source)
            .or_default()
            .insert(scid_dir);

        self.channels.insert(scid_dir, dir_chan_state)
    }

    pub fn remove(&mut self, scid_dir: &ShortChannelIdDir) -> Option<ShortChannelIdDirState> {
        if let Some(value) = self.channels.remove(scid_dir) {
            self.graph
                .get_mut(&value.source)
                .map(|keys| keys.remove(scid_dir));
            if self
                .graph
                .get(&value.source)
                .is_some_and(|keys| keys.is_empty())
            {
                self.graph.remove(&value.source);
            }
            Some(value)
        } else {
            None
        }
    }

    pub fn has_announcement(
        &self,
        scid_dir: &ShortChannelIdDir,
        chan_announcement: &ChannelAnnouncement,
    ) -> Result<bool, anyhow::Error> {
        let (source, _destination) = get_node_order(
            scid_dir.direction,
            chan_announcement.source,
            chan_announcement.destination,
        )?;
        if let Some(node_channels) = self.graph.get(&source) {
            if node_channels.get(scid_dir).is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn get_state_mut_direction(
        &mut self,
        scid_dir: ShortChannelIdDir,
    ) -> Option<&mut ShortChannelIdDirState> {
        self.channels.get_mut(&scid_dir)
    }

    pub fn get_state_no_direction(
        &self,
        source: &PublicKey,
        scid: &ShortChannelId,
    ) -> Result<(ShortChannelIdDir, &ShortChannelIdDirState), Error> {
        let dir_chan_0 = ShortChannelIdDir {
            short_channel_id: *scid,
            direction: 0,
        };
        let dir_chan_1 = ShortChannelIdDir {
            short_channel_id: *scid,
            direction: 1,
        };
        if let Some(node_channels) = self.graph.get(source) {
            if let Some(dir_0) = node_channels.get(&dir_chan_0) {
                if let Some(state) = self.channels.get(dir_0) {
                    return Ok((dir_chan_0, state));
                }
            } else if let Some(dir_1) = node_channels.get(&dir_chan_1) {
                if let Some(state) = self.channels.get(dir_1) {
                    return Ok((dir_chan_1, state));
                }
            }
        }
        Err(anyhow!("Could not find channel in lngraph: {}", scid))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn edges(
        &self,
        two_weeks_ago: u32,
        keypair: &PublicKeyPair,
        exclude_graph: &ExcludeGraph,
        job: &Job,
        candidatelist: &[ShortChannelId],
        tempbans: &HashMap<ShortChannelId, u64>,
        parallel_bans: &[ShortChannelIdDir],
        liquidity: &HashMap<ShortChannelIdDir, Liquidity>,
    ) -> Vec<(&ShortChannelIdDir, &ShortChannelIdDirState)> {
        let mut result = Vec::new();
        if let Some(node_channels) = self.graph.get(&keypair.other_pubkey) {
            for dir_chan in node_channels {
                if let Some(dir_chan_state) = self.channels.get(dir_chan) {
                    if dir_chan_state.active
                        && dir_chan_state.last_update >= two_weeks_ago
                        && !exclude_graph
                            .exclude_chans
                            .contains(&dir_chan.short_channel_id)
                        && !tempbans.contains_key(&dir_chan.short_channel_id)
                        && !parallel_bans.contains(dir_chan)
                        && Amount::msat(&dir_chan_state.htlc_minimum_msat) <= job.amount_msat
                        && Amount::msat(&dir_chan_state.htlc_maximum_msat) >= job.amount_msat
                        && !exclude_graph.exclude_peers.contains(&dir_chan_state.source)
                        && !exclude_graph
                            .exclude_peers
                            .contains(&dir_chan_state.destination)
                        && if dir_chan_state.source == keypair.my_pubkey
                            || dir_chan_state.destination == keypair.my_pubkey
                        {
                            candidatelist
                                .iter()
                                .any(|c| c == &dir_chan.short_channel_id)
                        } else {
                            true
                        }
                        && if let Some(liq) = liquidity.get(dir_chan) {
                            liq.liquidity_msat >= job.amount_msat
                        } else {
                            dir_chan_state.htlc_maximum_msat.msat() / 2 >= job.amount_msat
                        }
                    {
                        result.push((dir_chan, dir_chan_state));
                    }
                }
            }
            result
        } else {
            Vec::<(&ShortChannelIdDir, &ShortChannelIdDirState)>::new()
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

#[derive(Debug, Serialize, Tabled)]
pub struct StatSummary {
    pub alias: String,
    pub scid: ShortChannelId,
    pub pubkey: PublicKey,
    #[tabled(skip)]
    pub status: Vec<String>,
    #[serde(skip_serializing)]
    pub status_str: String,
    pub rebamount: String,
    pub w_feeppm: u64,
    pub last_route_taken: String,
    pub last_success_reb: String,
}
