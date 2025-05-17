use std::{
    collections::{HashMap, HashSet},
    fmt::{self, Display, Formatter},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
};

use anyhow::{anyhow, Error};
use cln_rpc::{
    model::responses::{GetinfoResponse, ListpeerchannelsChannels},
    primitives::{Amount, PublicKey, ShortChannelId, ShortChannelIdDir},
};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize, Serializer};
use sling::Job;
use tabled::Tabled;
use tokio::{fs::OpenOptions, io::AsyncWriteExt};

use crate::gossip::{ChannelAnnouncement, ChannelUpdate};

pub const SUCCESSES_SUFFIX: &str = "successes.json";
pub const FAILURES_SUFFIX: &str = "failures.json";
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
    pub tempbans: Arc<Mutex<HashMap<ShortChannelId, u64>>>,
    pub tasks: Arc<Mutex<Tasks>>,
    pub blockheight: Arc<Mutex<u32>>,
    pub rpc_lock: Arc<tokio::sync::Mutex<()>>,
}
impl PluginState {
    pub fn new(config: Config, liquidity: HashMap<ShortChannelIdDir, Liquidity>) -> PluginState {
        PluginState {
            config: Arc::new(Mutex::new(config)),
            peer_channels: Arc::new(Mutex::new(HashMap::new())),
            graph: Arc::new(Mutex::new(LnGraph::new())),
            incomplete_channels: Arc::new(Mutex::new(IncompleteChannels::new())),
            liquidity: Arc::new(Mutex::new(liquidity)),
            pays: Arc::new(RwLock::new(HashMap::new())),
            alias_peer_map: Arc::new(Mutex::new(HashMap::new())),
            tempbans: Arc::new(Mutex::new(HashMap::new())),
            tasks: Arc::new(Mutex::new(Tasks::new())),
            blockheight: Arc::new(Mutex::new(0)),
            rpc_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Tasks {
    tasks: HashMap<ShortChannelId, HashMap<u16, Task>>,
}
impl Tasks {
    pub fn new() -> Tasks {
        Tasks {
            tasks: HashMap::new(),
        }
    }
    pub fn insert_task(
        &mut self,
        scid: ShortChannelId,
        task_id: u16,
        task: Task,
    ) -> Result<(), anyhow::Error> {
        match self.tasks.entry(scid) {
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(HashMap::from([(task_id, task)]));
                Ok(())
            }
            std::collections::hash_map::Entry::Occupied(mut e) => {
                match e.get_mut().entry(task_id) {
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(task);
                        Ok(())
                    }
                    std::collections::hash_map::Entry::Occupied(_e) => {
                        Err(anyhow::anyhow!("task already exists"))
                    }
                }
            }
        }
    }
    pub fn is_any_active(&self, scid: &ShortChannelId) -> bool {
        if let Some(tasks) = self.tasks.get(scid) {
            tasks.values().any(|t| t.is_active())
        } else {
            false
        }
    }
    pub fn set_active(&mut self, task_ident: &TaskIdentifier, active: bool) {
        if let Some(tasks) = self.tasks.get_mut(&task_ident.get_chan_id()) {
            if let Some(task) = tasks.get_mut(&task_ident.get_task_id()) {
                task.set_active(active);
            }
        }
    }
    pub fn set_state(&mut self, task_ident: &TaskIdentifier, state: JobMessage) {
        if let Some(tasks) = self.tasks.get_mut(&task_ident.get_chan_id()) {
            if let Some(task) = tasks.get_mut(&task_ident.get_task_id()) {
                task.set_state(state);
            }
        }
    }
    pub fn get_all_tasks_mut(&mut self) -> &mut HashMap<ShortChannelId, HashMap<u16, Task>> {
        &mut self.tasks
    }
    pub fn get_all_tasks(&self) -> &HashMap<ShortChannelId, HashMap<u16, Task>> {
        &self.tasks
    }
    pub fn get_scid_tasks_mut(&mut self, scid: &ShortChannelId) -> Option<&mut HashMap<u16, Task>> {
        self.tasks.get_mut(scid)
    }
    pub fn get_scid_tasks(&self, scid: &ShortChannelId) -> Option<&HashMap<u16, Task>> {
        self.tasks.get(scid)
    }
    pub fn get_parallelbans(
        &self,
        scid: ShortChannelId,
    ) -> Result<HashSet<ShortChannelIdDir>, anyhow::Error> {
        if let Some(tasks) = self.tasks.get(&scid) {
            Ok(tasks.values().filter_map(|t| t.parallel_ban).collect())
        } else {
            Err(anyhow::anyhow!("no tasks found for scid"))
        }
    }
    pub fn get_task(&self, task_ident: &TaskIdentifier) -> Option<&Task> {
        if let Some(tasks) = self.tasks.get(&task_ident.get_chan_id()) {
            tasks.get(&task_ident.get_task_id())
        } else {
            None
        }
    }
    pub fn get_task_mut(&mut self, task_ident: &TaskIdentifier) -> Option<&mut Task> {
        if let Some(tasks) = self.tasks.get_mut(&task_ident.get_chan_id()) {
            tasks.get_mut(&task_ident.get_task_id())
        } else {
            None
        }
    }
    pub fn remove_all_tasks(&mut self) {
        self.tasks.clear();
    }
    pub fn remove_task(&mut self, scid: &ShortChannelId) {
        self.tasks.remove(scid);
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, PartialOrd)]
pub struct PubKeyBytes([u8; 33]);

impl PubKeyBytes {
    pub fn new(bytes: [u8; 33]) -> PubKeyBytes {
        PubKeyBytes(bytes)
    }
    pub fn from_pubkey(pubkey: &PublicKey) -> PubKeyBytes {
        PubKeyBytes(pubkey.serialize())
    }
    pub fn from_str(s: &str) -> Result<PubKeyBytes, anyhow::Error> {
        Ok(PubKeyBytes(PublicKey::from_str(s)?.serialize()))
    }
    pub fn to_pubkey(self) -> PublicKey {
        PublicKey::from_slice(&self.0).unwrap()
    }
}

impl Display for PubKeyBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", PublicKey::from_slice(&self.0).unwrap())
    }
}

impl Hash for PubKeyBytes {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl Serialize for PubKeyBytes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut hex = String::with_capacity(66);
        for byte in self.0.iter() {
            use std::fmt::Write;
            write!(&mut hex, "{:02x}", byte).unwrap();
        }
        serializer.serialize_str(&hex)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskIdentifier {
    short_channel_id: ShortChannelId,
    task_id: u16,
}
impl TaskIdentifier {
    pub fn new(short_channel_id: ShortChannelId, task_id: u16) -> TaskIdentifier {
        TaskIdentifier {
            short_channel_id,
            task_id,
        }
    }
    pub fn get_chan_id(&self) -> ShortChannelId {
        self.short_channel_id
    }
    pub fn get_task_id(&self) -> u16 {
        self.task_id
    }
}
impl Display for TaskIdentifier {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.short_channel_id, self.task_id)
    }
}

#[derive(Clone, Debug)]
pub struct Task {
    task_ident: TaskIdentifier,
    latest_state: JobMessage,
    active: bool,
    should_stop: bool,
    once: bool,
    pub parallel_ban: Option<ShortChannelIdDir>,
    pub other_pubkey: PubKeyBytes,
}
impl Display for Task {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.task_ident)
    }
}

impl Task {
    pub fn new(
        short_channel_id: ShortChannelId,
        task_id: u16,
        latest_state: JobMessage,
        once: bool,
        other_pubkey: PubKeyBytes,
    ) -> Self {
        Task {
            latest_state,
            active: true,
            should_stop: false,
            once,
            other_pubkey,
            parallel_ban: None,
            task_ident: TaskIdentifier::new(short_channel_id, task_id),
        }
    }

    pub fn get_state(&self) -> JobMessage {
        self.latest_state
    }
    pub fn set_state(&mut self, state: JobMessage) {
        self.latest_state = state;
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
        if active {
            self.should_stop = false;
        }
    }
    pub fn get_chan_id(&self) -> ShortChannelId {
        self.task_ident.get_chan_id()
    }
    pub fn get_identifier(&self) -> &TaskIdentifier {
        &self.task_ident
    }
    pub fn is_once(&self) -> bool {
        self.once
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub pubkey: PublicKey,
    pub pubkey_bytes: PubKeyBytes,
    pub rpc_path: PathBuf,
    pub sling_dir: PathBuf,
    pub version: String,
    pub network: String,
    pub refresh_aliasmap_interval: u64,
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
    pub exclude_chans_pull: HashSet<ShortChannelId>,
    pub exclude_chans_push: HashSet<ShortChannelId>,
    pub exclude_peers: HashSet<PubKeyBytes>,
}
impl Config {
    pub fn new(
        getinfo: GetinfoResponse,
        rpc_path: PathBuf,
        sling_dir: PathBuf,
        exclude_chans_pull: HashSet<ShortChannelId>,
        exclude_chans_push: HashSet<ShortChannelId>,
        exclude_peers: HashSet<PubKeyBytes>,
    ) -> Config {
        Config {
            pubkey: getinfo.id,
            pubkey_bytes: PubKeyBytes::from_pubkey(&getinfo.id),
            rpc_path,
            sling_dir,
            version: getinfo.version,
            network: getinfo.network,
            refresh_aliasmap_interval: 3600,
            reset_liquidity_interval: 360,
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
            exclude_chans_pull,
            exclude_chans_push,
            exclude_peers,
        }
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
    NotStarted,
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
            JobMessage::NotStarted => write!(f, "NotStarted"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DijkstraNode {
    pub score: u64,
    pub channel_state: ShortChannelIdDirState,
    pub short_channel_id: ShortChannelId,
    pub destination: PubKeyBytes,
    pub hops: u8,
}
impl PartialEq for DijkstraNode {
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
    pub source: PubKeyBytes,
    pub destination: PubKeyBytes,
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
    pub source: Option<PubKeyBytes>,
    destination: Option<PubKeyBytes>,
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
    node_1: PubKeyBytes,
    node_2: PubKeyBytes,
) -> Result<(PubKeyBytes, PubKeyBytes), Error> {
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
    graph: HashMap<PubKeyBytes, HashSet<ShortChannelIdDir>>,
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
        source: &PubKeyBytes,
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
        source: &PubKeyBytes,
        two_weeks_ago: u32,
        actual_candidates: &[ShortChannelId],
        config: &Config,
        job: &Job,
        excepts: &[ShortChannelIdDir],
        liquidity: &HashMap<ShortChannelIdDir, Liquidity>,
    ) -> Vec<(&ShortChannelIdDir, &ShortChannelIdDirState)> {
        let mut result = Vec::new();
        if let Some(node_channels) = self.graph.get(source) {
            for dir_chan in node_channels {
                if let Some(dir_chan_state) = self.channels.get(dir_chan) {
                    if dir_chan_state.active
                        && dir_chan_state.last_update >= two_weeks_ago
                        && !excepts.contains(dir_chan)
                        && Amount::msat(&dir_chan_state.htlc_minimum_msat) <= job.amount_msat
                        && Amount::msat(&dir_chan_state.htlc_maximum_msat) >= job.amount_msat
                        && !config.exclude_peers.contains(&dir_chan_state.source)
                        && !config.exclude_peers.contains(&dir_chan_state.destination)
                        && if dir_chan_state.source == config.pubkey_bytes
                            || dir_chan_state.destination == config.pubkey_bytes
                        {
                            actual_candidates
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
            .open(sling_dir.join(chan_id.to_string() + "_" + SUCCESSES_SUFFIX))
            .await?;
        file.write_all(format!("{}\n", serialized).as_bytes())
            .await?;
        Ok(())
    }

    pub async fn read_from_files(
        sling_dir: &Path,
        search_scid: Option<ShortChannelId>,
    ) -> Result<HashMap<ShortChannelId, Vec<SuccessReb>>, Error> {
        let mut result = HashMap::new();
        let mut read_dir = tokio::fs::read_dir(sling_dir).await?;
        while let Some(file) = read_dir.next_entry().await? {
            let file_name_os = file.file_name();
            let file_name = if let Some(f_n) = file_name_os.to_str() {
                f_n
            } else {
                continue;
            };
            let file_path = file.path();
            let file_extension = if let Some(f_e) = file_path.extension() {
                if let Some(f_e_str) = f_e.to_str() {
                    f_e_str
                } else {
                    continue;
                }
            } else {
                continue;
            };
            let (scid_str, suffix) = if let Some(split) = file_name.split_once('_') {
                split
            } else {
                continue;
            };
            let scid = if let Ok(id) = ShortChannelId::from_str(scid_str) {
                id
            } else {
                continue;
            };
            if let Some(s) = search_scid {
                if s != scid {
                    continue;
                }
            }

            if suffix == SUCCESSES_SUFFIX && file_extension == "json" {
                log::debug!("Reading success file: {}", file.path().display());
                let contents = tokio::fs::read_to_string(&file_path).await?;
                for line in contents.lines() {
                    let reb: SuccessReb = if let Ok(r) = serde_json::from_str(line) {
                        r
                    } else {
                        continue;
                    };
                    match result.entry(scid) {
                        std::collections::hash_map::Entry::Vacant(e) => {
                            e.insert(vec![reb]);
                        }
                        std::collections::hash_map::Entry::Occupied(mut e) => {
                            e.get_mut().push(reb);
                        }
                    }
                }
                if let Some(rebs) = result.remove(&scid) {
                    if rebs.is_empty() {
                        log::debug!("Deleting empty success file: {}", file.path().display());
                        tokio::fs::remove_file(file_path).await?;
                    } else {
                        result.insert(scid, rebs);
                    }
                } else {
                    log::debug!("Deleting empty success file: {}", file.path().display());
                    tokio::fs::remove_file(file_path).await?;
                }
            }
        }
        Ok(result)
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
            .open(sling_dir.join(chan_id.to_string() + "_" + FAILURES_SUFFIX))
            .await?;
        file.write_all(format!("{}\n", serialized).as_bytes())
            .await?;
        Ok(())
    }

    pub async fn read_from_files(
        sling_dir: &Path,
        search_scid: Option<ShortChannelId>,
    ) -> Result<HashMap<ShortChannelId, Vec<FailureReb>>, Error> {
        let mut result = HashMap::new();
        let mut read_dir = tokio::fs::read_dir(sling_dir).await?;
        while let Some(file) = read_dir.next_entry().await? {
            let file_name_os = file.file_name();
            let file_name = if let Some(f_n) = file_name_os.to_str() {
                f_n
            } else {
                continue;
            };
            let file_path = file.path();
            let file_extension = if let Some(f_e) = file_path.extension() {
                if let Some(f_e_str) = f_e.to_str() {
                    f_e_str
                } else {
                    continue;
                }
            } else {
                continue;
            };
            let (scid_str, suffix) = if let Some(split) = file_name.split_once('_') {
                split
            } else {
                continue;
            };
            let scid = if let Ok(id) = ShortChannelId::from_str(scid_str) {
                id
            } else {
                continue;
            };
            if let Some(s) = search_scid {
                if s != scid {
                    continue;
                }
            }

            if suffix == FAILURES_SUFFIX && file_extension == "json" {
                log::debug!("Reading failure file: {}", file.path().display());
                let contents = tokio::fs::read_to_string(&file_path).await?;
                for line in contents.lines() {
                    let reb: FailureReb = if let Ok(r) = serde_json::from_str(line) {
                        r
                    } else {
                        continue;
                    };
                    match result.entry(scid) {
                        std::collections::hash_map::Entry::Vacant(e) => {
                            e.insert(vec![reb]);
                        }
                        std::collections::hash_map::Entry::Occupied(mut e) => {
                            e.get_mut().push(reb);
                        }
                    }
                }
                if let Some(rebs) = result.remove(&scid) {
                    if rebs.is_empty() {
                        log::debug!("Deleting empty failure file: {}", file.path().display());
                        tokio::fs::remove_file(file_path).await?;
                    } else {
                        result.insert(scid, rebs);
                    }
                } else {
                    log::debug!("Deleting empty failure file: {}", file.path().display());
                    tokio::fs::remove_file(file_path).await?;
                }
            }
        }
        Ok(result)
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
