//! Creep names as identity (ROADMAP P2, the minimal spawn path). The bot keeps no creep state in
//! `Memory`, so a creep's role, home room and serial are its name, which survives every global
//! reset and deploy for the creep's whole life (`.claude/rules/wasm-bot.md`).
//!
//! Format, one ASCII string: the role's code (one lowercase letter), the home room's name in its
//! canonical form (`W`/`E` and `N`/`S` uppercase, no leading zeros), `-`, then the serial in base 36
//! with lowercase digits and no leading zeros. For example `wW1N1-5vosf7k` for a worker of W1N1 ordered first on tick 50000000.
//! At most [`NAME_MAX`] bytes, under the engine's `extra::CREEP_NAME_MAX_LENGTH` (V171).
//!
//! This is a persisted format: creeps alive when a new build deploys still carry names an older
//! build gave them. Never change a role's code or reuse a retired one; a format change must keep
//! parsing the old form until no own creep carries it: at least `CREEP_LIFE_TIME` +
//! `MAX_CREEP_SIZE` × `CREEP_SPAWN_TIME` ticks after the last old-form order (the life clock starts
//! when the creep leaves the spawn, V170), longer while a spawn's exits are blocked (V174), and with
//! no bound once creeps are renewed. A name that does not parse (another
//! script's creep, a creep named in the console) has no identity: no job counts or drives it.

use rustedscreep_consts::{RoomName, extra::HALF_WORLD_SIZE};

use crate::snapshot::parse_room_name;

/// A creep's job, as its name records it. P2's opening added the worker; P3 adds its roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    /// The bootstrap generalist: harvests, carries, builds and upgrades (RCL1–3 opening).
    Worker,
    /// A static miner: stands on its source's mining tile and harvests (P3.1).
    Miner,
    /// A hauler: carries a mined source's energy to the spawns, the extensions and the controller
    /// container (P3.3).
    Hauler,
    /// An upgrader: stands at a post next to the controller container or a controller link,
    /// withdraws and upgrades (P3.4).
    Upgrader,
}

impl Role {
    /// Every role, in a fixed order.
    pub const ALL: [Self; 4] = [Self::Worker, Self::Miner, Self::Hauler, Self::Upgrader];

    /// The first byte of the role's creep names. Never change or reuse one (see the module docs).
    pub const fn code(self) -> u8 {
        match self {
            Self::Worker => b'w',
            Self::Miner => b'm',
            Self::Hauler => b'h',
            Self::Upgrader => b'u',
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.code() == code)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Worker => "worker",
            Self::Miner => "miner",
            Self::Hauler => "hauler",
            Self::Upgrader => "upgrader",
        }
    }

    /// The role named `name` ([`Self::as_str`]), for the console.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_str() == name)
    }
}

/// What a creep's name says about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Identity {
    pub role: Role,
    /// The room it works for.
    pub home: RoomName,
    /// Unique among the bot's creeps on its shard: from `Game.time` and the order's index in its
    /// tick ([`serial`]).
    pub serial: u64,
}

/// Longest name [`CreepName::encode`] makes: the role code, the longest room name (`W127N127`),
/// `-` and `u64::MAX` in base 36 (13 digits). Unit: bytes (ASCII, so also UTF-16 units).
pub const NAME_MAX: usize = 1 + 8 + 1 + 13;

/// Spawn orders one tick may name (`serial`'s low byte). Unit: orders.
pub const ORDERS_PER_TICK: u32 = 256;

/// The serial of the `index`-th spawn order of tick `time`, or `None` from the
/// [`ORDERS_PER_TICK`]-th on. Names of one tick differ by index and names of different ticks by
/// time, so no two orders ever share a name (the engine refuses a name `Game.creeps` holds or one
/// named earlier in the tick, V171, and makes a second creep of a name suicide, V176).
pub fn serial(time: u32, index: u32) -> Option<u64> {
    (index < ORDERS_PER_TICK).then(|| (u64::from(time) << 8) | u64::from(index))
}

/// A creep name in the format of the module docs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CreepName {
    bytes: [u8; NAME_MAX],
    len: u8,
}

impl CreepName {
    /// The name for `id`.
    pub fn encode(id: &Identity) -> Self {
        let mut name = Self {
            bytes: [0; NAME_MAX],
            len: 0,
        };
        name.push(id.role.code());
        name.push_room(id.home);
        name.push(b'-');
        name.push_base36(id.serial);
        name
    }

    /// The identity a name holds, or `None` unless `bytes` is exactly the name [`Self::encode`]
    /// makes for it (so lowercase room letters, leading zeros or trailing bytes do not parse).
    pub fn parse(bytes: &[u8]) -> Option<Identity> {
        let (&code, rest) = bytes.split_first()?;
        let role = Role::from_code(code)?;
        let dash = rest.iter().position(|&b| b == b'-')?;
        let home = parse_room_name(rest.get(..dash)?)?;
        let serial = parse_base36(rest.get(dash + 1..)?)?;
        let id = Identity { role, home, serial };
        (Self::encode(&id).as_bytes() == bytes).then_some(id)
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.bytes.get(..usize::from(self.len)).unwrap_or(&[])
    }

    /// The name as text. It is ASCII by construction, so this never fails in practice.
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(self.as_bytes()).unwrap_or("")
    }

    fn push(&mut self, byte: u8) {
        if let Some(slot) = self.bytes.get_mut(usize::from(self.len)) {
            *slot = byte;
            self.len += 1;
        }
    }

    /// The room's canonical name: `W` n (room x = -n - 1) or `E` n (x = n), then `N` or `S` the same
    /// way (V169). Written by hand: `RoomName`'s `Display` goes through `core::fmt`.
    ///
    /// Uses: `extra::HALF_WORLD_SIZE` (through the packed room bytes).
    fn push_room(&mut self, room: RoomName) {
        let [x, y] = room.packed_repr().to_be_bytes();
        let (west, n) = split_axis(x);
        self.push(if west { b'W' } else { b'E' });
        self.push_decimal(n);
        let (north, n) = split_axis(y);
        self.push(if north { b'N' } else { b'S' });
        self.push_decimal(n);
    }

    fn push_decimal(&mut self, n: u8) {
        if n >= 100 {
            self.push(b'0' + n / 100);
        }
        if n >= 10 {
            self.push(b'0' + n / 10 % 10);
        }
        self.push(b'0' + n % 10);
    }

    fn push_base36(&mut self, mut n: u64) {
        let mut digits = [0u8; 13];
        let mut count = 0;
        loop {
            let d = (n % 36) as u8;
            if let Some(slot) = digits.get_mut(count) {
                *slot = if d < 10 { b'0' + d } else { b'a' + d - 10 };
            }
            count += 1;
            n /= 36;
            if n == 0 {
                break;
            }
        }
        for i in (0..count).rev() {
            if let Some(&d) = digits.get(i) {
                self.push(d);
            }
        }
    }
}

impl std::fmt::Debug for CreepName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `extra::HALF_WORLD_SIZE` as the packed room byte it offsets (the test checks the cast).
const HALF: u8 = HALF_WORLD_SIZE as u8;

/// A packed room byte as (negative side, number): bytes below `HALF_WORLD_SIZE` are `W`/`N`
/// (coordinate = byte − HALF_WORLD_SIZE = −n − 1), the rest `E`/`S` (n = byte − HALF_WORLD_SIZE).
fn split_axis(byte: u8) -> (bool, u8) {
    if byte < HALF {
        (true, HALF - 1 - byte)
    } else {
        (false, byte - HALF)
    }
}

/// Lowercase base-36 digits, at least one, without overflow.
fn parse_base36(digits: &[u8]) -> Option<u64> {
    if digits.is_empty() {
        return None;
    }
    digits.iter().try_fold(0u64, |n, &b| {
        let d = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'z' => b - b'a' + 10,
            _ => return None,
        };
        n.checked_mul(36)?.checked_add(u64::from(d))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustedscreep_consts::extra::CREEP_NAME_MAX_LENGTH;

    fn room(name: &str) -> RoomName {
        name.parse().expect("room name")
    }

    fn worker(home: &str, serial: u64) -> Identity {
        Identity {
            role: Role::Worker,
            home: room(home),
            serial,
        }
    }

    #[test]
    fn names_fit_the_engine_limit() {
        const { assert!(NAME_MAX <= CREEP_NAME_MAX_LENGTH) };
        let longest = CreepName::encode(&worker("W127N127", u64::MAX));
        assert_eq!(longest.as_bytes().len(), NAME_MAX);
        assert_eq!(longest.as_str(), "wW127N127-3w5e11264sgsf");
    }

    #[test]
    fn the_half_world_byte_matches_the_facade() {
        assert_eq!(i32::from(HALF), HALF_WORLD_SIZE);
    }

    #[test]
    fn a_pinned_example() {
        let id = worker("E4N59", serial(84_277_402, 3).expect("serial"));
        let name = CreepName::encode(&id);
        assert_eq!(id.serial, (84_277_402u64 << 8) | 3);
        assert_eq!(name.as_str(), "wE4N59-9wt7g1v");
        assert_eq!(CreepName::parse(name.as_bytes()), Some(id));
    }

    /// Every room name round trips, as does every serial width.
    #[test]
    fn every_room_and_serial_width_round_trips() {
        for packed in 0..=u16::MAX {
            let home = RoomName::from_packed(packed);
            let id = Identity {
                role: Role::Worker,
                home,
                serial: u64::from(packed),
            };
            let name = CreepName::encode(&id);
            assert!(name.as_str().starts_with(&format!("w{home}-")), "{name:?}");
            assert_eq!(CreepName::parse(name.as_bytes()), Some(id), "{name:?}");
        }
        let mut s = 1u64;
        while let Some(next) = s.checked_mul(36) {
            for serial in [s - 1, s, next - 1] {
                let id = worker("W1N1", serial);
                assert_eq!(
                    CreepName::parse(CreepName::encode(&id).as_bytes()),
                    Some(id)
                );
            }
            s = next;
        }
    }

    #[test]
    fn only_canonical_names_parse() {
        for bad in [
            "",
            "w",
            "wW1N1",
            "wW1N1-",
            "xW1N1-1",
            "WW1N1-1",
            "ww1n1-1",
            "wW01N1-1",
            "wW1N1-01",
            "wW1N1-1A",
            "wW1N1-1-",
            "wW1N1-1 ",
            "wW128N1-1",
            "wW1N1-3w5e11264sgsg",
            "Probe1",
            "Harvester",
        ] {
            assert_eq!(CreepName::parse(bad.as_bytes()), None, "{bad:?}");
        }
        assert!(CreepName::parse(b"wW1N1-0").is_some());
    }

    #[test]
    fn serials_are_unique_per_tick_and_index() {
        assert_eq!(serial(7, ORDERS_PER_TICK), None);
        let mut seen = std::collections::HashSet::new();
        for time in [0, 1, 2, u32::MAX - 1, u32::MAX] {
            for index in 0..ORDERS_PER_TICK {
                assert!(seen.insert(serial(time, index).expect("serial")));
            }
        }
    }

    #[test]
    fn roles_round_trip_by_code_and_name() {
        for role in Role::ALL {
            assert_eq!(Role::from_code(role.code()), Some(role));
            assert_eq!(Role::from_name(role.as_str()), Some(role));
            assert!(role.code().is_ascii_lowercase());
        }
        let codes: std::collections::HashSet<u8> = Role::ALL.iter().map(|r| r.code()).collect();
        assert_eq!(codes.len(), Role::ALL.len(), "codes are unique");
        assert_eq!(Role::from_name("nobody"), None);
        // The codes creeps alive today carry (a persisted format, module docs).
        assert_eq!(
            Role::ALL.map(Role::code),
            [b'w', b'm', b'h', b'u'],
            "never change or reuse a code"
        );
    }
}
