//! Generate the `examples/one_month` verification dataset.
//!
//! Fully deterministic (no RNG): every value is a round number on a slot
//! boundary so results can be cross-checked by hand and by the independent
//! referee (`tools/referee.py`). 30 days x 96 slots of 15 min.
//!
//! Vehicle archetypes (persistence-chained daily unless noted):
//!   0 commuter        daily 09:00-17:00, banks nothing under uncontrolled
//!   1 resident        overnight 18:00-08:00, session spans midnight
//!   2 surplus donor   daily 10:00-19:30, arrives far above target (V2B bank),
//!                     present through the evening DR windows
//!   3 deficit heavy   daily 08:00-20:00, large need, discharge disabled
//!   4 visitor         weekly 11:00-15:00 (days 2, 9, 16, 23)
//!   5 unserved        day 2 12:00-14:00, all four chargers busy -> never connects
//!   6 floor-bound     overnight 22:00-06:00 (spans midnight), high SoC floor
//!   7 evening light   daily 17:00-22:00, small battery, no V2B
//!
//! DR: Tue/Thu each week 17:00-19:00 (encoded per the (start, end] convention:
//! start_slot at 17:00 means the first affected slot is 17:15), plus four
//! boundary/interaction events from the R1 review:
//!   day 0  00:00-01:00  earliest representable window (covers slot 1..4)
//!   day 8  19:00-20:00  back-to-back with that day's standard event
//!   day 26 15:00-17:30  straddles the off-peak/peak TOU boundary at 16:00
//!   day 29 23:00-23:45  ends at the final slot of the horizon (2879)

use std::fmt::Write as _;
use std::path::Path;

const DAYS: usize = 30;
const SLOTS_PER_DAY: usize = 96;

fn slot(day: usize, hour: f64) -> usize {
    day * SLOTS_PER_DAY + (hour * 4.0) as usize
}

fn main() -> std::io::Result<()> {
    let dir = Path::new("examples/one_month");
    std::fs::create_dir_all(dir)?;

    // scenario.json
    std::fs::write(
        dir.join("scenario.json"),
        r#"{
  "slot_minutes": 15.0,
  "horizon_slots": 2880,
  "charge_efficiency": 1.0,
  "discharge_efficiency": 1.0,
  "demand_charge_usd_per_kw": 0.0,
  "demand_charge_peak_usd_per_kw": 11.67,
  "dr_events_file": "dr_events.csv",
  "persistence": true
}
"#,
    )?;

    // chargers.csv: 4 ports, 2 bidirectional.
    std::fs::write(
        dir.join("chargers.csv"),
        "charger_id,max_kw,bidirectional\n0,20.0,true\n1,20.0,true\n2,20.0,false\n3,20.0,false\n",
    )?;

    // building_load.csv: daily double hump, piecewise constant.
    // 00-08h: 20 kW, 08-12h: 45, 12-16h: 35, 16-21h: 60, 21-24h: 25.
    let mut building = String::from("slot,value\n");
    for day in 0..DAYS {
        for (hour, kw) in [
            (0.0, 20.0),
            (8.0, 45.0),
            (12.0, 35.0),
            (16.0, 60.0),
            (21.0, 25.0),
        ] {
            writeln!(building, "{},{kw}", slot(day, hour)).expect("write to string");
        }
    }
    std::fs::write(dir.join("building_load.csv"), building)?;

    // grid_prices.csv: super-off-peak 00-06h 0.08, off-peak 06-16h 0.15,
    // peak 16-21h 0.32, off-peak 21-24h 0.15.
    let mut prices = String::from("slot,value,tou\n");
    for day in 0..DAYS {
        for (hour, price, tou) in [
            (0.0, 0.08, "super-off-peak"),
            (6.0, 0.15, "off-peak"),
            (16.0, 0.32, "peak"),
            (21.0, 0.15, "off-peak"),
        ] {
            writeln!(prices, "{},{price},{tou}", slot(day, hour)).expect("write to string");
        }
    }
    std::fs::write(dir.join("grid_prices.csv"), prices)?;

    // dr_events.csv: Tue/Thu (day % 7 in {1, 3}) 17:00-19:00, FSL 45 kW,
    // baseline 62 kW. (start, end]: start at 17:00 -> covered slots are
    // 17:15..19:00 inclusive.
    let mut dr = String::from(
        "start_slot,end_slot,fsl_kw,penalty_usd_per_kwh,incentive_usd_per_kw,baseline_kw\n",
    );
    for day in 0..DAYS {
        if day % 7 == 1 || day % 7 == 3 {
            writeln!(
                dr,
                "{},{},45.0,6.0,13.6,62.0",
                slot(day, 17.0),
                slot(day, 19.0)
            )
            .expect("write to string");
        }
    }
    // Boundary/interaction events (see module docs). The day-0 event is
    // deliberately violated (FSL 18 < night load 20, nothing connected to
    // discharge), so the dataset settles both honored and violated windows.
    writeln!(dr, "{},{},18.0,6.0,13.6,25.0", slot(0, 0.0), slot(0, 1.0)).expect("write to string");
    writeln!(dr, "{},{},45.0,6.0,13.6,62.0", slot(8, 19.0), slot(8, 20.0))
        .expect("write to string");
    writeln!(
        dr,
        "{},{},55.0,6.0,13.6,62.0",
        slot(26, 15.0),
        slot(26, 17.5)
    )
    .expect("write to string");
    writeln!(
        dr,
        "{},{},30.0,6.0,13.6,35.0",
        slot(29, 23.0),
        slot(29, 23.75)
    )
    .expect("write to string");
    std::fs::write(dir.join("dr_events.csv"), dr)?;

    // vehicles.csv
    let mut v = String::from(
        "vehicle_id,arrival_slot,departure_slot,battery_kwh,soc_arrival_kwh,soc_target_kwh,\
         max_charge_kw,max_discharge_kw,min_soc_kwh,depletion_kwh\n",
    );
    let mut row = |id: u32,
                   arr: usize,
                   dep: usize,
                   batt: f64,
                   soc: f64,
                   target: f64,
                   chg: f64,
                   dis: f64,
                   floor: f64,
                   depl: f64| {
        writeln!(
            v,
            "{id},{arr},{dep},{batt},{soc},{target},{chg},{dis},{floor},{depl}"
        )
        .expect("write to string");
    };
    for day in 0..DAYS {
        // 0: commuter. First-session SoC 25; drives 12 kWh/day.
        row(
            0,
            slot(day, 9.0),
            slot(day, 17.0),
            60.0,
            25.0,
            45.0,
            11.0,
            11.0,
            6.0,
            12.0,
        );
        // 2: surplus donor, present through the evening DR windows.
        row(
            2,
            slot(day, 10.0),
            slot(day, 19.5),
            100.0,
            90.0,
            40.0,
            20.0,
            20.0,
            10.0,
            5.0,
        );
        // 3: deficit heavy, no V2B.
        row(
            3,
            slot(day, 8.0),
            slot(day, 20.0),
            60.0,
            8.0,
            55.0,
            20.0,
            0.0,
            5.0,
            30.0,
        );
        // 7: evening light, no V2B.
        row(
            7,
            slot(day, 17.0),
            slot(day, 22.0),
            30.0,
            10.0,
            25.0,
            6.0,
            0.0,
            3.0,
            10.0,
        );
        // Overnight sessions (span midnight); none may start on the last day.
        if day + 1 < DAYS {
            // 1: resident.
            row(
                1,
                slot(day, 18.0),
                slot(day + 1, 8.0),
                80.0,
                30.0,
                60.0,
                19.0,
                19.0,
                8.0,
                15.0,
            );
            // 6: floor-bound V2B.
            row(
                6,
                slot(day, 22.0),
                slot(day + 1, 6.0),
                50.0,
                20.0,
                30.0,
                10.0,
                10.0,
                15.0,
                8.0,
            );
        }
    }
    // 4: weekly visitor.
    for day in [2, 9, 16, 23] {
        row(
            4,
            slot(day, 11.0),
            slot(day, 15.0),
            40.0,
            15.0,
            35.0,
            7.0,
            0.0,
            4.0,
            20.0,
        );
    }
    // 5: unserved. Day 2 12:00-14:00: chargers held by 0, 2, 3, 4.
    row(
        5,
        slot(2, 12.0),
        slot(2, 14.0),
        50.0,
        20.0,
        40.0,
        20.0,
        0.0,
        5.0,
        0.0,
    );
    std::fs::write(dir.join("vehicles.csv"), v)?;

    // Lossy twin: identical CSVs, asymmetric efficiencies. No hand math here;
    // the referee checks it from first principles.
    let lossy = Path::new("examples/one_month_lossy");
    std::fs::create_dir_all(lossy)?;
    for f in [
        "chargers.csv",
        "building_load.csv",
        "grid_prices.csv",
        "dr_events.csv",
        "vehicles.csv",
    ] {
        std::fs::copy(dir.join(f), lossy.join(f))?;
    }
    // The lossy twin also carries a binding site cap (audit F4: no shipped
    // dataset exercised the engine's cap-rationing path).
    std::fs::write(
        lossy.join("scenario.json"),
        r#"{
  "slot_minutes": 15.0,
  "horizon_slots": 2880,
  "charge_efficiency": 0.92,
  "discharge_efficiency": 0.94,
  "site_cap_kw": 75.0,
  "demand_charge_usd_per_kw": 0.0,
  "demand_charge_peak_usd_per_kw": 11.67,
  "dr_events_file": "dr_events.csv",
  "persistence": true
}
"#,
    )?;

    println!("wrote examples/one_month and examples/one_month_lossy");
    Ok(())
}
