use std::{
    collections::HashMap,
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
    str::FromStr,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Error};
use bitcoin::secp256k1::PublicKey;
use cln_plugin::Plugin;
use cln_rpc::primitives::{Amount, ShortChannelId};
use log::{debug, warn};

use crate::{DirectedChannel, DirectedChannelState, PluginState};

#[derive(Debug, Clone)]
pub struct ChannelUpdate {
    pub direction: u32,
    // pub message_flags: u8,
    // pub channel_flags: u8,
    pub active: bool,
    pub last_update: u32,
    pub base_fee_millisatoshi: u32,
    pub fee_per_millionth: u32,
    pub delay: u32,
    pub htlc_minimum_msat: Amount,
    pub htlc_maximum_msat: Amount,
}

#[derive(Debug, Clone)]
pub struct ChannelAnnouncement {
    pub source: PublicKey,
    pub destination: PublicKey,
    // pub features: String,
}

pub async fn read_gossip_store(plugin: Plugin<PluginState>, offset: &mut u64) -> Result<(), Error> {
    let now = Instant::now();
    debug!("gossip_reader: offset:{}", offset);
    let is_start_up = *offset == 0;

    let file = File::open(Path::new(&plugin.configuration().lightning_dir).join("gossip_store"))?;
    let mut reader = BufReader::new(file);

    if is_start_up {
        // Read and check the version
        debug!("gossip_reader: checking gossip_store version...");
        let mut version = [0u8; 1];
        reader.read_exact(&mut version)?;
        if (u8::from_be_bytes(version) & 0b1110_0000) != 0b0000_0000 {
            warn!("gossip_reader: Unsupported gossip_store version!");
            return plugin.shutdown();
        }
        debug!("gossip_reader: gossip_store version is good");
    }

    let mut channel_anns = plugin.state().gossip_store_anns.lock();
    let mut channel_updates: HashMap<DirectedChannel, ChannelUpdate> = HashMap::new();
    let mut channel_amts = plugin.state().gossip_store_amts.lock();
    let mut channel_dels = Vec::new();
    let mut last_scid = None;

    reader.seek(SeekFrom::Current(*offset as i64))?;
    loop {
        *offset = reader.stream_position()?;
        // Read the record header + type
        let mut header_type = [0u8; 14];

        let flags;
        let len;
        // let crc;
        // let timestamp;
        let msg_type;
        match reader.read_exact(&mut header_type) {
            Ok(_) => {
                flags = u16::from_be_bytes(header_type[0..2].try_into()?);
                len = u16::from_be_bytes(header_type[2..4].try_into()?);
                // crc = u32::from_be_bytes(header_type[4..8].try_into()?);
                // timestamp = u32::from_be_bytes(header_type[8..12].try_into()?);
                msg_type = u16::from_be_bytes(header_type[12..14].try_into()?);
            }
            Err(e) => {
                // EOF or read error
                debug!(
                    "gossip_reader: header error at {}:{} (not an actual error if \
                        buffer could not be filled)",
                    offset, e
                );
                break;
            }
        };

        // Check if the record is marked as deleted
        if flags & 0x8000 != 0 {
            reader.seek(SeekFrom::Current((len - 2) as i64))?;
            continue;
        }
        // Check if the record is marked as dying
        if flags & 0x0800 != 0 {
            reader.seek(SeekFrom::Current((len - 2) as i64))?;
            continue;
        }

        match msg_type {
            256 => {
                // public channel_announcement
                let mut ann = vec![0u8; len as usize - 2];
                reader.read_exact(&mut ann)?;
                let (scid, chan_ann) = parse_channel_announcement(&ann)?;
                last_scid = Some(scid);
                channel_anns.insert(scid, chan_ann);
            }
            4104 => {
                // private_channel_announcement
                //  `gossip_store_private_channel` (4104)
                //   - `amount_sat`: u64
                //   - `len`: u16
                //   - `msg_type + announcement`: u16 + u8[len-2]
                let mut ann = vec![0u8; len as usize - 2];
                reader.read_exact(&mut ann)?;
                let (scid, chan_ann) = parse_channel_announcement(&ann[12..])?;
                channel_amts.insert(scid, u64::from_be_bytes(ann[0..8].try_into()?));
                channel_anns.insert(scid, chan_ann);
            }
            258 => {
                // channel_update
                let mut update = vec![0u8; len as usize - 2];
                reader.read_exact(&mut update)?;
                let (scid, chan_up) = parse_channel_update(&update)?;
                channel_updates.insert(
                    DirectedChannel {
                        short_channel_id: scid,
                        direction: chan_up.direction,
                    },
                    chan_up,
                );
            }
            4102 => {
                //   - `gossip_store_private_update` (4102)
                //   - `len`: u16
                //   - `msg_type + update`: u16 + u8[len-2]
                let mut update = vec![0u8; len as usize - 2];
                reader.read_exact(&mut update)?;
                let (scid, chan_up) = parse_channel_update(&update[4..])?;
                channel_updates.insert(
                    DirectedChannel {
                        short_channel_id: scid,
                        direction: chan_up.direction,
                    },
                    chan_up,
                );
            }
            4101 => {
                // gossip_store_channel_amount
                //  - `satoshis`: u64
                let mut satoshis = [0u8; 8];
                reader.read_exact(&mut satoshis)?;
                if let Some(scid) = last_scid {
                    channel_amts.insert(scid, u64::from_be_bytes(satoshis));
                    last_scid = None;
                } else {
                    warn!("gossip_reader: Malformed gossip_store: 4101 without 256")
                }
            }
            4103 => {
                // 4103 gossip_store_delete_chan
                //  - `scid`: u64
                let mut scid_bytes = vec![0u8; 8];
                reader.read_exact(&mut scid_bytes)?;
                let scid = extract_scid(&scid_bytes)?;
                channel_dels.push(scid);
                channel_anns.remove(&scid);
                channel_updates.remove(&DirectedChannel {
                    short_channel_id: scid,
                    direction: 0,
                });
                channel_updates.remove(&DirectedChannel {
                    short_channel_id: scid,
                    direction: 1,
                });
                channel_amts.remove(&scid);
            }
            4106 => {
                // 4106 WIRE_GOSSIP_STORE_CHAN_DYING
                //  - `scid`: u64
                //  - `blockheight`: u32
                let mut scid_bytes = vec![0u8; 12];
                reader.read_exact(&mut scid_bytes)?;
                let scid = extract_scid(&scid_bytes[0..8])?;
                channel_dels.push(scid);
                channel_anns.remove(&scid);
                channel_updates.remove(&DirectedChannel {
                    short_channel_id: scid,
                    direction: 0,
                });
                channel_updates.remove(&DirectedChannel {
                    short_channel_id: scid,
                    direction: 1,
                });
                channel_amts.remove(&scid);
            }
            _e => {
                // Unknown message type
                // debug!("unknown: {}", e);
                reader.seek(SeekFrom::Current(len as i64 - 2))?;
            }
        }
    }
    debug!(
        "gossip_reader: gossip_store read in: {}ms",
        now.elapsed().as_millis()
    );
    debug!(
        "gossip_reader: found updates:{} announcements:{} amounts:{} deletes/dying:{}",
        channel_updates.len(),
        channel_anns.len(),
        channel_amts.len(),
        channel_dels.len()
    );

    let post_now = Instant::now();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let mut lngraph = plugin.state().graph.lock();

    for node_channels in lngraph.graph.values_mut() {
        for (dir_chan, dir_chan_state) in node_channels {
            if let Some(update) = channel_updates.get(dir_chan) {
                dir_chan_state.update(update);
            }
        }
    }

    for (ann_scid, chan_ann) in channel_anns.iter() {
        let dir_chan_0 = DirectedChannel {
            short_channel_id: *ann_scid,
            direction: 0,
        };
        let dir_chan_1 = DirectedChannel {
            short_channel_id: *ann_scid,
            direction: 1,
        };
        for dir_chan in [dir_chan_0, dir_chan_1] {
            if let Some(chan_update) = channel_updates.get(&dir_chan) {
                if let Some(chan_amt) = channel_amts.get(ann_scid) {
                    let (source, destination) = get_node_order(
                        chan_update.direction,
                        chan_ann.source,
                        chan_ann.destination,
                    )?;
                    let new_dir_chan_state = DirectedChannelState {
                        source,
                        destination,
                        active: chan_update.active,
                        scid_alias: None,
                        fee_per_millionth: chan_update.fee_per_millionth,
                        base_fee_millisatoshi: chan_update.base_fee_millisatoshi,
                        htlc_maximum_msat: chan_update.htlc_maximum_msat,
                        htlc_minimum_msat: chan_update.htlc_minimum_msat,
                        amount_msat: Amount::from_sat(*chan_amt),
                        delay: chan_update.delay,
                        liquidity: chan_update.htlc_maximum_msat.msat() / 2,
                        liquidity_age: timestamp,
                        last_update: chan_update.last_update,
                    };
                    if let Some(graph_node_channels) = lngraph.graph.get_mut(&source) {
                        if let Some(old_dir_chan_state) = graph_node_channels.get_mut(&dir_chan) {
                            old_dir_chan_state.update(chan_update);
                        } else {
                            graph_node_channels.insert(dir_chan, new_dir_chan_state);
                        }
                    } else {
                        let mut first_chan = HashMap::new();
                        first_chan.insert(dir_chan, new_dir_chan_state);
                        lngraph.graph.insert(source, first_chan);
                    }
                }
            }
        }
    }

    channel_anns.retain(|scid, chan_ann| {
        let has_channel = |node| {
            lngraph.graph.get(node).map_or(false, |channels| {
                channels.contains_key(&DirectedChannel {
                    short_channel_id: *scid,
                    direction: 0,
                }) || channels.contains_key(&DirectedChannel {
                    short_channel_id: *scid,
                    direction: 1,
                })
            })
        };

        !(has_channel(&chan_ann.source) && has_channel(&chan_ann.destination))
    });

    channel_amts.retain(|scid, _| channel_anns.contains_key(scid));

    for channels in lngraph.graph.values_mut() {
        channels.retain(|dir_chan, _| {
            if is_start_up {
                !channel_dels.contains(&dir_chan.short_channel_id)
                    && channel_updates.contains_key(dir_chan)
            } else {
                !channel_dels.contains(&dir_chan.short_channel_id)
            }
        })
    }

    debug!(
        "gossip_reader: post_processing_time: {}ms",
        post_now.elapsed().as_millis()
    );
    Ok(())
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

fn extract_scid(buf: &[u8]) -> Result<ShortChannelId, Error> {
    let block = (u32::from(buf[0]) << 16) | (u32::from(buf[1]) << 8) | u32::from(buf[2]);
    let tx = (u32::from(buf[3]) << 16) | (u32::from(buf[4]) << 8) | u32::from(buf[5]);
    let txout = (u16::from(buf[6]) << 8) | u16::from(buf[7]);
    ShortChannelId::from_str(&format!("{}x{}x{}", block, tx, txout))
}

fn parse_channel_update(inpu: &[u8]) -> Result<(ShortChannelId, ChannelUpdate), Error> {
    let scid = extract_scid(&inpu[96..104])?;
    Ok((
        scid,
        ChannelUpdate {
            direction: (inpu[109] & 0b0000_0001) as u32,
            // message_flags: inpu[108],
            // channel_flags: inpu[109],
            active: ((inpu[109] & 0b0000_0010) >> 1) != 1,
            last_update: u32::from_be_bytes(inpu[104..108].try_into()?),
            base_fee_millisatoshi: u32::from_be_bytes(inpu[120..124].try_into()?),
            fee_per_millionth: u32::from_be_bytes(inpu[124..128].try_into()?),
            delay: (u32::from(inpu[110]) << 8) | u32::from(inpu[111]),
            htlc_minimum_msat: Amount::from_msat(u64::from_be_bytes(inpu[112..120].try_into()?)),
            htlc_maximum_msat: Amount::from_msat(u64::from_be_bytes(inpu[128..136].try_into()?)),
        },
    ))
}

fn parse_channel_announcement(inpu: &[u8]) -> Result<(ShortChannelId, ChannelAnnouncement), Error> {
    // 0..64 sig1
    // 64..128 sig2
    // 128..192 bc_sig_1
    // 192..256 bc_sig_2
    let len = u16::from_be_bytes(inpu[256..258].try_into()?) as usize;
    // 258..258+len features
    // 258+len..290+len chain_hash
    let scid = extract_scid(&inpu[(290 + len)..(298 + len)])?;
    let source = PublicKey::from_slice(&inpu[(298 + len)..(331 + len)])?;
    let destination = PublicKey::from_slice(&inpu[(331 + len)..(364 + len)])?;
    Ok((
        scid,
        ChannelAnnouncement {
            source,
            destination,
            // features: String::new(),
        },
    ))
}
