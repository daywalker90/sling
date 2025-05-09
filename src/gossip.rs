use std::{
    fs::File,
    io::{BufReader, Read},
    time::Instant,
};

use anyhow::{anyhow, Error};
use bitcoin::secp256k1::PublicKey;
use cln_plugin::Plugin;
use cln_rpc::primitives::{Amount, ShortChannelId, ShortChannelIdDir};

use crate::{
    model::{IncompleteChannels, LnGraph, ShortChannelIdDirStateBuilder},
    PluginState,
};

#[derive(Debug, Clone, Copy)]
pub struct ChannelUpdate {
    // pub direction: u32,
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

#[derive(Debug, Clone, Copy)]
pub struct ChannelAnnouncement {
    pub source: PublicKey,
    pub destination: PublicKey,
    // pub features: String,
}

const CHUNK_SIZE: usize = 1024 * 1024;

pub async fn read_gossip_store(
    plugin: Plugin<PluginState>,
    reader: &mut BufReader<File>,
    is_start_up: &mut bool,
) -> Result<(), Error> {
    let mut graph = plugin.state().graph.lock();
    let mut incomplete_channels = plugin.state().incomplete_channels.lock();

    let mut offset = 0;

    read_gossip_file(
        is_start_up,
        reader,
        &mut graph,
        &mut incomplete_channels,
        &mut offset,
    )?;

    Ok(())
}

pub fn read_gossip_file(
    is_start_up: &mut bool,
    reader: &mut BufReader<File>,
    graph: &mut LnGraph,
    incomplete_channels: &mut IncompleteChannels,
    offset: &mut usize,
) -> Result<(), anyhow::Error> {
    let now = Instant::now();

    if *is_start_up {
        // Read and check the version
        let mut gossip_ver_buffer = vec![0u8; 1];
        reader.read_exact(&mut gossip_ver_buffer)?;
        log::debug!("read_gossip_file: checking gossip_store version...");
        if (gossip_ver_buffer[0] & 0b1110_0000) != 0b0000_0000 {
            log::warn!("read_gossip_file: Unsupported gossip_store version!");
            return Err(anyhow!(
                "read_gossip_file: Unsupported gossip_store version!"
            ));
        }
        log::debug!("read_gossip_file: gossip_store version is good");
    }

    let mut gossip_file = vec![0u8; CHUNK_SIZE];

    loop {
        let mut bytes_read = 0;

        // Read up to CHUNK_SIZE bytes
        while bytes_read < CHUNK_SIZE {
            match reader.read(&mut gossip_file[bytes_read..]) {
                Ok(0) => break, // EOF reached
                Ok(n) => bytes_read += n,
                Err(e) => return Err(e).map_err(|e| anyhow!("Error reading gossip file: {}", e)),
            }
        }

        if bytes_read == 0 {
            break;
        }
        let test_now = Instant::now();
        read_gossip_file_chunk(
            &gossip_file[..bytes_read],
            offset,
            graph,
            incomplete_channels,
        )?;
        log::debug!(
            "read_gossip_file: gossip_store read chunk {} in: {}ms",
            bytes_read,
            test_now.elapsed().as_millis()
        );
        if *offset < bytes_read {
            reader.seek_relative(*offset as i64 - bytes_read as i64)?;
        }
        *offset = 0;
    }
    log::debug!(
        "read_gossip_file: gossip_store read in: {}ms",
        now.elapsed().as_millis()
    );
    log::debug!(
        "read_gossip_file: found {} potential channels",
        incomplete_channels.len(),
    );

    let post_now = Instant::now();

    incomplete_channels.update_graph(graph);

    *is_start_up = false;

    log::debug!(
        "read_gossip_file: post_processing_time: {}ms",
        post_now.elapsed().as_millis()
    );
    log::debug!(
        "read_gossip_file: found {} actual channels and {} incomplete channels",
        graph.public_channel_count(),
        incomplete_channels.len()
    );
    Ok(())
}

fn read_gossip_file_chunk(
    gossip_file: &[u8],
    offset: &mut usize,
    graph: &mut LnGraph,
    incomplete_channels: &mut IncompleteChannels,
) -> Result<(), anyhow::Error> {
    log::debug!(
        "read_gossip_file_chunk: reading gossip_store chunk of size {}",
        gossip_file.len()
    );
    while *offset + 14 < gossip_file.len() {
        // Read the record header + type
        let flags = u16::from_be_bytes(gossip_file[*offset..*offset + 2].try_into()?);
        *offset += 2;
        let len = u16::from_be_bytes(gossip_file[*offset..*offset + 2].try_into()?) as usize;
        *offset += 10;
        if *offset + len > gossip_file.len() {
            *offset -= 12;
            break;
        }
        // let crc;
        // let timestamp;
        let msg_type = u16::from_be_bytes(gossip_file[*offset..*offset + 2].try_into()?);
        *offset += 2;

        // Check if the record is marked as deleted
        if flags & 0x8000 != 0 {
            *offset += len - 2;
            continue;
        }
        // Check if the record is marked as dying
        if flags & 0x0800 != 0 {
            *offset += len - 2;
            continue;
        }

        match msg_type {
            256 => {
                // public channel_announcement
                let (scid, chan_ann) =
                    parse_channel_announcement(&gossip_file[*offset..*offset + len - 2])?;
                *offset += len - 2;

                let dir_chan_0 = ShortChannelIdDir {
                    short_channel_id: scid,
                    direction: 0,
                };
                let dir_chan_1 = ShortChannelIdDir {
                    short_channel_id: scid,
                    direction: 1,
                };
                if !graph.has_announcement(&dir_chan_0, &chan_ann)? {
                    if let Some(chan_state) = incomplete_channels.get_mut(&dir_chan_0) {
                        if !chan_state.has_announcement() {
                            chan_state.add_announcement(dir_chan_0.direction, chan_ann)?;
                        }
                    } else {
                        let mut chan_state = ShortChannelIdDirStateBuilder::new();
                        chan_state.add_announcement(dir_chan_0.direction, chan_ann)?;

                        incomplete_channels.insert(dir_chan_0, chan_state);
                    }
                }
                if !graph.has_announcement(&dir_chan_1, &chan_ann)? {
                    if let Some(chan_state) = incomplete_channels.get_mut(&dir_chan_1) {
                        if !chan_state.has_announcement() {
                            chan_state.add_announcement(dir_chan_1.direction, chan_ann)?;
                        }
                    } else {
                        let mut chan_state = ShortChannelIdDirStateBuilder::new();
                        chan_state.add_announcement(dir_chan_1.direction, chan_ann)?;

                        incomplete_channels.insert(dir_chan_1, chan_state);
                    }
                }
            }
            4104 => {
                // private_channel_announcement
                //  `gossip_store_private_channel` (4104)
                //   - `amount_sat`: u64
                //   - `len`: u16
                //   - `msg_type + announcement`: u16 + u8[len-2]
                let (scid, chan_ann) =
                    parse_channel_announcement(&gossip_file[*offset + 12..*offset + 10 + len])?;

                let dir_chan_0 = ShortChannelIdDir {
                    short_channel_id: scid,
                    direction: 0,
                };
                let dir_chan_1 = ShortChannelIdDir {
                    short_channel_id: scid,
                    direction: 1,
                };

                if !graph.has_announcement(&dir_chan_0, &chan_ann)? {
                    if let Some(chan_state) = incomplete_channels.get_mut(&dir_chan_0) {
                        if !chan_state.has_announcement() {
                            chan_state.add_announcement(dir_chan_0.direction, chan_ann)?;
                        }
                    } else {
                        let mut chan_state = ShortChannelIdDirStateBuilder::new();
                        chan_state.add_announcement(dir_chan_0.direction, chan_ann)?;

                        incomplete_channels.insert(dir_chan_0, chan_state);
                    }
                }
                if !graph.has_announcement(&dir_chan_1, &chan_ann)? {
                    if let Some(chan_state) = incomplete_channels.get_mut(&dir_chan_1) {
                        if !chan_state.has_announcement() {
                            chan_state.add_announcement(dir_chan_1.direction, chan_ann)?;
                        }
                    } else {
                        let mut chan_state = ShortChannelIdDirStateBuilder::new();
                        chan_state.add_announcement(dir_chan_1.direction, chan_ann)?;

                        incomplete_channels.insert(dir_chan_1, chan_state);
                    }
                }
                *offset += len + 10;
            }
            258 => {
                // channel_update
                let (scid_dir, chan_up) =
                    parse_channel_update(&gossip_file[*offset..*offset + len - 2])?;
                *offset += len - 2;
                let mut updated = false;

                if let Some(chan_state) = graph.get_state_mut_direction(scid_dir) {
                    chan_state.update(chan_up);
                    updated = true;
                }

                if !updated {
                    if let Some(chan_state) = incomplete_channels.get_mut(&scid_dir) {
                        chan_state.add_update(chan_up);
                    } else {
                        let mut chan_state = ShortChannelIdDirStateBuilder::new();
                        chan_state.add_update(chan_up);

                        incomplete_channels.insert(scid_dir, chan_state);
                    }
                }
            }
            4102 => {
                //   - `gossip_store_private_update` (4102)
                //   - `len`: u16
                //   - `msg_type + update`: u16 + u8[len-2]
                let (scid_dir, chan_up) =
                    parse_channel_update(&gossip_file[*offset + 4..*offset + 2 + len])?;
                *offset += len + 2;
                let mut updated = false;
                if let Some(chan_state) = graph.get_state_mut_direction(scid_dir) {
                    chan_state.update(chan_up);
                    updated = true;
                }
                if !updated {
                    if let Some(chan_state) = incomplete_channels.get_mut(&scid_dir) {
                        chan_state.add_update(chan_up);
                    } else {
                        let mut chan_state = ShortChannelIdDirStateBuilder::new();
                        chan_state.add_update(chan_up);

                        incomplete_channels.insert(scid_dir, chan_state);
                    }
                }
            }
            4101 => {
                // gossip_store_channel_amount
                //  - `satoshis`: u64
                *offset += 8;
            }
            4103 => {
                // 4103 gossip_store_delete_chan
                //  - `scid`: u64
                let scid = extract_scid(&gossip_file[*offset..*offset + 8])?;
                *offset += 8;
                let dir_chan_0 = ShortChannelIdDir {
                    short_channel_id: scid,
                    direction: 0,
                };
                let dir_chan_1 = ShortChannelIdDir {
                    short_channel_id: scid,
                    direction: 1,
                };
                graph.remove(&dir_chan_0);
                graph.remove(&dir_chan_1);
                incomplete_channels.remove(&dir_chan_0);
                incomplete_channels.remove(&dir_chan_1);
            }
            4106 => {
                // 4106 WIRE_GOSSIP_STORE_CHAN_DYING
                //  - `scid`: u64
                //  - `blockheight`: u32
                let scid = extract_scid(&gossip_file[*offset..*offset + 8])?;
                // skip blockheight aswell (+4)
                *offset += 12;
                let dir_chan_0 = ShortChannelIdDir {
                    short_channel_id: scid,
                    direction: 0,
                };
                let dir_chan_1 = ShortChannelIdDir {
                    short_channel_id: scid,
                    direction: 1,
                };
                graph.remove(&dir_chan_0);
                graph.remove(&dir_chan_1);
                incomplete_channels.remove(&dir_chan_0);
                incomplete_channels.remove(&dir_chan_1);
            }
            _e => {
                // Unknown message type
                // debug!("unknown: {}", e);
                *offset += len - 2;
            }
        }
    }

    Ok(())
}

fn extract_scid(gossip_file: &[u8]) -> Result<ShortChannelId, anyhow::Error> {
    let scid = u64::from_be_bytes(gossip_file.try_into()?);
    Ok(ShortChannelId::from(scid))
}

fn parse_channel_update(inpu: &[u8]) -> Result<(ShortChannelIdDir, ChannelUpdate), Error> {
    let scid = extract_scid(&inpu[96..104])?;
    Ok((
        ShortChannelIdDir {
            short_channel_id: scid,
            direction: (inpu[109] & 0b0000_0001) as u32,
        },
        ChannelUpdate {
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
