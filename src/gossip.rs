use std::{
    fs::File,
    io::{BufReader, Read, Seek},
    time::Instant,
};

use anyhow::{Error, anyhow};
use bitcoin::consensus::encode::serialize_hex;
use cln_plugin::Plugin;
use cln_rpc::primitives::{Amount, ShortChannelId, ShortChannelIdDir};
use serde::Serialize;

use crate::{
    PluginState,
    model::{IncompleteChannels, LnGraph, PubKeyBytes, ShortChannelIdDirStateBuilder},
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
    pub inbound_fee: Option<InboundFee>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct InboundFee {
    pub base_msat: i32,
    pub proportional_millionths: i32,
}

#[derive(Debug, Clone, Copy)]
pub struct ChannelAnnouncement {
    pub source: PubKeyBytes,
    pub destination: PubKeyBytes,
    // pub features: String,
}

#[derive(Debug, Clone, Copy)]
pub struct NextGossipStore {
    pub equivalent_offset: u64,
    pub uuid: [u8; 32],
}

const CHUNK_SIZE: usize = 1024 * 1024;

pub async fn read_gossip_store(
    plugin: Plugin<PluginState>,
    reader: &mut BufReader<File>,
    is_start_up: &mut bool,
    store_hint: Option<NextGossipStore>,
) -> Result<Option<NextGossipStore>, Error> {
    let mut graph = plugin.state().graph.lock();
    let mut incomplete_channels = plugin.state().incomplete_channels.lock();

    let mut offset = 0;

    let next_store = read_gossip_file(
        is_start_up,
        reader,
        &mut graph,
        &mut incomplete_channels,
        &mut offset,
        store_hint,
    )?;

    Ok(next_store)
}

pub fn read_gossip_file(
    is_start_up: &mut bool,
    reader: &mut BufReader<File>,
    graph: &mut LnGraph,
    incomplete_channels: &mut IncompleteChannels,
    offset: &mut usize,
    store_hint: Option<NextGossipStore>,
) -> Result<Option<NextGossipStore>, anyhow::Error> {
    let now = Instant::now();

    if *is_start_up {
        // Read and check the version
        let mut gossip_ver_buffer = vec![0u8; 1];
        reader.read_exact(&mut gossip_ver_buffer)?;
        log::trace!("read_gossip_file: checking gossip_store version...");
        if (gossip_ver_buffer[0] & 0b1110_0000) != 0b0000_0000 {
            log::warn!("read_gossip_file: Unsupported gossip_store version!");
            return Err(anyhow!(
                "read_gossip_file: Unsupported gossip_store version!"
            ));
        }
        log::trace!("read_gossip_file: gossip_store version is good");
    }

    let mut gossip_file = vec![0u8; CHUNK_SIZE];

    let mut next_store = None;

    loop {
        let mut bytes_read = 0;

        // Read up to CHUNK_SIZE bytes
        while bytes_read < CHUNK_SIZE {
            match reader.read(&mut gossip_file[bytes_read..]) {
                Ok(0) => break, // EOF reached
                Ok(n) => bytes_read += n,
                Err(e) => return Err(e).map_err(|e| anyhow!("Error reading gossip file: {e}")),
            }
        }

        if bytes_read == 0 {
            break;
        }
        let test_now = Instant::now();

        next_store = read_gossip_file_chunk(
            &gossip_file[..bytes_read],
            offset,
            graph,
            incomplete_channels,
            reader,
            store_hint,
        )?;

        log::trace!(
            "read_gossip_file: gossip_store read chunk {} in: {}ms",
            bytes_read,
            test_now.elapsed().as_millis()
        );
        if next_store.is_some() {
            break;
        }
        if *offset < bytes_read {
            reader.seek_relative(*offset as i64 - bytes_read as i64)?;
        }
        *offset = 0;
    }
    log::trace!(
        "read_gossip_file: gossip_store read in: {}ms",
        now.elapsed().as_millis()
    );
    log::trace!(
        "read_gossip_file: found {} potential channels",
        incomplete_channels.len(),
    );

    let post_now = Instant::now();

    incomplete_channels.update_graph(graph);

    *is_start_up = false;

    log::trace!(
        "read_gossip_file: found {} actual channels and {} incomplete channels",
        graph.public_channel_count(),
        incomplete_channels.len()
    );
    log::trace!(
        "read_gossip_file: post_processing_time: {}ms",
        post_now.elapsed().as_millis()
    );
    Ok(next_store)
}

fn read_gossip_file_chunk(
    gossip_file: &[u8],
    offset: &mut usize,
    graph: &mut LnGraph,
    incomplete_channels: &mut IncompleteChannels,
    reader: &mut BufReader<File>,
    store_hint: Option<NextGossipStore>,
) -> Result<Option<NextGossipStore>, anyhow::Error> {
    log::trace!(
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
                    match parse_channel_announcement(&gossip_file[*offset..*offset + len - 2]) {
                        Ok(x) => {
                            *offset += len - 2;
                            x
                        }
                        Err(e) => {
                            log::warn!(
                                "read_gossip_file_chunk: Error parsing \
                            channel_announcement: {e}"
                            );
                            *offset += len - 2;
                            continue;
                        }
                    };

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
                let (scid, chan_ann) = match parse_channel_announcement(
                    &gossip_file[*offset + 12..*offset + 10 + len],
                ) {
                    Ok(x) => {
                        *offset += len + 10;
                        x
                    }
                    Err(e) => {
                        log::warn!(
                            "read_gossip_file_chunk: Error parsing \
                            channel_announcement: {e}"
                        );
                        *offset += len + 10;
                        continue;
                    }
                };

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
            258 => {
                // channel_update
                let (scid_dir, chan_up) =
                    match parse_channel_update(&gossip_file[*offset..*offset + len - 2]) {
                        Ok(x) => {
                            *offset += len - 2;
                            x
                        }
                        Err(e) => {
                            log::warn!(
                                "read_gossip_file_chunk: Error parsing \
                            channel_update: {e}"
                            );
                            *offset += len - 2;
                            continue;
                        }
                    };

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
                    match parse_channel_update(&gossip_file[*offset + 4..*offset + 2 + len]) {
                        Ok(x) => {
                            *offset += len + 2;
                            x
                        }
                        Err(e) => {
                            log::warn!(
                                "read_gossip_file_chunk: Error parsing \
                            channel_update: {e}"
                            );
                            *offset += len + 2;
                            continue;
                        }
                    };
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
                let scid = match extract_scid(&gossip_file[*offset..*offset + 8]) {
                    Ok(x) => {
                        *offset += 8;
                        x
                    }
                    Err(e) => {
                        log::warn!("read_gossip_file_chunk: Error parsing scid: {e}");
                        *offset += 8;
                        continue;
                    }
                };

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
                let scid = match extract_scid(&gossip_file[*offset..*offset + 8]) {
                    Ok(x) => {
                        // skip blockheight aswell (+4)
                        *offset += 12;
                        x
                    }
                    Err(e) => {
                        log::warn!("read_gossip_file_chunk: Error parsing scid: {e}");
                        // skip blockheight aswell (+4)
                        *offset += 12;
                        continue;
                    }
                };

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
            4105 => {
                // 4105 gossip_store_ended
                //  - `equivalent_offset`: u64
                //  - `uuid`: [u8; 32]
                let equivalent_offset =
                    u64::from_be_bytes(gossip_file[*offset..*offset + 8].try_into()?);
                *offset += 8;
                let uuid: [u8; 32] = gossip_file[*offset..*offset + 32].try_into()?;
                log::trace!(
                    "read_gossip_file_chunk: encountered gossip_store_ended with \
                equivalent_offset: {equivalent_offset} and next uuid: {}",
                    serialize_hex(&uuid)
                );
                let next_store = NextGossipStore {
                    equivalent_offset,
                    uuid,
                };
                return Ok(Some(next_store));
            }
            4107 => {
                // 4107 uuid
                //  - `uuid`: [u8; 32]
                let uuid: [u8; 32] = gossip_file[*offset..*offset + 32].try_into()?;
                log::trace!(
                    "read_gossip_file_chunk: opened gossip_store with uuid: {}",
                    serialize_hex(&uuid)
                );

                if let Some(ns) = &store_hint {
                    if uuid != ns.uuid {
                        let next_store = NextGossipStore {
                            equivalent_offset: 0,
                            uuid,
                        };
                        log::info!(
                            "read_gossip_file_chunk: missed gossip_store compaction, \
                        reopening..."
                        );
                        return Ok(Some(next_store));
                    }
                    log::debug!(
                        "read_gossip_file_chunk: seeking to equivalent_offset {} in \
                    compacted gossip_store",
                        ns.equivalent_offset
                    );
                    reader.seek(std::io::SeekFrom::Start(ns.equivalent_offset))?;
                    *offset = gossip_file.len();
                    break;
                }

                *offset += 32;
            }
            _e => {
                // Unknown message type
                // debug!("unknown: {}", e);
                *offset += len - 2;
            }
        }
    }

    Ok(None)
}

fn extract_scid(gossip_file: &[u8]) -> Result<ShortChannelId, anyhow::Error> {
    let scid = u64::from_be_bytes(gossip_file.try_into()?);
    Ok(ShortChannelId::from(scid))
}

fn parse_channel_update(input: &[u8]) -> Result<(ShortChannelIdDir, ChannelUpdate), Error> {
    if input.len() < 136 {
        return Err(anyhow!("channel_update < 136 bytes"));
    }

    let scid = extract_scid(&input[96..104])?;

    let scid_dir = ShortChannelIdDir {
        short_channel_id: scid,
        direction: u32::from(input[109] & 0b0000_0001),
    };

    let mut inbound_fee = None;

    if input.len() > 136 {
        parse_channel_update_tlvs(&input[136..], &mut inbound_fee)?;
    }

    let update = ChannelUpdate {
        active: ((input[109] & 0b0000_0010) >> 1) != 1,
        last_update: u32::from_be_bytes(input[104..108].try_into()?),
        base_fee_millisatoshi: u32::from_be_bytes(input[120..124].try_into()?),
        fee_per_millionth: u32::from_be_bytes(input[124..128].try_into()?),
        delay: u32::from(u16::from_be_bytes(input[110..112].try_into()?)),
        htlc_minimum_msat: Amount::from_msat(u64::from_be_bytes(input[112..120].try_into()?)),
        htlc_maximum_msat: Amount::from_msat(u64::from_be_bytes(input[128..136].try_into()?)),

        inbound_fee,
    };

    Ok((scid_dir, update))
}

fn parse_channel_announcement(
    input: &[u8],
) -> Result<(ShortChannelId, ChannelAnnouncement), Error> {
    // 0..64 sig1
    // 64..128 sig2
    // 128..192 bc_sig_1
    // 192..256 bc_sig_2
    let len = u16::from_be_bytes(input[256..258].try_into()?) as usize;
    // 258..258+len features
    // 258+len..290+len chain_hash
    let scid = extract_scid(&input[(290 + len)..(298 + len)])?;
    let source = PubKeyBytes::new(input[(298 + len)..(331 + len)].try_into()?);
    let destination = PubKeyBytes::new(input[(331 + len)..(364 + len)].try_into()?);
    Ok((
        scid,
        ChannelAnnouncement {
            source,
            destination,
            // features: String::new(),
        },
    ))
}

fn parse_channel_update_tlvs(
    mut data: &[u8],
    inbound_fee: &mut Option<InboundFee>,
) -> Result<(), Error> {
    let mut last_type = 0;

    while !data.is_empty() {
        let (ty, rest) = read_bigsize(data)?;
        let (len, rest) = read_bigsize(rest)?;

        if ty <= last_type {
            return Err(anyhow!("TLV types not strictly increasing"));
        }

        last_type = ty;

        if rest.len() < len as usize {
            return Err(anyhow!("TLV truncated"));
        }

        let (value, remaining) = rest.split_at(len as usize);

        match ty {
            55555 => {
                if value.len() != 8 {
                    return Err(anyhow!("inbound_fee TLV length != 8"));
                }

                let base_msat = i32::from_be_bytes(value[0..4].try_into()?);
                let proportional_millionths = i32::from_be_bytes(value[4..8].try_into()?);

                if base_msat != 0 || proportional_millionths != 0 {
                    *inbound_fee = Some(InboundFee {
                        base_msat,
                        proportional_millionths,
                    });
                }
            }

            even if even % 2 == 0 => {
                return Err(anyhow!("unknown even TLV type {}", even));
            }

            _ => {
                // ignore unknown odd TLVs
            }
        }

        data = remaining;
    }

    Ok(())
}

fn read_bigsize(data: &[u8]) -> Result<(u64, &[u8]), Error> {
    if data.is_empty() {
        return Err(anyhow!("read_bigsize: data is empty"));
    }

    match data[0] {
        n @ 0x00..=0xfc => Ok((u64::from(n), &data[1..])),

        0xfd => {
            if data.len() < 3 {
                return Err(anyhow!("read_bigsize: data is <3"));
            }

            let v = u64::from(u16::from_be_bytes([data[1], data[2]]));

            if v < 0xfd {
                return Err(anyhow!("read_bigsize: v < 0xfd"));
            }

            Ok((v, &data[3..]))
        }

        0xfe => {
            if data.len() < 5 {
                return Err(anyhow!("read_bigsize: data is <5"));
            }

            let v = u64::from(u32::from_be_bytes(data[1..5].try_into().unwrap()));

            if v < 0x10000 {
                return Err(anyhow!("read_bigsize: v < 0x10000"));
            }

            Ok((v, &data[5..]))
        }

        0xff => {
            if data.len() < 9 {
                return Err(anyhow!("read_bigsize: data is <9"));
            }

            let v = u64::from_be_bytes(data[1..9].try_into().unwrap());

            if v < 0x1_0000_0000 {
                return Err(anyhow!("read_bigsize: v < 0x1_0000_0000"));
            }

            Ok((v, &data[9..]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestCase {
        name: &'static str,
        value: u64,
        bytes: &'static str,
        exp_error: Option<&'static str>,
    }

    fn hex_decode(s: &str) -> Vec<u8> {
        if s.is_empty() {
            return Vec::new();
        }

        assert!(s.len() % 2 == 0);

        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn test_read_bigsize_vectors() {
        let tests = vec![
            TestCase {
                name: "zero",
                value: 0,
                bytes: "00",
                exp_error: None,
            },
            TestCase {
                name: "one byte high",
                value: 252,
                bytes: "fc",
                exp_error: None,
            },
            TestCase {
                name: "two byte low",
                value: 253,
                bytes: "fd00fd",
                exp_error: None,
            },
            TestCase {
                name: "two byte high",
                value: 65535,
                bytes: "fdffff",
                exp_error: None,
            },
            TestCase {
                name: "four byte low",
                value: 65536,
                bytes: "fe00010000",
                exp_error: None,
            },
            TestCase {
                name: "four byte high",
                value: 4294967295,
                bytes: "feffffffff",
                exp_error: None,
            },
            TestCase {
                name: "eight byte low",
                value: 4294967296,
                bytes: "ff0000000100000000",
                exp_error: None,
            },
            TestCase {
                name: "eight byte high",
                value: u64::MAX,
                bytes: "ffffffffffffffffff",
                exp_error: None,
            },
            TestCase {
                name: "two byte not canonical",
                value: 0,
                bytes: "fd00fc",
                exp_error: Some("read_bigsize: v < 0xfd"),
            },
            TestCase {
                name: "four byte not canonical",
                value: 0,
                bytes: "fe0000ffff",
                exp_error: Some("read_bigsize: v < 0x10000"),
            },
            TestCase {
                name: "eight byte not canonical",
                value: 0,
                bytes: "ff00000000ffffffff",
                exp_error: Some("read_bigsize: v < 0x1_0000_0000"),
            },
            TestCase {
                name: "two byte short read",
                value: 0,
                bytes: "fd00",
                exp_error: Some("read_bigsize: data is <3"),
            },
            TestCase {
                name: "four byte short read",
                value: 0,
                bytes: "feffff",
                exp_error: Some("read_bigsize: data is <5"),
            },
            TestCase {
                name: "eight byte short read",
                value: 0,
                bytes: "ffffffffff",
                exp_error: Some("read_bigsize: data is <9"),
            },
            TestCase {
                name: "one byte no read",
                value: 0,
                bytes: "",
                exp_error: Some("read_bigsize: data is empty"),
            },
            TestCase {
                name: "two byte no read",
                value: 0,
                bytes: "fd",
                exp_error: Some("read_bigsize: data is <3"),
            },
            TestCase {
                name: "four byte no read",
                value: 0,
                bytes: "fe",
                exp_error: Some("read_bigsize: data is <5"),
            },
            TestCase {
                name: "eight byte no read",
                value: 0,
                bytes: "ff",
                exp_error: Some("read_bigsize: data is <9"),
            },
        ];

        for tc in tests {
            let bytes = hex_decode(tc.bytes);

            match read_bigsize(&bytes) {
                Ok((value, remaining)) => {
                    if let Some(err) = tc.exp_error {
                        panic!(
                            "[{}] expected error '{}', got value {}",
                            tc.name, err, value
                        );
                    }

                    assert_eq!(value, tc.value, "[{}] decoded value mismatch", tc.name);

                    assert!(
                        remaining.is_empty(),
                        "[{}] remaining bytes not empty",
                        tc.name
                    );
                }

                Err(e) => {
                    let err = e.to_string();

                    match tc.exp_error {
                        Some(expected) => {
                            assert_eq!(err, expected, "[{}] error mismatch", tc.name);
                        }

                        None => {
                            panic!("[{}] unexpected error: {}", tc.name, err);
                        }
                    }
                }
            }
        }
    }
}
