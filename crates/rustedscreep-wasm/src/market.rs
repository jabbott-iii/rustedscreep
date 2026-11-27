//! P9.5 market boundary. Optional jobs sample terminal and market data and prepare at most one
//! deal. The act step issues it, then the game may fill all or part of the request (V449).

use std::cmp::Reverse;

use js_sys::{Array, JsString, Object, Reflect};
use rustedscreep_core::{
    config::{Config, MarketConfig, SchedulerConfig},
    intents::{ActLog, IntentBuffer, Outcome, error_name},
    market::{Deal, Quote, Side, Slot, Terminal, credit_balance, lot, milli, offer, spendable},
    scheduler::{Clock, Fault, FaultRecord, Job, JobState, Scheduler, TickCpu, Tier},
    snapshot::{Controller, Snapshot, balanced_resource, parse_room_name},
};
use screeps::{OrderType, ResourceType, RoomName, StructureType};
use wasm_bindgen::{JsCast, JsValue, prelude::wasm_bindgen};

use crate::snapshot::JsWorld;

#[wasm_bindgen]
extern "C" {
    type MarketStore;

    #[wasm_bindgen(method, catch, js_name = getUsedCapacity)]
    fn used(this: &MarketStore, resource: ResourceType) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(method, catch, js_name = getFreeCapacity)]
    fn free(this: &MarketStore) -> Result<JsValue, JsValue>;

    type MarketApi;

    #[wasm_bindgen(method, catch, js_name = getAllOrders)]
    fn get_all_orders(this: &MarketApi, filter: &Object) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(method, catch, js_name = calcTransactionCost)]
    fn transaction_cost(
        this: &MarketApi,
        amount: u32,
        from: &JsString,
        to: &JsString,
    ) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(method, catch)]
    fn deal(
        this: &MarketApi,
        id: &JsString,
        amount: u32,
        local: &JsString,
    ) -> Result<JsValue, JsValue>;
}

const MARKET: Job = Job {
    name: "market",
    tier: Tier::Optional,
};
const QUOTES: Job = Job {
    name: "market quotes",
    tier: Tier::Optional,
};

struct Book {
    market: MarketApi,
    orders: Array,
    resource: JsValue,
    side: JsValue,
    terminal: Terminal,
    limit: u32,
    budget: u64,
}

struct Candidate {
    id: JsString,
    remote: RoomName,
    deal: Deal,
}

struct Order {
    market: MarketApi,
    id: JsString,
    room: RoomName,
    slot: Slot,
    deal: Deal,
}

#[derive(Default)]
pub struct Market {
    probe: JobState,
    quotes: JobState,
    queued: Option<Order>,
    last_slot: Option<u32>,
    scanned: u32,
    accepted: u32,
    refused: u32,
}

impl Market {
    /// Runs after terminal balancing and all creep resource jobs to leave their intents priority.
    pub fn work<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        snap: &Snapshot,
        js: &JsWorld,
        buffer: &IntentBuffer,
        terminal_send: bool,
        cfg: &Config,
    ) {
        self.queued = None;
        let now = snap.tick.time;
        let Some(slot) = Slot::at(now, &cfg.market) else {
            return;
        };
        if terminal_send || !tick.allows(Tier::Optional) || self.last_slot == Some(now) {
            return;
        }

        let mut book = None;
        tick.run_units(&MARKET, &mut self.probe, 1, |_, _| {
            self.last_slot = Some(now);
            book = read_book(slot, snap, js, buffer, cfg)?;
            Ok(())
        });
        let Some(book) = book else { return };
        self.scanned = self.scanned.saturating_add(1);
        let len = book.orders.length();
        let count = cfg.market.scan_orders.min(len);
        if count == 0 {
            return;
        }
        let start = slot.offset(len, count);
        let mut best: Option<Candidate> = None;
        tick.run_units(&QUOTES, &mut self.quotes, count, |i, _| {
            let index = ((u64::from(start) + u64::from(i)) % u64::from(len)) as u32;
            if let Some(candidate) = read_candidate(&book, slot, index, snap, &cfg.market)
                && best
                    .as_ref()
                    .is_none_or(|b| candidate.deal.preferred_to(b.deal, slot.side))
            {
                best = Some(candidate);
            }
            Ok(())
        });
        let Some(candidate) = best else { return };
        let mut selected = Some((book, candidate));
        tick.run_units(&MARKET, &mut self.probe, 1, |_, commit| {
            if let Some((book, candidate)) = selected.take() {
                self.queued = check_fee(book, slot, candidate, &cfg.market)?;
            }
            if self.queued.is_some() {
                commit.intents(1);
            }
            Ok(())
        });
    }

    pub fn issue(
        &mut self,
        now: u32,
        log: &mut ActLog,
        sched: &mut Scheduler,
        cfg: &SchedulerConfig,
    ) {
        let Some(order) = self.queued.take() else {
            return;
        };
        let outcome = match order
            .market
            .deal(&order.id, order.deal.amount, &order.room.into())
        {
            Ok(result) => Outcome::from_return(result.as_f64().unwrap_or(f64::NAN)),
            Err(_) => {
                sched.faults_mut().record(
                    FaultRecord {
                        time: now,
                        job: MARKET.name,
                        fault: Fault::new("market deal threw", 0),
                    },
                    cfg,
                );
                Outcome::Threw
            }
        };
        log.record_structure(outcome);
        let status = match outcome {
            Outcome::Ok => {
                self.accepted = self.accepted.saturating_add(1);
                "queued"
            }
            other => {
                self.refused = self.refused.saturating_add(1);
                match other {
                    Outcome::Failed(code) => error_name(code),
                    Outcome::Unknown => "unknown return",
                    _ => "threw",
                }
            }
        };
        log::info!(
            "rustedscreep market: tick {now} {:?} {} {} in {} at {} milli-credits/unit, fee {} ({status})",
            order.slot.side,
            order.deal.amount,
            order.slot.kind,
            order.room,
            order.deal.price_milli,
            order.deal.fee
        );
    }

    pub fn summary(&self, cfg: &MarketConfig) -> String {
        format!(
            "rustedscreep market: {} | scans {}, queued {}, refused {} since boot",
            if cfg.enabled { "on" } else { "off" },
            self.scanned,
            self.accepted,
            self.refused,
        )
    }
}

fn read_book(
    slot: Slot,
    snap: &Snapshot,
    js: &JsWorld,
    buffer: &IntentBuffer,
    cfg: &Config,
) -> Result<Option<Book>, Fault> {
    let Some((terminal, limit)) = find_terminal(slot, snap, js, buffer, cfg)? else {
        return Ok(None);
    };
    let game = get(&js_sys::global(), "Game")?;
    let api = get(&game, "market")?;
    if !api.is_object() {
        return Err(Fault::new("market is unavailable", 0));
    }
    let market = api.unchecked_into::<MarketApi>();
    let own = get(market.as_ref(), "orders")?;
    if !own.is_object() {
        return Err(Fault::new("missing own market orders", 0));
    }
    // Standing orders can change stock and credit balances when other players initiate a deal.
    if Reflect::own_keys(&own)
        .map_err(|_| Fault::new("cannot read own market orders", 0))?
        .length()
        > 0
    {
        return Ok(None);
    }
    let budget = if slot.side == Side::Buy {
        let credits = get(market.as_ref(), "credits")?
            .as_f64()
            .and_then(credit_balance)
            .ok_or_else(|| Fault::new("invalid market credit balance", 0))?;
        let budget = spendable(credits, &cfg.market);
        if budget == 0 {
            return Ok(None);
        }
        budget
    } else {
        0
    };
    let filter = Object::new();
    let resource = JsValue::from_str(&slot.kind.to_string());
    let side = JsValue::from(match slot.side {
        Side::Buy => OrderType::Sell,
        Side::Sell => OrderType::Buy,
    });
    set_filter(&filter, "resourceType", &resource)?;
    set_filter(&filter, "type", &side)?;
    let orders = market
        .get_all_orders(&filter)
        .ok()
        .and_then(|v| v.dyn_into::<Array>().ok())
        .ok_or_else(|| Fault::new("getAllOrders failed or returned a non-array", 0))?;
    Ok(Some(Book {
        market,
        orders,
        resource,
        side,
        terminal,
        limit,
        budget,
    }))
}

fn find_terminal(
    slot: Slot,
    snap: &Snapshot,
    js: &JsWorld,
    buffer: &IntentBuffer,
    cfg: &Config,
) -> Result<Option<(Terminal, u32)>, Fault> {
    let st = &snap.structures;
    let keep = cfg.factory.mineral_keep.max(cfg.terminals.mineral_keep);
    let Some(index) = balanced_resource(slot.kind) else {
        return Ok(None);
    };
    let buying = slot.side == Side::Buy;
    let mut fault = None;
    let terminal = st
        .kinds()
        .iter()
        .enumerate()
        .filter_map(|(row, &kind)| {
            if kind != StructureType::Terminal {
                return None;
            }
            let room = st.positions().get(row)?.room_name();
            let Controller::Mine { level } =
                *snap.rooms.controllers().get(snap.rooms.row_of(room)?)?
            else {
                return None;
            };
            if StructureType::Terminal.controller_structures(u32::from(level)) == 0 {
                return None;
            }
            let t = if let Some(t) = snap.terminals.get(room) {
                if t.cooldown > 0 {
                    return None;
                }
                Terminal {
                    room,
                    stock: *t.stock.get(index)?,
                    free: t.free,
                    energy: t.energy,
                }
            } else {
                match read_terminal(js.structures.get(row)?.as_ref(), room, slot, &cfg.market) {
                    Ok(t) => t?,
                    Err(e) => {
                        fault = Some(e);
                        return None;
                    }
                }
            };
            let limit = lot(slot.side, t, keep, &cfg.market);
            if t.energy <= cfg.market.energy_keep
                || limit < cfg.market.min_amount
                || buffer.changes_market_stock(*st.handles().get(row)?, buying)
            {
                return None;
            }
            Some((t, limit))
        })
        .max_by_key(|(t, limit)| (*limit, t.energy, Reverse(t.room.packed_repr())));
    match (terminal, fault) {
        (None, Some(e)) => Err(e),
        _ => Ok(terminal),
    }
}

fn read_terminal(
    terminal: &JsValue,
    room: RoomName,
    slot: Slot,
    cfg: &MarketConfig,
) -> Result<Option<Terminal>, Fault> {
    let invalid = || Fault::new("invalid market terminal read", 0);
    if units(get(terminal, "cooldown")?).ok_or_else(invalid)? > 0 {
        return Ok(None);
    }
    let store = get(terminal, "store")?;
    if !store.is_object() {
        return Err(invalid());
    }
    let store = store.unchecked_into::<MarketStore>();
    let used = |resource| {
        store
            .used(resource)
            .ok()
            .and_then(units)
            .ok_or_else(invalid)
    };
    let energy = used(ResourceType::Energy)?;
    if energy <= cfg.energy_keep {
        return Ok(None);
    }
    let stock = used(slot.kind)?;
    let free = if slot.side == Side::Buy {
        store.free().ok().and_then(units).ok_or_else(invalid)?
    } else {
        0
    };
    Ok(Some(Terminal {
        room,
        stock,
        free,
        energy,
    }))
}

fn read_candidate(
    book: &Book,
    slot: Slot,
    index: u32,
    snap: &Snapshot,
    cfg: &MarketConfig,
) -> Option<Candidate> {
    let raw = Reflect::get_u32(&book.orders, index).ok()?;
    if get(&raw, "resourceType").ok()? != book.resource || get(&raw, "type").ok()? != book.side {
        return None;
    }
    let price = get(&raw, "price").ok()?.as_f64().and_then(milli)?;
    let price_milli = u32::try_from(price).ok()?;
    let amount = units(get(&raw, "amount").ok()?)?.min(units(get(&raw, "remainingAmount").ok()?)?);
    let name = get(&raw, "roomName").ok()?.as_string()?;
    let room = parse_room_name(name.as_bytes())?;
    if snap
        .rooms
        .row_of(room)
        .and_then(|row| snap.rooms.controllers().get(row))
        .is_some_and(|c| matches!(c, Controller::Mine { .. }))
    {
        return None;
    }
    let deal = offer(
        slot.side,
        book.terminal,
        book.limit,
        Quote {
            room,
            price_milli,
            amount,
        },
        book.budget,
        cfg,
    )?;
    let id = get(&raw, "id").ok()?.dyn_into::<JsString>().ok()?;
    (id.length() > 0).then_some(Candidate {
        id,
        remote: room,
        deal,
    })
}

fn check_fee(
    book: Book,
    slot: Slot,
    candidate: Candidate,
    cfg: &MarketConfig,
) -> Result<Option<Order>, Fault> {
    let room = book.terminal.room;
    let Candidate {
        id,
        remote,
        mut deal,
    } = candidate;
    let fee = book
        .market
        .transaction_cost(deal.amount, &room.into(), &remote.into())
        .ok()
        .and_then(units)
        .ok_or_else(|| Fault::new("invalid market transaction cost", 0))?;
    if fee > deal.amount || fee > book.terminal.energy.saturating_sub(cfg.energy_keep) {
        return Ok(None);
    }
    deal.fee = fee;
    Ok(Some(Order {
        market: book.market,
        id,
        room,
        slot,
        deal,
    }))
}

fn units(v: JsValue) -> Option<u32> {
    let v = v.as_f64()?;
    (v.fract() == 0.0 && (0.0..=f64::from(u32::MAX)).contains(&v)).then_some(v as u32)
}

fn get(obj: &JsValue, key: &str) -> Result<JsValue, Fault> {
    Reflect::get(obj, &JsValue::from_str(key))
        .map_err(|_| Fault::new("market property read failed", 0))
}

fn set_filter(obj: &Object, key: &str, value: &JsValue) -> Result<(), Fault> {
    match Reflect::set(obj, &JsValue::from_str(key), value) {
        Ok(true) => Ok(()),
        _ => Err(Fault::new("market filter could not be set", 0)),
    }
}
