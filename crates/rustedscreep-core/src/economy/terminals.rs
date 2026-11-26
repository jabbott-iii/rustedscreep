//! Terminal balancing (ROADMAP P9.4, ADR-047): the owned rooms' own terminals send each other
//! the base minerals, so every terminal holds up to `terminals.mineral_keep` of each from the
//! terminals that hold more (the user's two choices: the base minerals move, and the source
//! haulers fill a send's fee).
//!
//! A terminal (an RCL6 structure that holds up to `TERMINAL_CAPACITY` of any resource) can
//! `send` a resource to a terminal in another room once every `TERMINAL_COOLDOWN` ticks, paying a
//! fee in energy from its own store. Why the base minerals: a room's labs react only from its own
//! terminal (ADR-045), and a room mines only its own mineral (ADR-044), so without imports no
//! room has the two reagents of a first reaction. Compounds and bars stay where they were made,
//! until P10's boosting or P9.5's market moves them.
//!
//! - **The members.** Each owned room with an own terminal at a level that allows one
//!   (`CONTROLLER_STRUCTURES`, from RCL6): a terminal below its level takes nothing (V439), so it
//!   neither gives nor receives. Only own terminals read this tick are destinations, since the
//!   engine delivers to any player's terminal and drops a send to a room without one, with no
//!   refusal (V437, V438).
//! - **The fee** ([`fee`]): `ceil(amount × (1 − e^(−range / TERMINAL_SEND_COST_SCALE)))` energy,
//!   paid by the sender (V432), where the range is the larger of the two rooms' horizontal and
//!   vertical distance ([`range`], V433). The engine wraps the distance around the world's
//!   edge; [`range`] does not, so its fee is never below the engine's.
//! - **The plan** ([`plan`]). Each member that holds less than `terminals.mineral_keep` of a base
//!   mineral, by at least `terminals.send_min`, needs the difference; each member gives only what
//!   it holds above the keep. The needs are served largest first; each from the giver nearest to
//!   it (the lowest fee), then the one with the most to give. A send moves the least of the need,
//!   the giver's surplus, the receiver's free room and the most `terminals.energy_keep` pays the
//!   fee of, and nothing under `terminals.send_min`. A giver that lacks the fee and whose
//!   terminal no hauler can fill (its sources are not hauled, or it is full) is passed over for
//!   the next. A terminal sends at most once a plan (one `send` a tick, V436), and a receiver gets
//!   each mineral from one giver a plan. A receiver ends at most at the keep, and a giver at least
//!   at it, so nothing flows back. Every fee leaves [`FEE_MARGIN`] of the energy unspent.
//! - **The fee's energy** ([`Terminals::banks`], the user's choice). A planned send whose giver
//!   lacks the fee is cut to what its energy pays for when that is at least `terminals.send_min`;
//!   else it waits. While it waits in a room whose sources are hauled, and while the Standard tier
//!   runs, the terminal joins the source haulers' fills after the spawns, extensions and towers,
//!   up to `terminals.energy_keep` (`economy::Hauling::set_banks`, the bank ADR-046 opened for the
//!   factory), and its energy alone is read each tick. With fewer than two members, or off, no
//!   send waits and no bank opens.
//! - **A creep at the giver's terminal** ([`Wait::Busy`]). A ready send whose giver's terminal a
//!   creep withdraws from on the same tick waits for the next: the engine makes both in its room
//!   pass in an order the bot does not know, and drops a send left short there after the game side
//!   answered OK (V436).
//!
//! The reads (the snapshot's `TerminalSites`): with fewer than two members nothing is read. With
//! two or more, every member is read in full (its cooldown, free room, energy and base minerals)
//! on the tick its plan is due ([`Terminals::reads`]): `terminals.read_ticks` after the last plan,
//! `TERMINAL_COOLDOWN` after one that ordered a send (the senders can send again, and more may be
//! needed), when a giver waiting on its cooldown comes off it, and on the tick after a waiting
//! giver's energy read shows its fee. The plan runs only on a tick every member was read, so it
//! never works from a stock another send has changed.
//!
//! One tick, as rustedscreep-wasm drives it:
//! 1. [`Terminals::begin`] after the snapshot: the members and this tick's reads.
//! 2. [`Terminals::set_hauled`] before the hauling job: the banks for the haulers.
//! 3. The Standard `terminals` job runs [`Terminals::run`]: the plan on a read tick (the sends
//!    [`Terminals::orders`] gives the act step), or the waiting givers' fees on another.
//! 4. [`Terminals::reads`]: the terminals the next tick's snapshot reads, in full or for their
//!    energy alone.
//!
//! Everything is kept in the heap. After a global reset the members are read on the next tick
//! the Standard tier runs and planned from scratch. Nothing is read from or written to `Memory`.

use std::cmp::Reverse;

use rustedscreep_consts::{
    Position, ResourceType, RoomName, StructureType, TERMINAL_COOLDOWN, TERMINAL_SEND_COST_SCALE,
};

use crate::config::TerminalsConfig;
use crate::intents::{IntentBuffer, Outcome};
use crate::scheduler::{Commit, Fault};
use crate::snapshot::{
    BALANCED_RESOURCES, BalancedStock, Controller, Snapshot, StructureHandle, TerminalRead,
};

/// The fee, in energy from the sender's terminal, of sending `amount` of a resource `range` rooms
/// (V432): `ceil(amount × (1 − e^(−range / TERMINAL_SEND_COST_SCALE)))`. For energy itself the
/// terminal also needs the amount (V435).
///
/// Uses: `TERMINAL_SEND_COST_SCALE` (V430, V432).
pub fn fee(amount: u32, range: u32) -> u32 {
    let raw = (f64::from(amount) * share(range)).ceil();
    if raw <= 0.0 {
        0
    } else if raw >= f64::from(u32::MAX) {
        u32::MAX
    } else {
        raw as u32
    }
}

/// The share of an amount the fee takes over `range` rooms: `1 − e^(−range / scale)`, in
/// `[0, 1)`.
///
/// Uses: `TERMINAL_SEND_COST_SCALE`.
fn share(range: u32) -> f64 {
    let scale = f64::from(TERMINAL_SEND_COST_SCALE.max(1));
    1.0 - (-f64::from(range) / scale).exp()
}

/// The distance in rooms the fee is computed over: the larger of the two rooms' horizontal and
/// vertical distance (`calcRoomsDistance`, V433; the coordinates of `RoomName::x_coord` and
/// `y_coord` are the engine's `roomNameToXY`, V446). The engine wraps it around the world's edge,
/// which can only make it shorter, so [`fee`] over this range is never below the engine's.
pub fn range(a: RoomName, b: RoomName) -> u32 {
    let dx = a.x_coord().abs_diff(b.x_coord());
    let dy = a.y_coord().abs_diff(b.y_coord());
    dx.max(dy)
}

/// The most of a resource other than energy that `energy` pays the fee of over `range` rooms:
/// the largest amount whose [`fee`] is at most `energy` (`u32::MAX` at range 0, which is free).
///
/// Uses: `TERMINAL_SEND_COST_SCALE` (through [`fee`]).
pub fn affordable(energy: u32, range: u32) -> u32 {
    let s = share(range);
    if s <= 0.0 {
        return u32::MAX;
    }
    let guess = (f64::from(energy) / s).floor();
    let mut a = if guess <= 0.0 {
        0
    } else if guess >= f64::from(u32::MAX) {
        u32::MAX
    } else {
        guess as u32
    };
    // The guess is within a unit or two of the answer; the loops settle rounding either way.
    for _ in 0..4 {
        if a > 0 && fee(a, range) > energy {
            a -= 1;
        } else {
            break;
        }
    }
    for _ in 0..4 {
        if a < u32::MAX && fee(a.saturating_add(1), range) <= energy {
            a = a.saturating_add(1);
        } else {
            break;
        }
    }
    a
}

/// The energy a terminal keeps beyond a send's fee: the engine rounds the fee up from JavaScript's
/// `Math.exp` and [`fee`] from Rust's `f64::exp`, which may differ in the last digit, so a fee on
/// an exact boundary could come out 1 below the engine's (the hot-path review, 2026-11-26).
/// Unit: energy.
pub const FEE_MARGIN: u32 = 1;

/// What `energy` may spend on a fee: all of it but [`FEE_MARGIN`].
const fn spendable(energy: u32) -> u32 {
    energy.saturating_sub(FEE_MARGIN)
}

/// Why a planned send is not made this tick, or that it is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Wait {
    /// The giver is off its cooldown and holds the fee: the send is ordered this tick.
    #[default]
    Ready,
    /// The giver is on its cooldown (or held after a refused send): planned again when it ends.
    Cooldown,
    /// The giver lacks the fee: its terminal takes the haulers' energy ([`Terminals::banks`]).
    Fee,
    /// A creep withdraws from the giver's terminal this tick, which could leave it short when the
    /// engine makes the send (V436): planned again on the next tick ([`Terminals::run`]).
    Busy,
}

impl Wait {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Cooldown => "on its cooldown",
            Self::Fee => "waits for its fee",
            Self::Busy => "waits for a creep's withdrawal",
        }
    }
}

/// One planned send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Send {
    pub from: RoomName,
    pub to: RoomName,
    pub kind: ResourceType,
    /// Unit: units.
    pub amount: u32,
    /// Its [`fee`] from `from`'s terminal. Unit: energy.
    pub fee: u32,
    /// Its [`range`]. Unit: rooms.
    pub range: u32,
    pub wait: Wait,
}

/// The planner's buffers, kept between plans so a plan allocates nothing once warm.
#[derive(Default)]
pub struct PlanScratch {
    /// (need, mineral index, receiver index).
    needs: Vec<(u32, usize, usize)>,
    /// Each member's surplus above the keep, by mineral.
    left: Vec<BalancedStock>,
    /// Each member's free room less the sends planned to it.
    free: Vec<u32>,
    /// Whether each member already sends in this plan (or may not give).
    busy: Vec<bool>,
}

/// What member `i` may do in a plan ([`plan`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Giving {
    /// It may give (it is not held after a refused send).
    pub may_give: bool,
    /// Its terminal can be filled with a fee it lacks (its sources are hauled, so the source
    /// haulers can bring it, [`Terminals::banks`]). A giver that lacks the fee and cannot be
    /// filled is passed over, so a need is never left waiting on it.
    pub bankable: bool,
}

/// A giver chosen for a need in [`plan`]: its rank (range, then the most surplus first, then its
/// room's packed name), its index, the amount and the wait.
type Choice = ((u32, Reverse<u32>, u16), usize, u32, Wait);

/// What `giver` would send of a need of `need` over `range` rooms to a receiver with
/// `room_free` free (module docs): the least of the need, the giver's `surplus`, the free room and
/// what `terminals.energy_keep` pays the fee of, and how it waits; `None` under
/// `terminals.send_min`, or when it lacks the fee and is not `bankable` or has no free room for
/// the haulers' energy. Every fee leaves [`FEE_MARGIN`] unspent.
fn offer(
    need: u32,
    surplus: u32,
    room_free: u32,
    rg: u32,
    giver: &TerminalRead,
    bankable: bool,
    cfg: &TerminalsConfig,
) -> Option<(u32, Wait)> {
    let min = cfg.send_min.max(1);
    let amount = need
        .min(surplus)
        .min(room_free)
        .min(affordable(spendable(cfg.energy_keep), rg));
    if amount < min {
        return None;
    }
    if giver.cooldown > 0 {
        // Whether it then holds the fee is judged when the cooldown ends.
        return Some((amount, Wait::Cooldown));
    }
    let energy = spendable(giver.energy);
    if fee(amount, rg) <= energy {
        return Some((amount, Wait::Ready));
    }
    let paid = amount.min(affordable(energy, rg));
    if paid >= min {
        Some((paid, Wait::Ready))
    } else if bankable && giver.free > 0 {
        Some((amount, Wait::Fee))
    } else {
        None
    }
}

/// The sends for terminals read as `reads` (module docs), into `out` in the order they were
/// planned: the needs largest first (then by mineral, then by receiver), each served by the
/// nearest giver with at least `terminals.send_min` above the keep (then the one with the most,
/// then by room) whose offer stands (the private `offer`). `giving(i)` says what member `i` may do. Reads
/// are in any order; ties break on the rooms' packed names, so the plan is the same for the same
/// reads.
///
/// Uses: `TERMINAL_SEND_COST_SCALE` (through [`fee`] and [`affordable`]).
pub fn plan(
    reads: &[TerminalRead],
    giving: impl Fn(usize) -> Giving,
    cfg: &TerminalsConfig,
    scratch: &mut PlanScratch,
    out: &mut Vec<Send>,
) {
    out.clear();
    let PlanScratch {
        needs,
        left,
        free,
        busy,
    } = scratch;
    needs.clear();
    left.clear();
    free.clear();
    busy.clear();
    let keep = cfg.mineral_keep;
    let min = cfg.send_min.max(1);
    for (ri, r) in reads.iter().enumerate() {
        left.push(r.stock.map(|n| n.saturating_sub(keep)));
        free.push(r.free);
        busy.push(!giving(ri).may_give);
        for (mi, &n) in r.stock.iter().enumerate() {
            let need = keep.saturating_sub(n);
            if need >= min {
                needs.push((need, mi, ri));
            }
        }
    }
    let name = |i: usize| reads.get(i).map_or(0, |r| r.room.packed_repr());
    needs.sort_unstable_by_key(|&(need, mi, ri)| (Reverse(need), mi, name(ri)));
    for &(need, mi, ri) in needs.iter() {
        let (Some(to), Some(&room_free)) = (reads.get(ri), free.get(ri)) else {
            continue;
        };
        let mut best: Option<Choice> = None;
        for (gi, g) in reads.iter().enumerate() {
            if gi == ri || busy.get(gi).copied().unwrap_or(true) {
                continue;
            }
            let surplus = left.get(gi).and_then(|l| l.get(mi)).copied().unwrap_or(0);
            if surplus < min {
                continue;
            }
            let rg = range(g.room, to.room);
            if rg > cfg.max_range {
                continue;
            }
            let key = (rg, Reverse(surplus), g.room.packed_repr());
            if best.is_some_and(|b| b.0 <= key) {
                continue;
            }
            if let Some((amount, wait)) =
                offer(need, surplus, room_free, rg, g, giving(gi).bankable, cfg)
            {
                best = Some((key, gi, amount, wait));
            }
        }
        let Some(((rg, _, _), gi, amount, wait)) = best else {
            continue;
        };
        let (Some(giver), Some(&kind)) = (reads.get(gi), BALANCED_RESOURCES.get(mi)) else {
            continue;
        };
        if let Some(b) = busy.get_mut(gi) {
            *b = true;
        }
        if let Some(l) = left.get_mut(gi).and_then(|l| l.get_mut(mi)) {
            *l = l.saturating_sub(amount);
        }
        if let Some(f) = free.get_mut(ri) {
            *f = f.saturating_sub(amount);
        }
        out.push(Send {
            from: giver.room,
            to: to.room,
            kind,
            amount,
            fee: fee(amount, rg),
            range: rg,
            wait,
        });
    }
}

/// A member's counters since the global started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalStats {
    /// Full reads.
    pub reads: u32,
    /// Energy reads (while a send waits for its fee).
    pub energy_reads: u32,
    /// Sends ordered from it that the game accepted.
    pub sends: u32,
    /// What those sends moved. Unit: units.
    pub sent: u64,
    /// Their fees. Unit: energy.
    pub fees: u64,
    /// What it received by the sends ordered to it that the game accepted. Unit: units.
    pub received: u64,
    /// Sends from it the game refused (or that threw).
    pub refused: u32,
}

/// One member: an owned room with an own terminal at a level that allows one.
#[derive(Clone, Debug)]
pub struct TerminalRoom {
    pub name: RoomName,
    /// Whether this tick read its terminal in full.
    pub read: bool,
    /// The last full read (its energy updated by any later energy read).
    pub seen: Option<TerminalRead>,
    pub seen_time: u32,
    /// The send it waits to make, from the last plan ([`Wait::Cooldown`], [`Wait::Fee`] or
    /// [`Wait::Busy`]).
    pub waiting: Option<Send>,
    /// Its terminal this tick (from the structures, not the last read): the bank's target.
    pub terminal: Option<(StructureHandle, Position)>,
    /// No send from it until this tick (after a refused one).
    pub held_until: u32,
    /// Whether a source hauler of the room carries energy home (its hauling plan wants haulers),
    /// this tick: only then can a waiting send's fee come from the haulers.
    pub hauled: bool,
    /// Whether its terminal takes the haulers' energy this tick ([`Terminals::banks`]).
    pub bank: bool,
    pub stats: TerminalStats,
    seen_tick: u32,
}

/// A send for the act step: the plan's send and the row of its giver's terminal in this tick's
/// structures (its JS object, for `send`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SendOrder {
    pub row: u32,
    pub send: Send,
}

/// Terminal balancing's state for one global.
#[derive(Default)]
pub struct Terminals {
    time: u32,
    rooms: Vec<TerminalRoom>,
    /// The tick the next plan is due (the members are read in full on it).
    next_plan: u32,
    /// Whether this tick read every member in full, so the plan can run.
    planning: bool,
    /// Plans run since the global started.
    plans: u32,
    /// The last plan's sends, every wait included.
    planned: Vec<Send>,
    /// This tick's sends for the act step.
    orders: Vec<SendOrder>,
    reads: Vec<RoomName>,
    energy_reads: Vec<RoomName>,
    banks: Vec<(RoomName, StructureHandle, Position, u32)>,
    scratch: PlanScratch,
    view: Vec<TerminalRead>,
}

impl Terminals {
    /// Starts the tick (module docs, step 1): the members (own terminals at a level that allows
    /// one), and this tick's full and energy reads of each.
    pub fn begin(&mut self, snap: &Snapshot, cfg: &TerminalsConfig) {
        self.time = snap.tick.time;
        self.orders.clear();
        self.banks.clear();
        let now = self.time;
        let rooms = &snap.rooms;
        let allows = |name: RoomName| {
            rooms
                .row_of(name)
                .and_then(|r| rooms.controllers().get(r))
                .is_some_and(|c| match *c {
                    Controller::Mine { level } => {
                        StructureType::Terminal.controller_structures(u32::from(level)) > 0
                    }
                    _ => false,
                })
        };
        let st = &snap.structures;
        for ((&kind, &pos), &h) in st.kinds().iter().zip(st.positions()).zip(st.handles()) {
            if kind != StructureType::Terminal {
                continue;
            }
            let name = pos.room_name();
            if allows(name) {
                let i = self.track(name, now);
                if let Some(r) = self.rooms.get_mut(i) {
                    r.terminal = Some((h, pos));
                }
            }
        }
        self.rooms.retain(|r| r.seen_tick == now);
        // Off, or alone, nothing waits: no bank stays open on a wait no plan can end (the
        // hot-path review, 2026-11-26).
        let active = cfg.enabled && self.rooms.len() >= 2;
        for room in &mut self.rooms {
            room.read = false;
            room.bank = false;
            if !active {
                room.waiting = None;
            }
            if let Some(read) = snap.terminals.get(room.name) {
                room.read = true;
                room.seen = Some(*read);
                room.seen_time = now;
                room.stats.reads = room.stats.reads.saturating_add(1);
            } else if let Some(energy) = snap.terminals.energy(room.name) {
                room.stats.energy_reads = room.stats.energy_reads.saturating_add(1);
                if let Some(s) = room.seen.as_mut() {
                    s.energy = energy;
                }
            }
        }
        self.planning = active && self.rooms.iter().all(|r| r.read);
    }

    /// The index of `room`, added when missing; marks it a member this tick.
    fn track(&mut self, room: RoomName, now: u32) -> usize {
        let key = room.packed_repr();
        match self
            .rooms
            .binary_search_by_key(&key, |r| r.name.packed_repr())
        {
            Ok(i) => {
                if let Some(r) = self.rooms.get_mut(i) {
                    r.seen_tick = now;
                }
                i
            }
            Err(i) => {
                self.rooms.insert(
                    i,
                    TerminalRoom {
                        name: room,
                        read: false,
                        seen: None,
                        seen_time: 0,
                        waiting: None,
                        terminal: None,
                        held_until: 0,
                        hauled: false,
                        bank: false,
                        stats: TerminalStats::default(),
                        seen_tick: now,
                    },
                );
                i
            }
        }
    }

    /// Module docs, step 2: records which members' sources are hauled this tick (`hauled`, from
    /// the hauling plans), and, while `standard` (the Standard tier runs this tick, so the waiting
    /// terminal's energy is read), opens the terminal of each one whose send waits for its fee to
    /// the source haulers, up to `terminals.energy_keep` and within its free room
    /// ([`Self::banks`]).
    pub fn set_hauled(
        &mut self,
        hauled: impl Fn(RoomName) -> bool,
        standard: bool,
        cfg: &TerminalsConfig,
    ) {
        self.banks.clear();
        let open = cfg.enabled && standard && self.rooms.len() >= 2;
        for r in &mut self.rooms {
            r.hauled = hauled(r.name);
            let waits = r.waiting.is_some_and(|w| w.wait == Wait::Fee);
            if !(open && waits && r.hauled) {
                continue;
            }
            let (Some(s), Some((handle, at))) = (r.seen, r.terminal) else {
                continue;
            };
            let want = cfg.energy_keep.saturating_sub(s.energy).min(s.free);
            if want > 0 {
                r.bank = true;
                self.banks.push((r.name, handle, at, want));
            }
        }
    }

    /// The terminals the source haulers deliver energy to this tick (module docs): room,
    /// terminal, its place, and how much more energy it takes. `economy::Hauling::set_banks`
    /// takes them with the factory's.
    pub fn banks(&self) -> &[(RoomName, StructureHandle, Position, u32)] {
        &self.banks
    }

    /// The Standard `terminals` job (module docs, step 3): on a tick every member was read, the
    /// plan, whose ready sends become [`Self::orders`] (each committing its intent's CPU on
    /// `commit`) and whose others wait on their givers; on another tick, a waiting giver whose
    /// energy read shows its fee asks for the plan on the next tick. A ready send whose giver's
    /// terminal a creep withdraws from this tick (`buffer`, filled by the jobs before this one)
    /// waits for the next tick instead ([`Wait::Busy`]): the engine makes the creep's withdrawal
    /// and the send in its room pass in an order the bot does not know, and a send left short
    /// there is dropped after the game side answered OK (V436, the hot-path review).
    pub fn run(
        &mut self,
        cfg: &TerminalsConfig,
        buffer: &IntentBuffer,
        commit: &mut Commit,
    ) -> Result<(), Fault> {
        let now = self.time;
        if !self.planning {
            for r in &self.rooms {
                if let (Some(w), Some(s)) = (r.waiting, r.seen)
                    && w.wait == Wait::Fee
                    && w.fee <= spendable(s.energy)
                {
                    self.next_plan = self.next_plan.min(now.saturating_add(1));
                }
            }
            return Ok(());
        }
        self.view.clear();
        for r in &self.rooms {
            let Some(s) = r.seen else {
                return Err(Fault::new("a member with no read", 0));
            };
            self.view.push(s);
        }
        let rooms = &self.rooms;
        let giving = |i: usize| {
            rooms.get(i).map_or(
                Giving {
                    may_give: false,
                    bankable: false,
                },
                |r| Giving {
                    may_give: r.held_until <= now,
                    bankable: r.hauled,
                },
            )
        };
        plan(
            &self.view,
            giving,
            cfg,
            &mut self.scratch,
            &mut self.planned,
        );
        self.plans = self.plans.saturating_add(1);
        for r in &mut self.rooms {
            r.waiting = None;
        }
        let mut next = now.saturating_add(cfg.read_ticks.max(1));
        for &send in &self.planned {
            let Some(giver) = self.rooms.iter_mut().find(|r| r.name == send.from) else {
                continue;
            };
            match send.wait {
                Wait::Ready => {
                    let Some(s) = giver.seen else {
                        continue;
                    };
                    if buffer.withdraws_from(s.handle) {
                        giver.waiting = Some(Send {
                            wait: Wait::Busy,
                            ..send
                        });
                        next = next.min(now.saturating_add(1));
                        continue;
                    }
                    self.orders.push(SendOrder { row: s.row, send });
                    commit.intents(1);
                    next = next.min(now.saturating_add(TERMINAL_COOLDOWN.max(1)));
                }
                Wait::Cooldown => {
                    let ends = giver.seen.map_or(0, |s| s.cooldown);
                    let ends = now
                        .saturating_add(ends)
                        .max(giver.held_until)
                        .max(now.saturating_add(1));
                    next = next.min(ends);
                    giver.waiting = Some(send);
                }
                Wait::Fee | Wait::Busy => {
                    giver.waiting = Some(send);
                }
            }
        }
        self.next_plan = next;
        // A terminal whose send no longer waits for its fee is not read for its energy next tick
        // (its bank was given to the haulers before this plan).
        for r in &mut self.rooms {
            r.bank &= r.waiting.is_some_and(|w| w.wait == Wait::Fee);
        }
        Ok(())
    }

    /// This tick's sends for the act step, in plan order.
    pub fn orders(&self) -> &[SendOrder] {
        &self.orders
    }

    /// Records the act step's result for order `index` ([`Self::orders`]): an accepted send is
    /// counted on both rooms; one that was not OK holds its giver for `terminals.read_ticks`
    /// (module docs). Returns whether this call started the hold.
    pub fn record_send(&mut self, index: usize, outcome: Outcome, cfg: &TerminalsConfig) -> bool {
        let Some(order) = self.orders.get(index).copied() else {
            return false;
        };
        let send = order.send;
        if outcome == Outcome::Ok {
            if let Some(g) = self.rooms.iter_mut().find(|r| r.name == send.from) {
                g.stats.sends = g.stats.sends.saturating_add(1);
                g.stats.sent = g.stats.sent.saturating_add(u64::from(send.amount));
                g.stats.fees = g.stats.fees.saturating_add(u64::from(send.fee));
            }
            if let Some(r) = self.rooms.iter_mut().find(|r| r.name == send.to) {
                r.stats.received = r.stats.received.saturating_add(u64::from(send.amount));
            }
            return false;
        }
        let until = self.time.saturating_add(cfg.read_ticks.max(1));
        match self.rooms.iter_mut().find(|r| r.name == send.from) {
            Some(r) => {
                r.stats.refused = r.stats.refused.saturating_add(1);
                let started = r.held_until <= self.time;
                r.held_until = until;
                started
            }
            None => false,
        }
    }

    /// The terminals the next tick's snapshot reads (module docs), each sorted by packed name: in
    /// full, every member while `terminals.enabled` is on, there are two or more, the Standard
    /// tier ran this tick (`standard`), and the plan is due on the next tick; else, for their
    /// energy alone, the members whose terminal takes the haulers' energy for a waiting fee.
    pub fn reads(&mut self, cfg: &TerminalsConfig, standard: bool) -> (&[RoomName], &[RoomName]) {
        let next = self.time.saturating_add(1);
        self.reads.clear();
        self.energy_reads.clear();
        if cfg.enabled && standard && self.rooms.len() >= 2 {
            if next >= self.next_plan {
                self.reads.extend(self.rooms.iter().map(|r| r.name));
            } else {
                self.energy_reads
                    .extend(self.rooms.iter().filter(|r| r.bank).map(|r| r.name));
            }
        }
        (&self.reads, &self.energy_reads)
    }

    /// The members, by name.
    pub fn rooms(&self) -> &[TerminalRoom] {
        &self.rooms
    }

    /// The last plan's sends, every wait included.
    pub fn planned(&self) -> &[Send] {
        &self.planned
    }

    /// The tick the next plan is due.
    pub fn next_plan(&self) -> u32 {
        self.next_plan
    }

    /// Plans run since the global started.
    pub fn plans(&self) -> u32 {
        self.plans
    }

    /// Whether this tick read every member in full, so [`Self::run`] plans.
    pub fn planning(&self) -> bool {
        self.planning
    }
}
