//! Ogg container (RFC 3533): page reading and packet reassembly for every logical stream, and
//! a page writer with correct lacing, granule positions and CRCs.
use oc_core::{Error, ErrorCode, Result};

static CRC_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u32) << 24;
        let mut k = 0;
        while k < 8 {
            c = if c & 0x8000_0000 != 0 {
                (c << 1) ^ 0x04C1_1DB7
            } else {
                c << 1
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

/// Ogg's CRC-32 (polynomial 0x04C11DB7, MSB first, initial value 0, no final xor).
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = 0u32;
    for &b in data {
        c = (c << 8) ^ CRC_TABLE[usize::from((c >> 24) as u8 ^ b)];
    }
    c
}

pub fn sniff(data: &[u8]) -> bool {
    data.starts_with(b"OggS")
}

/// One packet of a logical stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub data: Vec<u8>,
    /// Granule position of the page this packet ends on; -1 when the page has none.
    /// Only meaningful for the last packet that completes on each page.
    pub granule: i64,
    /// True for the packet that ends the stream (page flag EOS).
    pub eos: bool,
}

#[derive(Debug, Clone, Default)]
pub struct LogicalStream {
    pub serial: u32,
    pub packets: Vec<Packet>,
}

/// Parse every page of `data` and gather the packets of each logical stream, in the order the
/// streams begin. Damaged pages (bad CRC) are skipped and the lost packet is dropped; the
/// second value counts them.
pub fn read(data: &[u8]) -> Result<(Vec<LogicalStream>, usize)> {
    if !sniff(data) {
        return Err(Error::new(ErrorCode::UnsupportedFormat, "not an Ogg file"));
    }
    let mut streams: Vec<LogicalStream> = Vec::new();
    // Partial packet carried across pages, per stream.
    let mut carry: Vec<Vec<u8>> = Vec::new();
    let mut bad = 0usize;
    let mut o = 0usize;
    while o + 27 <= data.len() {
        if &data[o..o + 4] != b"OggS" {
            // Resynchronise on the next capture pattern.
            match data[o + 1..].windows(4).position(|w| w == b"OggS") {
                Some(n) => {
                    o += n + 1;
                    bad += 1;
                    continue;
                }
                None => break,
            }
        }
        let nseg = usize::from(data[o + 26]);
        let table_end = o + 27 + nseg;
        let Some(table) = data.get(o + 27..table_end) else {
            break;
        };
        let body_len: usize = table.iter().map(|&b| usize::from(b)).sum();
        let Some(body) = data.get(table_end..table_end + body_len) else {
            break;
        };
        let page = &data[o..table_end + body_len];
        o = table_end + body_len;
        let mut check = page.to_vec();
        check[22..26].fill(0);
        let stored = u32::from_le_bytes([page[22], page[23], page[24], page[25]]);
        if crc32(&check) != stored {
            bad += 1;
            continue;
        }
        let flags = page[5];
        let granule = i64::from_le_bytes([
            page[6], page[7], page[8], page[9], page[10], page[11], page[12], page[13],
        ]);
        let serial = u32::from_le_bytes([page[14], page[15], page[16], page[17]]);
        let idx = match streams.iter().position(|s| s.serial == serial) {
            Some(i) => i,
            None => {
                streams.push(LogicalStream {
                    serial,
                    packets: Vec::new(),
                });
                carry.push(Vec::new());
                streams.len() - 1
            }
        };
        if flags & 1 == 0 && !carry[idx].is_empty() {
            // The page does not continue the packet we were building: it was lost.
            carry[idx].clear();
        }
        let mut pos = 0usize;
        let mut cur = std::mem::take(&mut carry[idx]);
        let mut finished: Vec<Vec<u8>> = Vec::new();
        for &lace in table {
            let n = usize::from(lace);
            cur.extend_from_slice(&body[pos..pos + n]);
            pos += n;
            if lace < 255 {
                finished.push(std::mem::take(&mut cur));
            }
        }
        carry[idx] = cur;
        let count = finished.len();
        for (i, p) in finished.into_iter().enumerate() {
            let last = i + 1 == count;
            streams[idx].packets.push(Packet {
                data: p,
                granule: if last { granule } else { -1 },
                eos: last && flags & 4 != 0,
            });
        }
    }
    Ok((streams, bad))
}

/// Builds pages for one logical stream.
pub struct Writer {
    serial: u32,
    seq: u32,
    out: Vec<u8>,
    lacing: Vec<u8>,
    body: Vec<u8>,
    first: bool,
    continued: bool,
    page_target: usize,
}

impl Writer {
    pub fn new(serial: u32) -> Self {
        Self {
            serial,
            seq: 0,
            out: Vec::new(),
            lacing: Vec::new(),
            body: Vec::new(),
            first: true,
            continued: false,
            page_target: 4096,
        }
    }

    fn flush(&mut self, granule: i64, eos: bool) {
        if self.lacing.is_empty() && !eos {
            return;
        }
        let mut page = Vec::with_capacity(27 + self.lacing.len() + self.body.len());
        page.extend_from_slice(b"OggS");
        page.push(0);
        let mut flags = 0u8;
        if self.continued {
            flags |= 1;
        }
        if self.first {
            flags |= 2;
        }
        if eos {
            flags |= 4;
        }
        page.push(flags);
        page.extend_from_slice(&granule.to_le_bytes());
        page.extend_from_slice(&self.serial.to_le_bytes());
        page.extend_from_slice(&self.seq.to_le_bytes());
        page.extend_from_slice(&[0; 4]);
        page.push(self.lacing.len() as u8);
        page.extend_from_slice(&self.lacing);
        page.extend_from_slice(&self.body);
        let c = crc32(&page);
        page[22..26].copy_from_slice(&c.to_le_bytes());
        self.out.extend_from_slice(&page);
        self.seq += 1;
        self.first = false;
        self.lacing.clear();
        self.body.clear();
        self.continued = false;
    }

    /// Add a packet. `granule` is the position after it; `end_page` forces a page break after it
    /// (headers do this so the first audio packet starts a fresh page); `eos` ends the stream.
    pub fn packet(&mut self, data: &[u8], granule: i64, end_page: bool, eos: bool) {
        let mut rest = data;
        loop {
            if self.lacing.len() == 255 {
                // The packet continues on the next page; this page has no complete packet.
                self.flush(-1, false);
                self.continued = true;
            }
            let n = rest.len().min(255);
            self.lacing.push(n as u8);
            self.body.extend_from_slice(&rest[..n]);
            rest = &rest[n..];
            if n < 255 {
                break;
            }
            if rest.is_empty() {
                // A packet that is a multiple of 255 bytes ends with a zero lacing value.
                if self.lacing.len() == 255 {
                    self.flush(-1, false);
                    self.continued = true;
                }
                self.lacing.push(0);
                break;
            }
        }
        if eos {
            self.flush(granule, true);
        } else if end_page || self.body.len() >= self.page_target {
            self.flush(granule, false);
        }
    }

    pub fn finish(mut self) -> Vec<u8> {
        if !self.lacing.is_empty() {
            self.flush(-1, false);
        }
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_round_trip_with_long_packets() {
        let mut w = Writer::new(7);
        let packets: Vec<Vec<u8>> = vec![
            vec![1; 30],
            vec![2; 255],
            vec![3; 510],
            vec![4; 70_000],
            vec![5; 0],
            vec![6; 4],
        ];
        for (i, p) in packets.iter().enumerate() {
            w.packet(p, (i as i64 + 1) * 100, i == 0, i == packets.len() - 1);
        }
        let bytes = w.finish();
        let (streams, bad) = read(&bytes).unwrap();
        assert_eq!(bad, 0);
        assert_eq!(streams.len(), 1);
        let got: Vec<&Vec<u8>> = streams[0].packets.iter().map(|p| &p.data).collect();
        assert_eq!(got, packets.iter().collect::<Vec<_>>());
        assert!(streams[0].packets.last().unwrap().eos);
        assert_eq!(streams[0].packets.last().unwrap().granule, 600);
    }

    #[test]
    fn damaged_page_is_skipped() {
        let mut w = Writer::new(1);
        w.packet(&[9; 100], 10, true, false);
        w.packet(&[8; 100], 20, true, false);
        w.packet(&[7; 100], 30, true, true);
        let mut bytes = w.finish();
        bytes[60] ^= 0xFF;
        let (streams, bad) = read(&bytes).unwrap();
        assert_eq!(bad, 1);
        assert_eq!(streams[0].packets.len(), 2);
    }
}
