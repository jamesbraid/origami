//! The PROM environment a stopped machine keeps in its clock chip's NVRAM.
//!
//! Each supported PROM keeps its environment variables at fixed offsets in
//! the `nvram0` store, behind a one-byte checksum. The layouts here were
//! measured by setting each variable at the PROM's Command Monitor and
//! comparing the store before and after. Stores edited here booted without
//! the PROM's checksum warning. The BASEIO checksum is the one Origami's
//! Timekeeper datasheet gives; the Octane checksum is fitted, and
//! `Layout::checksum` says what the fit does and does not establish.
//! Variables a PROM keeps elsewhere, such as an Octane's boot settings in
//! its flash, are refused rather than guessed.

use crate::{Offering, Result};
use std::fs;
use std::io::Write;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

/// Which PROM layout a store holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// The BASEIO PROM's environment in a 32 KiB Timekeeper: Origin 200,
    /// Origin 2000, Onyx2, Origin 300, Origin 3000, Fuel, Onyx 350 and
    /// Tezro.
    BaseIo,
    /// The IP30 PROM's environment in an Octane's 512-byte DS1687 image.
    Octane,
}

const BASEIO_TOPOLOGIES: &[&str] = &[
    "origin200",
    "origin200-dual",
    "origin200-gigachannel",
    "origin2000-deskside",
    "origin2000-rack",
    "onyx2-deskside",
    "onyx2-rack",
    "origin300",
    "origin3200",
    "origin3400",
    "origin3900",
    "onyx3000",
    "fuel",
    "onyx350-compat",
    "tezro-tower-multi",
];
const OCTANE_TOPOLOGIES: &[&str] = &["octane", "octane2"];

/// The store holding the console node's environment. A two-node Origin 200
/// has a second Timekeeper, but its PROM reads the environment from the first.
const STORE: &str = "nvram0";

impl Layout {
    pub fn for_offering(offering: &Offering) -> Result<Self> {
        let topology = offering.topology.as_str();
        let layout = if BASEIO_TOPOLOGIES.contains(&topology) {
            Layout::BaseIo
        } else if OCTANE_TOPOLOGIES.contains(&topology) {
            Layout::Octane
        } else {
            return Err(format!(
                "origami nvram does not know where {topology}'s PROM keeps its environment"
            )
            .into());
        };
        // A topology variant with a different clock part has no nvram0 of
        // the measured size.
        if !offering
            .storage
            .iter()
            .any(|item| item.name == STORE && item.size == layout.size() as u64)
        {
            return Err(format!(
                "origami nvram does not know {topology}'s NVRAM: it has no {} byte {STORE} store",
                layout.size()
            )
            .into());
        }
        Ok(layout)
    }

    fn size(self) -> usize {
        match self {
            Layout::BaseIo => 0x8000,
            Layout::Octane => 0x200,
        }
    }

    fn fields(self) -> &'static [Field] {
        match self {
            Layout::BaseIo => BASEIO_FIELDS,
            Layout::Octane => OCTANE_FIELDS,
        }
    }

    fn checksum_offset(self) -> usize {
        match self {
            Layout::BaseIo => 0x4bc,
            Layout::Octane => 0x0f,
        }
    }

    /// The checksum the PROM expects for `data`. Both PROMs fold each byte
    /// in with XOR and then rotate left by one, from offset 0x0e, skipping
    /// the checksum byte itself.
    fn checksum(self, data: &[u8]) -> u8 {
        let fold = |seed: u8, skip: &[usize], bytes: &mut dyn Iterator<Item = (usize, u8)>| {
            bytes.fold(seed, |sum, (offset, byte)| {
                let sum = if skip.contains(&offset) {
                    sum
                } else {
                    sum ^ byte
                };
                sum.rotate_left(1)
            })
        };
        match self {
            // The BASEIO PROM also skips 0x46f, which records why the machine
            // last restarted, and stops below 0x7fd1.
            Layout::BaseIo => fold(
                165,
                &[0x4bc, 0x46f],
                &mut (0x0e..0x7fd1).map(|i| (i, data[i])),
            ),
            // The IP30 PROM's checksum was fitted to 21 PROM-written images.
            // In all of them 0x0e to 0x10 and the extended RAM, which the
            // image keeps at 0xc0, held the same bytes, nonzero only at 0xc1
            // and 0xc9. The seed absorbs those constants, so the fit fixes
            // neither the exact start of the walk nor which extended bytes it
            // covers. Two boots narrow it: the PROM accepted an edit at 0x7a,
            // so bank-0 RAM is covered at least that far, and it rejected a
            // store whose checksum missed its own change at 0xc9, so that
            // byte is covered. Every variable written here changed among the
            // fitted images, so its place in the walk is fitted, not assumed.
            Layout::Octane => fold(
                0x68,
                &[0x0f],
                &mut (0x0e..0x80)
                    .chain(0xc0..0x140)
                    .enumerate()
                    .map(|(index, i)| (index + 0x0e, data[i])),
            ),
        }
    }

    /// Whether the PROM has written its environment here yet.
    fn initialised(self, data: &[u8]) -> bool {
        match self {
            // The BASEIO PROM stores layout revision 4 once it has written
            // its defaults. A new machine's image is zero there.
            Layout::BaseIo => data[0x0e] == 4,
            Layout::Octane => data[0x0e..0x80].iter().any(|&byte| byte != 0),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    /// One character from the list.
    Choice(&'static [&'static str]),
    /// `Y` or `N`, which `printenv` shows as Yes or No.
    YesNo,
    /// A baud rate as up to `width` ASCII digits, NUL-padded.
    Baud(usize),
    /// A NUL-terminated string in a slot of `width` bytes.
    Text(usize),
    /// Four big-endian address bytes.
    Ipv4Bytes,
    /// A dotted IPv4 address as a NUL-terminated string of up to `width` bytes.
    Ipv4Text(usize),
}

#[derive(Debug)]
struct Field {
    name: &'static str,
    offset: usize,
    kind: Kind,
}

const fn field(name: &'static str, offset: usize, kind: Kind) -> Field {
    Field { name, offset, kind }
}

const CONSOLES: &[&str] = &["d", "g"];
const DISKLESS: &[&str] = &["0", "1"];
const BAUD_RATES: &[&str] = &["300", "1200", "2400", "4800", "9600", "19200", "38400"];

// Measured on BASEIO PROM 6.156 (Origin 200) and 6.210 (Origin 300 and 3200), and
// checked by booting an edited store on each topology above. A string's slot
// runs to the next field: the PROM's own setenv rewrites the whole slot.
const BASEIO_FIELDS: &[Field] = &[
    field("console", 0x0f, Kind::Choice(CONSOLES)),
    field("SystemPartition", 0x91, Kind::Text(0x80)),
    field("OSLoader", 0x111, Kind::Text(0x80)),
    field("OSLoadFilename", 0x191, Kind::Text(0x80)),
    field("dbaud", 0x297, Kind::Baud(5)),
    field("diskless", 0x31c, Kind::Choice(DISKLESS)),
    field("TimeZone", 0x31d, Kind::Text(8)),
    field("OSLoadPartition", 0x325, Kind::Text(0x80)),
    field("AutoLoad", 0x3a5, Kind::YesNo),
    field("root", 0x3a6, Kind::Text(0x80)),
    field("netaddr", 0x428, Kind::Ipv4Bytes),
];

// Measured on the IP30 PROM 4.17 and checked on Octane and Octane2. The PROM
// keeps OSLoader and the other boot settings in its flash instead.
const OCTANE_FIELDS: &[Field] = &[
    field("console", 0x11, Kind::Choice(CONSOLES)),
    field("dbaud", 0x19, Kind::Baud(8)),
    field("diskless", 0x21, Kind::Choice(DISKLESS)),
    field("AutoLoad", 0x22, Kind::YesNo),
    // The 16-byte slot is inferred, not measured: the PROM's own write ran
    // to 0x3b, and 16 bytes hold the longest dotted address and its NUL.
    field("netaddr", 0x2b, Kind::Ipv4Text(16)),
];

/// Why a name the PROM knows is missing from this layout, if it is one.
fn not_stored(layout: Layout, name: &str) -> Option<String> {
    if name.eq_ignore_ascii_case("eaddr") {
        return Some(
            "eaddr is not kept in NVRAM: the PROM reads the Ethernet address from the \
             machine's hardware"
                .into(),
        );
    }
    let elsewhere = BASEIO_FIELDS
        .iter()
        .find(|field| field.name.eq_ignore_ascii_case(name))?;
    (layout == Layout::Octane).then(|| {
        format!(
            "an Octane PROM does not keep {} in its clock NVRAM, so origami nvram cannot \
             read or change it",
            elsewhere.name
        )
    })
}

/// A store's bytes and the layout they follow.
pub struct Store {
    layout: Layout,
    data: Vec<u8>,
}

impl Store {
    pub fn new(layout: Layout, data: Vec<u8>) -> Result<Self> {
        if data.len() != layout.size() {
            return Err(format!(
                "NVRAM store is {} bytes; expected {}",
                data.len(),
                layout.size()
            )
            .into());
        }
        Ok(Self { layout, data })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    /// Refuse a store the PROM has not written or would reject.
    pub fn check(&self) -> Result<()> {
        if !self.layout.initialised(&self.data) {
            return Err("PROM has not initialised NVRAM yet; boot once".into());
        }
        if !self.checksum_valid() {
            return Err(
                "the NVRAM checksum does not match its contents, so the PROM will \
                        reinitialise it at its next boot"
                    .into(),
            );
        }
        Ok(())
    }

    pub fn checksum_valid(&self) -> bool {
        self.data[self.layout.checksum_offset()] == self.layout.checksum(&self.data)
    }

    fn field(&self, name: &str) -> Result<&'static Field> {
        let fields = self.layout.fields();
        if let Some(field) = fields
            .iter()
            .find(|field| field.name.eq_ignore_ascii_case(name))
        {
            return Ok(field);
        }
        if let Some(reason) = not_stored(self.layout, name) {
            return Err(reason.into());
        }
        let names: Vec<_> = fields.iter().map(|field| field.name).collect();
        Err(format!(
            "unknown NVRAM variable {name}; this machine's are {}",
            names.join(", ")
        )
        .into())
    }

    /// Each known variable and its value, in store order.
    pub fn variables(&self) -> Vec<(&'static str, String)> {
        self.layout
            .fields()
            .iter()
            .map(|field| (field.name, self.read(field)))
            .collect()
    }

    /// The variable's canonical name and value.
    pub fn get(&self, name: &str) -> Result<(&'static str, String)> {
        let field = self.field(name)?;
        Ok((field.name, self.read(field)))
    }

    fn read(&self, field: &Field) -> String {
        let at = field.offset;
        match field.kind {
            Kind::Choice(_) => text(&self.data[at..at + 1]),
            Kind::YesNo => match self.data[at] {
                b'Y' => "Yes".into(),
                b'N' => "No".into(),
                _ => text(&self.data[at..at + 1]),
            },
            Kind::Baud(width) | Kind::Text(width) | Kind::Ipv4Text(width) => {
                text(&self.data[at..at + width])
            }
            Kind::Ipv4Bytes => {
                let bytes: [u8; 4] = self.data[at..at + 4].try_into().unwrap();
                Ipv4Addr::from(bytes).to_string()
            }
        }
    }

    /// Set a variable and recompute the checksum. Returns its canonical name.
    pub fn set(&mut self, name: &str, value: &str) -> Result<&'static str> {
        let field = self.field(name)?;
        let encoded = encode(field, value)?;
        self.data[field.offset..field.offset + encoded.len()].copy_from_slice(&encoded);
        let offset = self.layout.checksum_offset();
        self.data[offset] = self.layout.checksum(&self.data);
        Ok(field.name)
    }
}

/// The bytes up to the first NUL, with anything unprintable escaped.
fn text(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    bytes[..end]
        .iter()
        .flat_map(|&b| std::ascii::escape_default(b))
        .map(char::from)
        .collect()
}

fn encode(field: &Field, value: &str) -> Result<Vec<u8>> {
    let name = field.name;
    let padded = |width: usize| {
        let mut bytes = value.as_bytes().to_vec();
        bytes.resize(width, 0);
        bytes
    };
    match field.kind {
        Kind::Choice(choices) => {
            if !choices.contains(&value) {
                return Err(format!("{name} must be {}", choices.join(" or ")).into());
            }
            Ok(value.as_bytes().to_vec())
        }
        Kind::YesNo => match value.to_ascii_lowercase().as_str() {
            "yes" => Ok(b"Y".to_vec()),
            "no" => Ok(b"N".to_vec()),
            _ => Err(format!("{name} must be Yes or No").into()),
        },
        Kind::Baud(width) => {
            if !BAUD_RATES.contains(&value) {
                return Err(format!("{name} must be one of {}", BAUD_RATES.join(", ")).into());
            }
            Ok(padded(width))
        }
        Kind::Text(width) => {
            if value.is_empty()
                || value.len() >= width
                || !value.bytes().all(|b| b.is_ascii_graphic())
            {
                return Err(format!(
                    "{name} must be 1 to {} printable ASCII characters without spaces",
                    width - 1
                )
                .into());
            }
            Ok(padded(width))
        }
        Kind::Ipv4Bytes | Kind::Ipv4Text(_) => {
            let address: Ipv4Addr = value
                .parse()
                .map_err(|_| format!("{name} must be an IPv4 address such as 192.0.2.10"))?;
            Ok(match field.kind {
                Kind::Ipv4Bytes => address.octets().to_vec(),
                Kind::Ipv4Text(width) => {
                    let mut bytes = address.to_string().into_bytes();
                    bytes.resize(width, 0);
                    bytes
                }
                _ => unreachable!(),
            })
        }
    }
}

fn store_path(dir: &Path) -> PathBuf {
    dir.join("state").join(format!("{STORE}.raw"))
}

/// Read `dir`'s PROM environment store. The caller holds the edit lock.
pub fn open(dir: &Path, offering: &Offering) -> Result<Store> {
    let layout = Layout::for_offering(offering)?;
    let path = store_path(dir);
    let data =
        fs::read(&path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let store = Store::new(layout, data).map_err(|error| format!("{}: {error}", path.display()))?;
    store.check()?;
    Ok(store)
}

/// Replace `dir`'s store with `store`, so an interrupted write leaves the
/// old one in place.
pub fn save(dir: &Path, store: &Store) -> Result<()> {
    let path = store_path(dir);
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    fs::set_permissions(file.path(), fs::metadata(&path)?.permissions())?;
    file.write_all(store.bytes())?;
    file.as_file().sync_all()?;
    file.persist(&path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Stores the PROMs wrote on their first boot, cut down to the measured
    // variables, the revision byte and a recomputed checksum. Everything
    // else, which includes text the PROM copied from its own image, is zero.
    const ORIGIN200: &[u8] = include_bytes!("../tests/fixtures/nvram/origin200-ip27prom-6.156.raw");
    const OCTANE: &[u8] = include_bytes!("../tests/fixtures/nvram/octane-ip30prom-4.17.raw");
    // An Origin 3200 after an IRIX install, scrubbed the same way. The PROM
    // 6.210 store passed the BASEIO checksum unscrubbed, and so did an
    // Origin 3400, an Origin 3900 and an Onyx 3000 after one boot.
    const ORIGIN3200: &[u8] =
        include_bytes!("../tests/fixtures/nvram/origin3200-ip35prom-6.210.raw");

    fn store(layout: Layout, data: &[u8]) -> Store {
        Store::new(layout, data.to_vec()).unwrap()
    }

    #[test]
    fn checksums_of_the_fixtures_match() {
        for (layout, data, at, expected) in [
            (Layout::BaseIo, ORIGIN200, 0x4bc, 0xaf),
            (Layout::Octane, OCTANE, 0x0f, 0x2a),
        ] {
            assert_eq!(layout.checksum(data), expected, "{layout:?}");
            assert_eq!(data[at], expected, "{layout:?}");
            store(layout, data).check().unwrap();
        }
    }

    /// The unscrubbed first-boot captures stay out of the repository, because
    /// they hold text the PROM copied from its own image. Point
    /// ORIGAMI_NVRAM_CAPTURES at the directory holding
    /// o200-nvram-initialised.raw and octane-nvram-initialised.raw to check
    /// the checksum against the PROM's own: 0x4d and 0x6e.
    #[test]
    #[ignore = "needs ORIGAMI_NVRAM_CAPTURES"]
    fn checksums_of_prom_written_stores_match() {
        let dir = std::path::PathBuf::from(
            std::env::var_os("ORIGAMI_NVRAM_CAPTURES").expect("set ORIGAMI_NVRAM_CAPTURES"),
        );
        for (layout, name, at, expected) in [
            (Layout::BaseIo, "o200-nvram-initialised.raw", 0x4bc, 0x4d),
            (Layout::Octane, "octane-nvram-initialised.raw", 0x0f, 0x6e),
        ] {
            let data = fs::read(dir.join(name)).unwrap();
            assert_eq!(data[at], expected, "{name}");
            assert_eq!(layout.checksum(&data), expected, "{name}");
            store(layout, &data).check().unwrap();
        }
    }

    #[test]
    fn the_origin_3000_family_keeps_the_baseio_layout() {
        let machines = crate::test_machines();
        let offering = crate::preset(&machines, "origin3200").unwrap();
        assert_eq!(Layout::for_offering(offering).unwrap(), Layout::BaseIo);
        let nvram = store(Layout::BaseIo, ORIGIN3200);
        nvram.check().unwrap();
        assert_eq!(nvram.get("console").unwrap().1, "d");
        assert_eq!(nvram.get("AutoLoad").unwrap().1, "Yes");
        assert_eq!(nvram.get("root").unwrap().1, "dks0d1s0");
        assert_eq!(nvram.get("netaddr").unwrap().1, "192.0.2.15");
    }

    #[test]
    fn baseio_checksum_matches_the_datasheet_vectors() {
        let mut data = vec![0; 0x8000];
        assert_eq!(Layout::BaseIo.checksum(&data), 0x2d);
        data[0x0e] = 4;
        data[0x3883] = 1;
        assert_eq!(Layout::BaseIo.checksum(&data), 0x4d);
        data[0x46f] = 0x5a;
        data[0x4bc] = 0x5a;
        assert_eq!(Layout::BaseIo.checksum(&data), 0x4d);
    }

    #[test]
    fn set_then_get_round_trips_and_keeps_the_checksum_valid() {
        for (layout, data) in [(Layout::BaseIo, ORIGIN200), (Layout::Octane, OCTANE)] {
            let mut nvram = store(layout, data);
            for (name, value) in [
                ("console", "g"),
                ("autoload", "yes"),
                ("netaddr", "10.11.12.13"),
                ("dbaud", "19200"),
                ("diskless", "1"),
            ] {
                nvram.set(name, value).unwrap();
            }
            nvram.check().unwrap();
            let reread = Store::new(layout, nvram.bytes().to_vec()).unwrap();
            let values: Vec<_> = reread.variables();
            for (name, value) in [
                ("console", "g"),
                ("AutoLoad", "Yes"),
                ("netaddr", "10.11.12.13"),
                ("dbaud", "19200"),
                ("diskless", "1"),
            ] {
                assert!(
                    values.contains(&(name, value.to_string())),
                    "{layout:?} {name} {values:?}"
                );
            }
            assert_eq!(reread.get("DBAUD").unwrap(), ("dbaud", "19200".into()));
        }
    }

    #[test]
    fn prom_defaults_read_back() {
        let origin = store(Layout::BaseIo, ORIGIN200);
        assert_eq!(origin.get("console").unwrap().1, "d");
        assert_eq!(origin.get("AutoLoad").unwrap().1, "No");
        assert_eq!(origin.get("OSLoadPartition").unwrap().1, "dksc(0,1,0)");
        assert_eq!(origin.get("TimeZone").unwrap().1, "PST8PDT");
        assert_eq!(origin.get("netaddr").unwrap().1, "192.0.2.1");
        let octane = store(Layout::Octane, OCTANE);
        assert_eq!(octane.get("console").unwrap().1, "g");
        assert_eq!(octane.get("AutoLoad").unwrap().1, "Yes");
        assert_eq!(octane.get("dbaud").unwrap().1, "9600");
        assert_eq!(octane.get("netaddr").unwrap().1, "192.0.2.1");
    }

    #[test]
    fn a_shorter_string_clears_the_rest_of_its_slot() {
        let mut nvram = store(Layout::BaseIo, ORIGIN200);
        nvram.set("OSLoadPartition", "dksc(0,2,0)").unwrap();
        nvram.set("dbaud", "9600").unwrap();
        assert_eq!(&nvram.bytes()[0x297..0x29c], b"9600\0");
        assert!(nvram.bytes()[0x325 + 11..0x3a5].iter().all(|&b| b == 0));
        assert_eq!(nvram.get("AutoLoad").unwrap().1, "No");
    }

    #[test]
    fn refuses_invalid_values_and_unknown_or_unstored_names() {
        let mut origin = store(Layout::BaseIo, ORIGIN200);
        for (name, value) in [
            ("console", "x"),
            ("AutoLoad", "maybe"),
            ("dbaud", "1234567"),
            ("netaddr", "1.2.3"),
            ("diskless", "7"),
            ("TimeZone", "TOOLONGTZ"),
            ("OSLoader", "has space"),
            ("bootfile", "x"),
            ("eaddr", "08:00:69:00:00:01"),
        ] {
            assert!(origin.set(name, value).is_err(), "{name}={value}");
        }
        assert_eq!(origin.bytes(), ORIGIN200);
        let mut octane = store(Layout::Octane, OCTANE);
        let error = octane.set("OSLoader", "sash").unwrap_err().to_string();
        assert!(error.contains("does not keep OSLoader"), "{error}");
        assert!(octane.get("eaddr").is_err());
    }

    #[test]
    fn a_store_the_prom_has_not_written_is_refused() {
        let blank = store(Layout::Octane, &[0; 0x200]);
        assert_eq!(
            blank.check().unwrap_err().to_string(),
            "PROM has not initialised NVRAM yet; boot once"
        );
        // A new Origin 200's store holds only the clock's leap-cycle record.
        let mut fresh = vec![0; 0x8000];
        fresh[0x3883] = 1;
        fresh[0x4bc] = 0x6d;
        assert!(store(Layout::BaseIo, &fresh).check().is_err());
        let mut damaged = ORIGIN200.to_vec();
        damaged[0x20] ^= 1;
        let error = store(Layout::BaseIo, &damaged).check().unwrap_err();
        assert!(error.to_string().contains("checksum"), "{error}");
    }
}
