//! P9.5 market policy for the seven base minerals. An Optional tick may buy to fill a terminal
//! or sell surplus from it. Quotes and terminal stock are sampled again for each slot.

use rustedscreep_consts::{ResourceType, RoomName, TERMINAL_COOLDOWN, extra::MARKET_CREDIT_SCALE};

use crate::{
    config::MarketConfig,
    economy::{terminal_affordable, terminal_fee, terminal_range},
    snapshot::BALANCED_RESOURCES,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub kind: ResourceType,
    pub side: Side,
    /// Number of times this mineral and side came up before this slot.
    pub round: u32,
}

impl Slot {
    /// A fixed tick schedule survives global resets. Each mineral gets a buy slot and a sell
    /// slot. A disabled side leaves its slot idle. Uses: `TERMINAL_COOLDOWN`.
    pub fn at(time: u32, cfg: &MarketConfig) -> Option<Self> {
        let interval = cfg.scan_ticks.max(TERMINAL_COOLDOWN).max(1);
        if !cfg.enabled || !time.is_multiple_of(interval) || cfg.min_amount > cfg.max_amount {
            return None;
        }
        let slot = time / interval;
        let side = if slot.is_multiple_of(2) {
            Side::Buy
        } else {
            Side::Sell
        };
        if match side {
            Side::Buy => cfg.buy_target == 0 || cfg.buy_max_milli == 0,
            Side::Sell => cfg.sell_min_milli == 0,
        } {
            return None;
        }
        let mineral = slot as usize / 2;
        Some(Self {
            kind: *BALANCED_RESOURCES.get(mineral % BALANCED_RESOURCES.len())?,
            side,
            round: (mineral / BALANCED_RESOURCES.len()) as u32,
        })
    }

    /// Start of the rotating window in a freshly fetched market book.
    pub fn offset(self, len: u32, limit: u32) -> u32 {
        (u64::from(self.round) * u64::from(limit))
            .checked_rem(u64::from(len))
            .unwrap_or(0) as u32
    }
}

/// Translate verified engine money values without rounding a quote to a more favorable price.
/// Values outside the safe JavaScript integer range or off the milli-credit grid fail closed.
/// Uses: `extra::MARKET_CREDIT_SCALE` (V447).
pub fn milli(credits: f64) -> Option<u64> {
    let scale = f64::from(MARKET_CREDIT_SCALE);
    let scaled = (credits * scale).round();
    let max_safe_integer = ((1_u64 << f64::MANTISSA_DIGITS) - 1) as f64;
    (scaled >= 0.0 && scaled <= max_safe_integer && scaled / scale == credits)
        .then_some(scaled as u64)
}

/// Round a fractional balance down before applying the credit floor. Order expiry can leave
/// fractional engine money units (V453). Uses: `extra::MARKET_CREDIT_SCALE`.
pub fn credit_balance(credits: f64) -> Option<u64> {
    if let Some(exact) = milli(credits) {
        return Some(exact);
    }
    let scale = f64::from(MARKET_CREDIT_SCALE);
    let scaled = (credits * scale).floor();
    let max_safe_integer = ((1_u64 << f64::MANTISSA_DIGITS) - 1) as f64;
    if !(0.0..=max_safe_integer).contains(&scaled) {
        return None;
    }
    Some((scaled as u64).saturating_sub(u64::from(scaled / scale > credits)))
}

pub fn spendable(credits_milli: u64, cfg: &MarketConfig) -> u64 {
    credits_milli.saturating_sub(u64::from(cfg.credit_floor) * u64::from(MARKET_CREDIT_SCALE))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Terminal {
    pub room: RoomName,
    pub stock: u32,
    pub free: u32,
    pub energy: u32,
}

/// Local bounds after protecting the terminal's mineral stocks and initial free capacity.
/// The engine clips an incoming deal against free capacity before deducting its fee (V449).
pub fn lot(side: Side, t: Terminal, other_mineral_keep: u32, cfg: &MarketConfig) -> u32 {
    match side {
        Side::Buy => cfg.buy_target.saturating_sub(t.stock).min(t.free),
        Side::Sell => t
            .stock
            .saturating_sub(cfg.sell_keep.max(cfg.buy_target).max(other_mineral_keep)),
    }
    .min(cfg.max_amount)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quote {
    pub room: RoomName,
    pub price_milli: u32,
    /// The smaller of the available and remaining volumes (V451).
    pub amount: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Deal {
    pub amount: u32,
    pub price_milli: u32,
    /// Conservative fee before the live wrapped-cost check.
    pub fee: u32,
}

impl Deal {
    /// Best credit price first, then lower energy per unit and larger volume.
    pub fn preferred_to(self, other: Self, side: Side) -> bool {
        let price = match side {
            Side::Buy => other.price_milli.cmp(&self.price_milli),
            Side::Sell => self.price_milli.cmp(&other.price_milli),
        };
        let left = u64::from(self.fee) * u64::from(other.amount);
        let right = u64::from(other.fee) * u64::from(self.amount);
        price
            .then_with(|| right.cmp(&left))
            .then_with(|| self.amount.cmp(&other.amount))
            .is_gt()
    }
}

/// Clip a deal against local stock or space, both order volume fields, the volume cap, energy,
/// price and the credit floor. Quoted prices use integer engine money units (V447–V453). The unwrapped fee
/// is conservative away from rounding boundaries. The boundary checks the selected fee again.
/// Uses: `TERMINAL_SEND_COST_SCALE` through `terminal_fee` and `terminal_affordable`.
pub fn offer(
    side: Side,
    t: Terminal,
    local_limit: u32,
    q: Quote,
    spendable_milli: u64,
    cfg: &MarketConfig,
) -> Option<Deal> {
    if q.price_milli == 0 || t.room == q.room {
        return None;
    }
    let mut amount = local_limit.min(q.amount).min(cfg.max_amount);
    match side {
        Side::Buy if q.price_milli <= cfg.buy_max_milli => {
            let funded = spendable_milli / u64::from(q.price_milli);
            amount = amount.min(u32::try_from(funded).unwrap_or(u32::MAX));
        }
        Side::Sell if cfg.sell_min_milli > 0 && q.price_milli >= cfg.sell_min_milli => {}
        _ => return None,
    }
    let range = terminal_range(t.room, q.room);
    amount = amount.min(terminal_affordable(
        t.energy.saturating_sub(cfg.energy_keep),
        range,
    ));
    (amount >= cfg.min_amount.max(1)).then(|| Deal {
        amount,
        price_milli: q.price_milli,
        fee: terminal_fee(amount, range),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MARKET_MMO;

    fn terminal() -> Terminal {
        Terminal {
            room: "E1N1".parse().expect("home"),
            stock: 0,
            free: MARKET_MMO.max_amount,
            energy: MARKET_MMO.energy_keep + MARKET_MMO.max_amount,
        }
    }

    fn quote(price_milli: u32, amount: u32) -> Quote {
        Quote {
            room: "E2N1".parse().expect("other room"),
            price_milli,
            amount,
        }
    }

    #[test]
    fn slot_schedule_is_reset_independent_and_rotates_both_sides() {
        let cfg = MarketConfig {
            enabled: true,
            ..MARKET_MMO
        };
        for (i, &kind) in BALANCED_RESOURCES.iter().enumerate() {
            let start = 2 * i as u32 * cfg.scan_ticks;
            let buy = Slot::at(start, &cfg).expect("buy");
            let sell = Slot::at(start + cfg.scan_ticks, &cfg).expect("sell");
            assert_eq!((buy.kind, buy.side, buy.round), (kind, Side::Buy, 0));
            assert_eq!((sell.kind, sell.side, sell.round), (kind, Side::Sell, 0));
            assert_eq!(Slot::at(start + 1, &cfg), None);
            assert_eq!(Slot::at(start, &MARKET_MMO), None);
        }
        let next = 2 * BALANCED_RESOURCES.len() as u32 * cfg.scan_ticks;
        assert_eq!(Slot::at(next, &cfg).expect("next round").offset(97, 64), 64);
    }

    #[test]
    fn money_accepts_integer_milli_credits_and_rejects_corrupt_quotes() {
        for n in [0_u32, 1, 1_003, 4_001_111] {
            assert_eq!(
                milli(f64::from(n) / f64::from(MARKET_CREDIT_SCALE)),
                Some(u64::from(n))
            );
        }
        assert_eq!(credit_balance(1.2349), Some(1_234));
        assert_eq!(credit_balance(f64::INFINITY), None);
        for malformed in [f64::NAN, f64::INFINITY, -0.001, 0.0001, f64::MAX] {
            assert_eq!(milli(malformed), None);
        }
    }

    #[test]
    fn buy_and_sell_limits_are_inclusive() {
        let c = &MARKET_MMO;
        for (side, good, bad) in [
            (Side::Buy, c.buy_max_milli, c.buy_max_milli + 1),
            (Side::Sell, c.sell_min_milli, c.sell_min_milli - 1),
        ] {
            assert!(
                offer(
                    side,
                    terminal(),
                    c.max_amount,
                    quote(good, c.max_amount),
                    u64::MAX,
                    c
                )
                .is_some()
            );
            assert!(
                offer(
                    side,
                    terminal(),
                    c.max_amount,
                    quote(bad, c.max_amount),
                    u64::MAX,
                    c
                )
                .is_none()
            );
        }
    }

    #[test]
    fn equal_prices_favor_lower_fee_per_unit_then_larger_lots() {
        let cfg = &MARKET_MMO;
        let t = terminal();
        let mut q = quote(cfg.buy_max_milli, cfg.max_amount);
        let buy = |q| offer(Side::Buy, t, cfg.max_amount, q, u64::MAX, cfg).expect("viable offer");
        let large = buy(q);
        q.amount = cfg.min_amount;
        let small = buy(q);
        assert!(large.preferred_to(small, Side::Buy));
    }

    #[test]
    fn floor_and_volume_hold_at_the_milli_credit_boundary() {
        let c = &MARKET_MMO;
        let t = terminal();
        let price = c.buy_max_milli - 1;
        let floor = u64::from(c.credit_floor) * u64::from(MARKET_CREDIT_SCALE);
        let minimum_cost = u64::from(c.min_amount) * u64::from(price);
        let buy = |credit, offered| {
            offer(
                Side::Buy,
                t,
                lot(Side::Buy, t, 0, c),
                quote(price, offered),
                spendable(credit, c),
                c,
            )
        };
        assert!(buy(floor - 1, c.max_amount).is_none());
        assert!(buy(floor + minimum_cost - 1, c.max_amount).is_none());
        assert_eq!(
            buy(floor + minimum_cost, c.max_amount)
                .expect("exact floor")
                .amount,
            c.min_amount
        );
        assert_eq!(
            buy(u64::MAX, c.max_amount + 1).expect("cap").amount,
            c.max_amount
        );
        assert_eq!(
            buy(u64::MAX, c.min_amount).expect("quoted volume").amount,
            c.min_amount
        );
    }

    #[test]
    fn stock_capacity_and_energy_bound_deal_volume() {
        let c = &MARKET_MMO;
        let mut t = terminal();
        t.free = c.min_amount;
        assert_eq!(lot(Side::Buy, t, 0, c), c.min_amount);
        t.stock = c.sell_keep + c.max_amount;
        assert_eq!(lot(Side::Sell, t, t.stock - c.min_amount, c), c.min_amount);
        let overlapping = MarketConfig {
            buy_target: t.stock,
            ..*c
        };
        assert_eq!(lot(Side::Sell, t, 0, &overlapping), 0);
        let q = quote(c.sell_min_milli, c.max_amount);
        let rg = terminal_range(t.room, q.room);
        let fee = terminal_fee(c.min_amount, rg);
        t.energy = c.energy_keep + fee - 1;
        let sell = |t| offer(Side::Sell, t, c.max_amount, q, 0, c);
        assert!(sell(t).is_none());
        t.energy += 1;
        let deal = sell(t).expect("fee funded");
        assert!(deal.amount >= c.min_amount && deal.amount <= c.max_amount);
        assert_eq!(deal.fee, fee);
        assert!(
            offer(
                Side::Sell,
                t,
                c.max_amount,
                Quote { room: t.room, ..q },
                0,
                c
            )
            .is_none()
        );
    }
}
