//! Road/pavement path network from `pc/data/cdimages/paths.img`: 64 `nodesN.nod` files, one per
//! 750 m area of an 8x8 grid covering GTA world x,y in [-3000, 3000] (Z up). Several areas are
//! empty (all-zero header, no water/road there). Format from gtamods.com/wiki/Path_Node plus
//! inspection of the shipped files:
//!
//! Header (16 bytes): u32 total nodes, u32 car nodes, u32 "intersection" (ped) nodes, u32 links.
//! `total` nodes (32 bytes each) follow, then `links` links (8 bytes each); the file is then
//! zero-padded to a 2048-byte sector boundary.
//!
//! Node (32 bytes): u32 mem addr, u32 zero, u16 area, u16 id (== index in the file), u32 street
//! name hash, u16 heuristic cost, **u16 first-link index** (verified 2026-09-27: non-decreasing in\n//! all 35 areas; node i's links are [first_i, first_{i+1}), the last node's run to the header\n//! count; linked nodes are close: median 8.4 m, 90% < 19 m, over all 51,614 links), i16 x, i16 y,\n//! i16 z (x, y / 8; z / 64 — checked 2026-09-27 against the collision ground under 20 nodes, ratio 8.0 every time; gtamods says /128, which is also wrong), u8 path width, u8 floodfill label, u32 flags.
//!
//! Flags: the link count is NOT in the flags (an earlier guess at bits 16-19 didn't add up).\n//! Bit 25 is water (100% of set nodes fall inside a `water.dat`
//! quad's bounding box, cross-checked in `liberty paths`), bit 21 correlates with being near
//! water (likely boat lanes, weaker signal), and bits 24-27 form a small behaviour enum (values
//! 0,1,2,3,4,6 seen). Other bits are unconfirmed and deliberately have no accessor here.
//!
//! Link (8 bytes): u16 target area, u16 target node, u8 length (metres: tracks the real distance
//! between the two nodes, correlation 0.96 over all 47,928 car links), u8 lanes, u16 flags.
//!
//! The lanes byte (decoded 2026-10-01): bits 0-2 = lanes in the link's own direction, bits 3-5 =
//! lanes the other way, bits 6-7 = the traffic-light phase (1 or 2) of a link that ends at a
//! light's stop line, set in the approach direction only (~1,700 links). Decoded 2026-10-01:
//! where marked links share an end node, crossing roads carry different phases in 37 of 38 cases
//! and opposite approaches the same phase in 46 of 49; and IV's own traffic stops on them
//! (stopped 24% of the time on phase-2 links, 3% on unmarked ones; `re/junctions.py`). Checked two ways: in every one of the 47,928 link pairs the reverse link has the
//! two counts swapped; and against IV's own traffic (two captures, 104,207 moving-car samples on
//! 2,272 links, `re/lanes.py`): on "2 with, 0 against" links 28,114 samples drove with the link
//! and 100 against. The same samples give the lane positions (`lane_offset`): lanes are about
//! 5.3 m apart; on a two-way road the first lane's middle is 2.6 m right of the node line, on a
//! one-way road the lanes straddle the line.
//!
//! Nodes [0, car_nodes) are car (road) nodes; [car_nodes, total) are pedestrian (pavement) nodes
//! — checked in `liberty paths` by looking at their flags/width and who they link to.

use anyhow::{Result, ensure};

#[derive(Debug, Clone, Copy)]
pub struct PathNode {
    pub area: u16,
    pub id: u16,
    pub street_hash: u32,
    pub heuristic_cost: u16,
    /// Raw u16 at offset 18 whose meaning `liberty paths` tries to pin down.
    pub link_field: u16,
    /// World position (GTA space, Z up): x, y = i16 / 8, z = i16 / 64.
    pub pos: [f32; 3],
    pub width: u8,
    pub floodfill: u8,
    pub flags: u32,
    /// Index of this node's first link in the area's `links` vec.
    pub first_link: u32,
    /// Outgoing link count: next node's first link minus this one's.
    pub link_count: u8,
    /// True if this node's index is >= the area's `car_nodes` (pedestrian node).
    pub is_ped: bool,
}

impl PathNode {
    /// Bit 25: confirmed against `water.dat` — every node with this bit set falls inside a water
    /// quad's bounding box (`liberty paths` reports the cross-check).
    pub fn water(&self) -> bool {
        self.flags & (1 << 25) != 0
    }
    /// Bit 21: weaker signal, but nodes with this bit are disproportionately close to water
    /// (likely boat lanes) — see `liberty paths`.
    pub fn boat(&self) -> bool {
        self.flags & (1 << 21) != 0
    }
    /// Bits 24-27: small enum (0,1,2,3,4,6 observed), plausibly AI driving behaviour.
    pub fn behaviour(&self) -> u8 {
        ((self.flags >> 24) & 0xF) as u8
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PathLink {
    pub area: u16,
    pub node: u16,
    /// Metres between the two nodes.
    pub length: u8,
    /// Lanes with the link (bits 0-2), against it (bits 3-5), and two marks (bits 6-7).
    pub lanes: u8,
    pub flags: u16,
}

impl PathLink {
    pub fn lanes_with(&self) -> u8 {
        self.lanes & 7
    }
    pub fn lanes_against(&self) -> u8 {
        (self.lanes >> 3) & 7
    }
    /// 0 = no light at the end of this link; 1 or 2 = which of the junction's two phases it runs on.
    pub fn light_phase(&self) -> u8 {
        self.lanes >> 6
    }
}

/// Distance between lane middles, metres (measured from IV's traffic: peaks at 2.6 and 8.0 m on
/// two-lanes-each-way roads, at -2.5 and +2.5 m on one-way two-lane roads).
pub const LANE_WIDTH: f32 = 5.3;

/// How far right of the link's line (from its start node toward its end) the middle of lane
/// `lane` (0 = innermost) sits, for a link with `with` lanes its own way and `against` the other.
pub fn lane_offset(with: u8, against: u8, lane: u8) -> f32 {
    let lane = lane.min(with.saturating_sub(1)) as f32;
    if against == 0 {
        // one-way: the lanes straddle the line
        (lane - (with.max(1) as f32 - 1.0) / 2.0) * LANE_WIDTH
    } else {
        (lane + 0.5) * LANE_WIDTH
    }
}

#[derive(Debug, Clone, Default)]
pub struct PathArea {
    pub nodes: Vec<PathNode>,
    pub links: Vec<PathLink>,
    pub car_nodes: u32,
    pub ped_nodes: u32,
    /// Link count from the header, to cross-check against the sum of per-node link counts.
    pub header_links: u32,
}

/// Parse one `nodesN.nod` file. `area` is the area id from the filename (used only to sanity
/// check against the area field stored in each node, when there are any).
pub fn read_nod(bytes: &[u8], area: u32) -> Result<PathArea> {
    ensure!(bytes.len() >= 16, "area {area}: .nod too short for header");
    let u32_at = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
    let total = u32_at(0) as usize;
    let car_nodes = u32_at(4);
    let ped_nodes = u32_at(8);
    let header_links = u32_at(12);

    if total == 0 {
        return Ok(PathArea { car_nodes, ped_nodes, header_links, ..Default::default() });
    }

    let nodes_end = 16 + total * 32;
    ensure!(bytes.len() >= nodes_end, "area {area}: .nod too short for {total} nodes");
    let u16_at = |o: usize| u16::from_le_bytes(bytes[o..o + 2].try_into().unwrap());
    let i16_at = |o: usize| i16::from_le_bytes(bytes[o..o + 2].try_into().unwrap());

    let mut nodes = Vec::with_capacity(total);
    let first = |i: usize| if i < total { u16_at(16 + i * 32 + 18) as u32 } else { header_links };
    for i in 0..total {
        let o = 16 + i * 32;
        let flags = u32_at(o + 28);
        let link_count = first(i + 1).saturating_sub(first(i)).min(255) as u8;
        let pos = [
            i16_at(o + 20) as f32 / 8.0,
            i16_at(o + 22) as f32 / 8.0,
            i16_at(o + 24) as f32 / 64.0,
        ];
        nodes.push(PathNode {
            area: u16_at(o + 8),
            id: u16_at(o + 10),
            street_hash: u32_at(o + 12),
            heuristic_cost: u16_at(o + 16),
            link_field: u16_at(o + 18),
            pos,
            width: bytes[o + 26],
            floodfill: bytes[o + 27],
            flags,
            first_link: first(i),
            link_count,
            is_ped: i as u32 >= car_nodes,
        });
    }

    let links_end = nodes_end + header_links as usize * 8;
    ensure!(bytes.len() >= links_end, "area {area}: .nod too short for {header_links} links");
    let mut links = Vec::with_capacity(header_links as usize);
    for i in 0..header_links as usize {
        let o = nodes_end + i * 8;
        links.push(PathLink {
            area: u16_at(o),
            node: u16_at(o + 2),
            length: bytes[o + 4],
            lanes: bytes[o + 5],
            flags: u16_at(o + 6),
        });
    }

    Ok(PathArea { nodes, links, car_nodes, ped_nodes, header_links })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node_bytes(area: u16, id: u16, x: i16, y: i16, z: i16, width: u8, flags: u32) -> [u8; 32] {
        let mut b = [0u8; 32];
        b[8..10].copy_from_slice(&area.to_le_bytes());
        b[10..12].copy_from_slice(&id.to_le_bytes());
        b[20..22].copy_from_slice(&x.to_le_bytes());
        b[22..24].copy_from_slice(&y.to_le_bytes());
        b[24..26].copy_from_slice(&z.to_le_bytes());
        b[26] = width;
        b[28..32].copy_from_slice(&flags.to_le_bytes());
        b
    }

    #[test]
    fn empty_area_is_all_zero_header() {
        let a = read_nod(&[0u8; 2048], 5).unwrap();
        assert!(a.nodes.is_empty());
        assert!(a.links.is_empty());
    }

    #[test]
    fn parses_nodes_links_and_flags() {
        let mut bytes = Vec::new();
        // 1 car node with 1 link, 1 ped node with 0 links.
        bytes.extend_from_slice(&1u32.to_le_bytes()); // total (overwritten below)
        bytes.extend_from_slice(&1u32.to_le_bytes()); // car_nodes
        bytes.extend_from_slice(&1u32.to_le_bytes()); // ped_nodes
        bytes.extend_from_slice(&1u32.to_le_bytes()); // links
        bytes[0..4].copy_from_slice(&2u32.to_le_bytes()); // total = 2
        let mut n0 = node_bytes(27, 0, -4965, -5999, 1256, 4, 0);
        n0[18..20].copy_from_slice(&0u16.to_le_bytes()); // first link 0
        let mut n1 = node_bytes(27, 1, 0, 0, 0, 2, 0);
        n1[18..20].copy_from_slice(&1u16.to_le_bytes()); // first link 1 (so node 0 owns 1 link)
        bytes.extend_from_slice(&n0);
        bytes.extend_from_slice(&n1);
        // one link: area 27, node 1, raw byte, length 10, flags 0
        bytes.extend_from_slice(&27u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.push(0);
        bytes.push(10);
        bytes.extend_from_slice(&0u16.to_le_bytes());

        let a = read_nod(&bytes, 27).unwrap();
        assert_eq!(a.nodes.len(), 2);
        assert_eq!(a.links.len(), 1);
        assert_eq!(a.car_nodes, 1);
        assert!(!a.nodes[0].is_ped);
        assert!(a.nodes[1].is_ped);
        assert_eq!(a.nodes[0].link_count, 1);
        assert_eq!(a.nodes[0].first_link, 0);
        assert_eq!(a.nodes[1].first_link, 1);
        assert_eq!(a.nodes[1].link_count, 0);
        assert_eq!(a.nodes[0].pos, [-620.625, -749.875, 19.625]);
        assert_eq!(a.links[0].node, 1);
        assert_eq!(a.links[0].lanes, 10);
    }
}
