use std::{
    collections::HashMap,
    fs::File,
    io::{BufReader, Read},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Error};
use bitcoin::secp256k1::PublicKey;
use cln_plugin::Plugin;
use cln_rpc::primitives::{Amount, ShortChannelId, ShortChannelIdDir};

use crate::{
    model::{LnGraph, ShortChannelIdDirState},
    PluginState,
};

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
pub struct ChannelAnnouncement {
    pub source: PublicKey,
    pub destination: PublicKey,
    // pub features: String,
}

const CHUNK_SIZE: usize = 10 * 1024 * 1024;

pub async fn read_gossip_store(
    plugin: Plugin<PluginState>,
    reader: &mut BufReader<File>,
    is_start_up: &mut bool,
) -> Result<(), Error> {
    let mut channel_anns = plugin.state().gossip_store_anns.lock();
    let mut channel_amts = plugin.state().gossip_store_amts.lock();
    let mut lngraph = plugin.state().graph.lock();

    let mut offset = 0;

    read_gossip_file(
        is_start_up,
        reader,
        &mut lngraph,
        &mut offset,
        &mut channel_anns,
        &mut channel_amts,
    )?;

    Ok(())
}

fn read_gossip_file(
    is_start_up: &mut bool,
    reader: &mut BufReader<File>,
    lngraph: &mut LnGraph,
    offset: &mut usize,
    channel_anns: &mut HashMap<ShortChannelId, ChannelAnnouncement>,
    channel_amts: &mut HashMap<ShortChannelId, u64>,
) -> Result<(), anyhow::Error> {
    let now = Instant::now();

    if *is_start_up {
        // Read and check the version
        let mut gossip_ver_buffer = vec![0u8; 1];
        reader.read_exact(&mut gossip_ver_buffer)?;
        log::debug!("gossip_gossip_file: checking gossip_store version...");
        if (gossip_ver_buffer[0] & 0b1110_0000) != 0b0000_0000 {
            log::warn!("gossip_gossip_file: Unsupported gossip_store version!");
            return Err(anyhow!(
                "gossip_gossip_file: Unsupported gossip_store version!"
            ));
        }
        log::debug!("gossip_gossip_file: gossip_store version is good");
    }

    let mut gossip_file = vec![0u8; CHUNK_SIZE];
    let mut channel_updates: HashMap<ShortChannelIdDir, ChannelUpdate> = HashMap::new();
    let mut channel_dels: Vec<ShortChannelId> = Vec::new();

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

        read_gossip_file_chunk(
            &gossip_file[..bytes_read],
            offset,
            channel_anns,
            channel_amts,
            &mut channel_updates,
            &mut channel_dels,
        )?;
        if *offset < CHUNK_SIZE {
            reader.seek_relative(*offset as i64 - bytes_read as i64)?;
        }
        *offset = 0;
    }
    log::debug!(
        "gossip_gossip_file: gossip_store read in: {}ms",
        now.elapsed().as_millis()
    );
    log::debug!(
        "gossip_gossip_file: found updates:{} announcements:{} amounts:{} deletes/dying:{}",
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

    for node_channels in lngraph.graph.values_mut() {
        for (dir_chan, dir_chan_state) in node_channels {
            if let Some(update) = channel_updates.get(dir_chan) {
                dir_chan_state.update(update);
            }
        }
    }

    for (ann_scid, chan_ann) in channel_anns.iter() {
        let dir_chan_0 = ShortChannelIdDir {
            short_channel_id: *ann_scid,
            direction: 0,
        };
        let dir_chan_1 = ShortChannelIdDir {
            short_channel_id: *ann_scid,
            direction: 1,
        };
        for dir_chan in [dir_chan_0, dir_chan_1] {
            if let Some(chan_update) = channel_updates.get(&dir_chan) {
                if let Some(chan_amt) = channel_amts.get(ann_scid) {
                    let (source, destination) =
                        get_node_order(dir_chan.direction, chan_ann.source, chan_ann.destination)?;
                    let new_dir_chan_state = ShortChannelIdDirState {
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
                        private: false,
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
            lngraph.graph.get(node).is_some_and(|channels| {
                channels.contains_key(&ShortChannelIdDir {
                    short_channel_id: *scid,
                    direction: 0,
                }) || channels.contains_key(&ShortChannelIdDir {
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
            if *is_start_up {
                !channel_dels.contains(&dir_chan.short_channel_id)
                    && channel_updates.contains_key(dir_chan)
            } else {
                !channel_dels.contains(&dir_chan.short_channel_id)
            }
        })
    }

    *is_start_up = false;

    log::debug!(
        "gossip_gossip_file: post_processing_time: {}ms",
        post_now.elapsed().as_millis()
    );
    Ok(())
}

fn read_gossip_file_chunk(
    gossip_file: &[u8],
    offset: &mut usize,
    channel_anns: &mut HashMap<ShortChannelId, ChannelAnnouncement>,
    channel_amts: &mut HashMap<ShortChannelId, u64>,
    channel_updates: &mut HashMap<ShortChannelIdDir, ChannelUpdate>,
    channel_dels: &mut Vec<ShortChannelId>,
) -> Result<(), anyhow::Error> {
    log::trace!("gossip_gossip_file: offset:{}", offset);

    let mut last_scid = None;

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
                last_scid = Some(scid);
                channel_anns.insert(scid, chan_ann);
            }
            4104 => {
                // private_channel_announcement
                //  `gossip_store_private_channel` (4104)
                //   - `amount_sat`: u64
                //   - `len`: u16
                //   - `msg_type + announcement`: u16 + u8[len-2]

                let (scid, chan_ann) =
                    parse_channel_announcement(&gossip_file[*offset + 12..*offset + 10 + len])?;
                channel_amts.insert(
                    scid,
                    u64::from_be_bytes(gossip_file[*offset..*offset + 8].try_into()?),
                );
                *offset += len + 10;
                channel_anns.insert(scid, chan_ann);
            }
            258 => {
                // channel_update
                let (scid_dir, chan_up) =
                    parse_channel_update(&gossip_file[*offset..*offset + len - 2])?;
                *offset += len - 2;
                channel_updates.insert(scid_dir, chan_up);
            }
            4102 => {
                //   - `gossip_store_private_update` (4102)
                //   - `len`: u16
                //   - `msg_type + update`: u16 + u8[len-2]
                let (scid_dir, chan_up) =
                    parse_channel_update(&gossip_file[*offset + 4..*offset + 2 + len])?;
                *offset += len + 2;
                channel_updates.insert(scid_dir, chan_up);
            }
            4101 => {
                // gossip_store_channel_amount
                //  - `satoshis`: u64
                if let Some(scid) = last_scid {
                    channel_amts.insert(
                        scid,
                        u64::from_be_bytes(gossip_file[*offset..*offset + 8].try_into()?),
                    );
                    last_scid = None;
                } else {
                    log::warn!("gossip_gossip_file: Malformed gossip_store: 4101 without 256")
                }
                *offset += 8;
            }
            4103 => {
                // 4103 gossip_store_delete_chan
                //  - `scid`: u64
                let scid = extract_scid(&gossip_file[*offset..*offset + 8])?;
                *offset += 8;
                channel_dels.push(scid);
                channel_anns.remove(&scid);
                channel_updates.remove(&ShortChannelIdDir {
                    short_channel_id: scid,
                    direction: 0,
                });
                channel_updates.remove(&ShortChannelIdDir {
                    short_channel_id: scid,
                    direction: 1,
                });
                channel_amts.remove(&scid);
            }
            4106 => {
                // 4106 WIRE_GOSSIP_STORE_CHAN_DYING
                //  - `scid`: u64
                //  - `blockheight`: u32
                let scid = extract_scid(&gossip_file[*offset..*offset + 8])?;
                // skip blockheight aswell (+4)
                *offset += 12;
                channel_dels.push(scid);
                channel_anns.remove(&scid);
                channel_updates.remove(&ShortChannelIdDir {
                    short_channel_id: scid,
                    direction: 0,
                });
                channel_updates.remove(&ShortChannelIdDir {
                    short_channel_id: scid,
                    direction: 1,
                });
                channel_amts.remove(&scid);
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs::File;
    use std::io::{BufReader, Read};
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    use crate::gossip::read_gossip_file;
    use crate::model::LnGraph;

    // Read current RSS from /proc/self/statm (Linux-specific)
    fn get_current_rss() -> Option<u64> {
        let mut file = File::open("/proc/self/statm").ok()?;
        let mut contents = String::new();
        file.read_to_string(&mut contents).ok()?;
        let fields: Vec<&str> = contents.split_whitespace().collect();
        let resident_pages: u64 = fields.get(1)?.parse().ok()?;
        Some(resident_pages * 4096) // Convert pages to bytes (4KB page size)
    }
    #[test]
    fn test_gossip_file_reader() {
        let iterations = 1;
        let mut vec_times = Vec::new();
        let mut vec_rss = Vec::new();

        for i in 0..iterations {
            let peak_rss = Arc::new(AtomicU64::new(0));
            let sampling_active = Arc::new(std::sync::atomic::AtomicBool::new(true));

            // Spawn a thread to sample RSS
            let peak_rss_clone = Arc::clone(&peak_rss);
            let sampling_active_clone = Arc::clone(&sampling_active);
            let sampling_handle = thread::spawn(move || {
                while sampling_active_clone.load(Ordering::Relaxed) {
                    if let Some(rss) = get_current_rss() {
                        peak_rss_clone.fetch_max(rss, Ordering::Relaxed);
                    }
                    thread::sleep(Duration::from_millis(1)); // Sample every 1ms
                }
            });

            let gossip_file = File::open(Path::new("gossip_store")).expect("Failed to open file");
            let mut reader = BufReader::new(gossip_file);
            let mut is_start_up = true;
            let mut offset = 0;
            let mut channel_anns = HashMap::new();
            let mut channel_amts = HashMap::new();
            let mut lngraph = LnGraph::new();
            let now = Instant::now();
            read_gossip_file(
                &mut is_start_up,
                &mut reader,
                &mut lngraph,
                &mut offset,
                &mut channel_anns,
                &mut channel_amts,
            )
            .expect("read_gossip_file failed");
            let elapsed = now.elapsed().as_millis();
            // Signal the sampling thread to stop
            sampling_active.store(false, Ordering::Relaxed);

            // Wait for the sampling thread to finish
            sampling_handle.join().expect("Sampling thread panicked");

            // Calculate peak RAM used
            let peak_rss = peak_rss.load(Ordering::Relaxed);
            println!(
                "Iteration {}: chans:{} nodes:{} anns:{} amts:{} time: {}ms RAM: {}MB",
                i + 1,
                lngraph.graph.values().map(|v| v.len()).sum::<usize>(),
                lngraph.graph.len(),
                channel_anns.len(),
                channel_amts.len(),
                elapsed,
                peak_rss / (1024 * 1024)
            );
            vec_times.push(elapsed);
            vec_rss.push(peak_rss);
        }
        let vec_avg = vec_times.iter().sum::<u128>() / iterations as u128;
        let rss_avg = vec_rss.iter().sum::<u64>() / iterations as u64;
        println!(
            "average: time {}ms, RAM: {}MB",
            vec_avg,
            rss_avg / (1024 * 1024)
        );
    }
}
