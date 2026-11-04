// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Current weather for one configured place, and for each world-clock
//! city that names its own coordinates.
//!
//! Open-Meteo, no key. The coordinates come from config on every pass.
//! A failed request clears the last reading before the pane draws again:
//! a number this request did not fetch is not the current temperature,
//! and a zero is not standing in for one. A city fetch that fails does
//! not clear the others, and none of them is allowed to stop the clock.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use chrono_tz::Tz;
use opscope_core as tc;

use crate::parse::{self, CurrentConditions};

/// How long a successful observation stays up before the next request.
/// Open-Meteo's current block moves on a fifteen-minute step.
const SUCCESS_EVERY: Duration = Duration::from_secs(15 * 60);
/// A miss, a bad setting, or no coordinates yet. Short enough that a fix
/// shows up, long enough that a down network is not polled in a loop.
const RETRY_EVERY: Duration = Duration::from_secs(60);

/// The place to ask about. Both coordinates are required; neither is invented.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub latitude: f64,
    pub longitude: f64,
    pub label: String,
}

/// A latitude and longitude that were written down. Never inferred.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LatLon {
    pub latitude: f64,
    pub longitude: f64,
}

/// One `cities` row whose timezone the database knows.
///
/// `place` is set only for `[label, zone, lat, lon]` with both coordinates
/// usable. A pair is a clock and nothing else. `coord_error` is a row that
/// tried to opt in and cannot be fetched; the clock is still drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct CitySpec {
    pub name: String,
    pub zone: String,
    pub place: Option<LatLon>,
    pub coord_error: Option<String>,
}

/// What a world-clock row may draw. `Ready` is the only arm with a
/// condition, and it exists only after a response that carried a
/// temperature. The temperature stays off this row: the glyph is the reading.
#[derive(Debug, Clone, PartialEq)]
pub enum CityOutcome {
    Waiting,
    Ready { code: i64 },
    Failed(String),
}

/// One opted-in city, as the frame reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct CityWeather {
    pub name: String,
    pub zone: String,
    pub latitude: f64,
    pub longitude: f64,
    pub outcome: CityOutcome,
}

/// A city the poller asks about. Same identity the frame matches on.
#[derive(Debug, Clone, PartialEq)]
struct CityTarget {
    name: String,
    zone: String,
    latitude: f64,
    longitude: f64,
}

/// What the pane can draw. `Ready` is the only arm with a temperature.
#[derive(Debug, Clone, PartialEq)]
pub enum View {
    Unset,
    Waiting,
    Ready(Reading),
    Failed(String),
}

/// A reading that came back from a request on this run.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub label: String,
    pub celsius: f64,
    pub code: i64,
    pub observed: String,
}

#[derive(Debug)]
struct CitySlot {
    name: String,
    zone: String,
    latitude: f64,
    longitude: f64,
    outcome: CityOutcome,
    next: Instant,
}

#[derive(Debug)]
struct WeatherState {
    err: String,
    reading: Option<Reading>,
    /// The coordinates `reading` was fetched for. A later place does not
    /// keep showing the previous one while its own request is in flight.
    fetched_for: Option<(f64, f64)>,
    waiting: bool,
    cities: Vec<CitySlot>,
    place_next: Instant,
}

enum Event {
    Unset,
    Waiting,
    Ready(Reading),
    Failed(String),
}

impl WeatherState {
    fn from_place(place: Result<Option<Place>, String>) -> Self {
        let mut state = Self {
            err: String::new(),
            reading: None,
            fetched_for: None,
            waiting: false,
            cities: Vec::new(),
            place_next: Instant::now(),
        };
        publish(&mut state, event_for_place(place));
        state
    }

    fn from_config(cfg: &serde_json::Value) -> Self {
        let mut state = Self::from_place(weather_place(cfg));
        apply_targets(&mut state, &city_targets(cfg));
        state
    }

    fn cities(&self) -> Vec<CityWeather> {
        self.cities
            .iter()
            .map(|slot| CityWeather {
                name: slot.name.clone(),
                zone: slot.zone.clone(),
                latitude: slot.latitude,
                longitude: slot.longitude,
                outcome: slot.outcome.clone(),
            })
            .collect()
    }

    fn view(&self) -> View {
        // The error wins even if a reading were still stored. A failed
        // request is not a temperature.
        if !self.err.is_empty() {
            return View::Failed(self.err.clone());
        }
        if let Some(reading) = &self.reading {
            return View::Ready(reading.clone());
        }
        if self.waiting {
            View::Waiting
        } else {
            View::Unset
        }
    }
}

fn event_for_place(place: Result<Option<Place>, String>) -> Event {
    match place {
        Ok(None) => Event::Unset,
        Ok(Some(_)) => Event::Waiting,
        Err(why) => Event::Failed(why),
    }
}

fn publish(state: &mut WeatherState, event: Event) {
    match event {
        Event::Unset => {
            state.reading = None;
            state.fetched_for = None;
            state.waiting = false;
            state.err.clear();
        }
        Event::Waiting => {
            state.waiting = true;
            if state.reading.is_none() {
                state.err.clear();
            }
        }
        Event::Ready(reading) => {
            state.reading = Some(reading);
            state.waiting = false;
            state.err.clear();
        }
        Event::Failed(why) => {
            state.reading = None;
            state.fetched_for = None;
            state.waiting = false;
            state.err = why;
        }
    }
}

/// The configured place, or `Ok(None)` when weather is left off.
///
/// Null and a missing key are the same decision: do not fetch. Zero is a
/// real coordinate once it is written down, so it is not the stand-in for
/// "nobody has chosen a place".
pub fn weather_place(cfg: &serde_json::Value) -> Result<Option<Place>, String> {
    let latitude = cfg
        .get("weather_latitude")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let longitude = cfg
        .get("weather_longitude")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let label = cfg
        .get("weather_label")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    Ok(latlon_from(&latitude, &longitude)?.map(|place| Place {
        latitude: place.latitude,
        longitude: place.longitude,
        label,
    }))
}

/// Optional coordinates on one `cities` entry.
///
/// Two fields is a clock. Both coordinates absent is the same decision.
/// One without the other, or a number that is not on the globe, is an
/// error and is not fetched. Nothing in here reads the label or the zone.
pub fn city_coordinates(entry: &serde_json::Value) -> Result<Option<LatLon>, String> {
    let Some(row) = entry.as_array() else {
        return Ok(None);
    };
    if row.len() < 3 {
        return Ok(None);
    }
    let latitude = row.get(2).cloned().unwrap_or(serde_json::Value::Null);
    let longitude = row.get(3).cloned().unwrap_or(serde_json::Value::Null);
    latlon_from(&latitude, &longitude)
}

/// Cities whose zone the database knows, in the order they were written.
///
/// A zone that does not parse is left out, same as the clock. Coordinates
/// are taken only from the third and fourth fields. A label that is a city
/// someone could geocode is still just a label.
pub fn read_cities(cfg: &serde_json::Value) -> Vec<CitySpec> {
    // Absent and not-an-array are both "no rows". The clock decides
    // whether that means the built-in four; this function does not.
    let items = cfg
        .get("cities")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for entry in &items {
        let name = entry.get(0).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let zone = entry.get(1).and_then(|v| v.as_str()).unwrap_or("");
        let Ok(tz) = zone.parse::<Tz>() else {
            continue;
        };
        let (place, coord_error) = match city_coordinates(entry) {
            Ok(place) => (place, None),
            Err(why) => (None, Some(why)),
        };
        out.push(CitySpec {
            name,
            zone: tz.name().to_string(),
            place,
            coord_error,
        });
    }
    out
}

fn city_targets(cfg: &serde_json::Value) -> Vec<CityTarget> {
    read_cities(cfg)
        .into_iter()
        .filter_map(|row| {
            let place = row.place?;
            Some(CityTarget {
                name: row.name,
                zone: row.zone,
                latitude: place.latitude,
                longitude: place.longitude,
            })
        })
        .collect()
}

fn latlon_from(
    latitude: &serde_json::Value,
    longitude: &serde_json::Value,
) -> Result<Option<LatLon>, String> {
    let latitude = coordinate(latitude, "latitude")?;
    let longitude = coordinate(longitude, "longitude")?;
    match (latitude, longitude) {
        (None, None) => Ok(None),
        (None, Some(_)) => Err("weather: set latitude as well as longitude".into()),
        (Some(_), None) => Err("weather: set longitude as well as latitude".into()),
        (Some(latitude), Some(longitude)) => {
            if !(-90.0..=90.0).contains(&latitude) {
                return Err("weather: latitude must be between -90 and 90".into());
            }
            if !(-180.0..=180.0).contains(&longitude) {
                return Err("weather: longitude must be between -180 and 180".into());
            }
            Ok(Some(LatLon {
                latitude,
                longitude,
            }))
        }
    }
}

fn coordinate(value: &serde_json::Value, key: &str) -> Result<Option<f64>, String> {
    if value.is_null() {
        return Ok(None);
    }
    let n = if let Some(n) = value.as_f64() {
        n
    } else if let Some(text) = value.as_str() {
        match text.trim().parse::<f64>() {
            Ok(n) => n,
            Err(_) => return Err(format!("weather: {key} is not a number")),
        }
    } else {
        return Err(format!("weather: {key} is not a number"));
    };
    if !n.is_finite() {
        return Err(format!("weather: {key} is not a finite number"));
    }
    Ok(Some(n))
}

/// The current-conditions request for these coordinates. Nothing in the
/// URL is a place the widget chose; both numbers are the arguments.
pub fn weather_url(latitude: f64, longitude: f64) -> String {
    format!(
        "https://api.open-meteo.com/v1/forecast?latitude={latitude:.6}&longitude={longitude:.6}\
         &current=temperature_2m,weather_code&temperature_unit=celsius&timezone=auto&forecast_days=1"
    )
}

fn fetch_current(latitude: f64, longitude: f64) -> Result<CurrentConditions, String> {
    let body = tc::get(&weather_url(latitude, longitude), &[], 20)?;
    parse::parse_current_weather(&body)
}

fn as_failure(why: String) -> String {
    if why.starts_with("weather") {
        why
    } else {
        format!("weather unavailable: {why}")
    }
}

fn reading_from(label: &str, conditions: CurrentConditions) -> Reading {
    Reading {
        label: label.to_string(),
        celsius: conditions.celsius,
        code: conditions.code,
        observed: conditions.observed,
    }
}

enum Step {
    Wait(Duration),
    Stop(String),
}

fn same_target(slot: &CitySlot, target: &CityTarget) -> bool {
    slot.name == target.name
        && slot.zone == target.zone
        && slot.latitude == target.latitude
        && slot.longitude == target.longitude
}

fn same_city(a: &CityTarget, b: &CityTarget) -> bool {
    a.name == b.name && a.zone == b.zone && a.latitude == b.latitude && a.longitude == b.longitude
}

fn target_of(slot: &CitySlot) -> CityTarget {
    CityTarget {
        name: slot.name.clone(),
        zone: slot.zone.clone(),
        latitude: slot.latitude,
        longitude: slot.longitude,
    }
}

/// Keep a reading only while the configured city is still that city.
/// A new coordinate is a different request, so the glyph it used to have
/// comes off before the next one arrives.
///
/// One slot per city. A repeated row used to leave a second slot that
/// `set_city` never wrote, so it stayed due, `soonest` returned zero, and
/// the poller asked again on every pass.
fn apply_targets(state: &mut WeatherState, targets: &[CityTarget]) {
    let now = Instant::now();
    let mut pool = std::mem::take(&mut state.cities);
    let mut next = Vec::with_capacity(targets.len());
    for target in targets {
        if next.iter().any(|slot| same_target(slot, target)) {
            continue;
        }
        if let Some(at) = pool.iter().position(|slot| same_target(slot, target)) {
            next.push(pool.swap_remove(at));
        } else {
            next.push(CitySlot {
                name: target.name.clone(),
                zone: target.zone.clone(),
                latitude: target.latitude,
                longitude: target.longitude,
                outcome: CityOutcome::Waiting,
                next: now,
            });
        }
    }
    state.cities = next;
}

/// Write the outcome onto every slot for this city. One fetch is one
/// result: a twin left at `now` would make the next pass due immediately.
fn set_city(state: &mut WeatherState, target: &CityTarget, outcome: CityOutcome, wait: Duration) {
    let next = Instant::now() + wait;
    for slot in &mut state.cities {
        if same_target(slot, target) {
            slot.outcome = outcome.clone();
            slot.next = next;
        }
    }
}

/// Cities a pass should request. The same city twice is one request, even
/// when two slots are both due, so a duplicate cannot be fetched in a loop.
fn due_targets(state: &WeatherState, now: Instant) -> Vec<CityTarget> {
    let mut due = Vec::new();
    for slot in &state.cities {
        if slot.next > now {
            continue;
        }
        let target = target_of(slot);
        if due.iter().any(|have| same_city(have, &target)) {
            continue;
        }
        due.push(target);
    }
    due
}

fn stop(state: &Mutex<WeatherState>, why: &str) {
    if let Ok(mut guard) = state.lock() {
        guard.reading = None;
        guard.waiting = false;
        guard.err = format!("poller stopped: {why}");
        // The reason is the place row. Glyphs come off so a stopped poller
        // cannot keep drawing a condition it is no longer refreshing.
        for city in &mut guard.cities {
            city.outcome = CityOutcome::Waiting;
        }
    }
}

fn with_state<T>(
    state: &Mutex<WeatherState>,
    body: impl FnOnce(&mut WeatherState) -> T,
) -> Result<T, String> {
    match state.lock() {
        Ok(mut guard) => Ok(body(&mut guard)),
        Err(_) => Err("the weather lock was poisoned".into()),
    }
}

fn lock_publish(state: &Mutex<WeatherState>, event: Event) -> Result<(), ()> {
    match state.lock() {
        Ok(mut guard) => {
            publish(&mut guard, event);
            Ok(())
        }
        Err(_) => Err(()),
    }
}

fn schedule_place(state: &Mutex<WeatherState>, wait: Duration) -> Result<(), ()> {
    match state.lock() {
        Ok(mut guard) => {
            guard.place_next = Instant::now() + wait;
            Ok(())
        }
        Err(_) => Err(()),
    }
}

fn step(state: &Mutex<WeatherState>, activity: &tc::Refresh, force: &AtomicBool) -> Step {
    let force_now = force.swap(false, Ordering::Relaxed);
    let cfg = tc::load_config("clocks");
    let place = weather_place(&cfg);
    let targets = city_targets(&cfg);
    let due = match with_state(state, |st| {
        apply_targets(st, &targets);
        if force_now {
            let now = Instant::now();
            st.place_next = now;
            for city in &mut st.cities {
                city.next = now;
            }
        }
        let now = Instant::now();
        let place_due = st.place_next <= now;
        let cities = due_targets(st, now);
        (place_due, cities)
    }) {
        Ok(due) => due,
        Err(why) => return Step::Stop(why),
    };
    if due.0 {
        match place {
            Ok(None) => {
                if lock_publish(state, Event::Unset).is_err() || schedule_place(state, RETRY_EVERY).is_err()
                {
                    return Step::Stop("the weather lock was poisoned".into());
                }
            }
            Err(why) => {
                if lock_publish(state, Event::Failed(why)).is_err()
                    || schedule_place(state, RETRY_EVERY).is_err()
                {
                    return Step::Stop("the weather lock was poisoned".into());
                }
            }
            Ok(Some(place)) => {
                if let Step::Stop(why) = fetch_into(state, activity, place) {
                    return Step::Stop(why);
                }
            }
        }
    }
    for target in &due.1 {
        // A settings save landed mid-round. Leave the rest for the next
        // pass, which re-reads the file rather than finishing a list that
        // has just been replaced.
        if force.load(Ordering::Relaxed) {
            return Step::Wait(Duration::ZERO);
        }
        if let Step::Stop(why) = fetch_city(state, activity, target) {
            return Step::Stop(why);
        }
    }
    match with_state(state, soonest) {
        Ok(wait) => Step::Wait(wait),
        Err(why) => Step::Stop(why),
    }
}

fn soonest(state: &mut WeatherState) -> Duration {
    let now = Instant::now();
    let mut next = state.place_next;
    for city in &state.cities {
        if city.next < next {
            next = city.next;
        }
    }
    next.saturating_duration_since(now)
}

/// Drop a reading that belongs to another place, and say the row is
/// waiting when there is nothing left to show. The same place keeps its
/// number up until the refresh returns.
fn prepare_place_fetch(state: &mut WeatherState, place: &Place) {
    let here = (place.latitude, place.longitude);
    if state.fetched_for != Some(here) {
        state.reading = None;
        state.fetched_for = None;
    }
    if state.reading.is_none() {
        publish(state, Event::Waiting);
    }
}

fn fetch_into(state: &Mutex<WeatherState>, activity: &tc::Refresh, place: Place) -> Step {
    if state.lock().map(|mut guard| prepare_place_fetch(&mut guard, &place)).is_err() {
        return Step::Stop("the weather lock was poisoned".into());
    }
    let work = activity.start();
    let fetched = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        fetch_current(place.latitude, place.longitude)
    }));
    drop(work);
    match fetched {
        Ok(Ok(conditions)) => {
            let reading = reading_from(&place.label, conditions);
            let stored = match state.lock() {
                Ok(mut guard) => {
                    publish(&mut guard, Event::Ready(reading));
                    guard.fetched_for = Some((place.latitude, place.longitude));
                    Ok(())
                }
                Err(_) => Err(()),
            };
            if stored.is_err() || schedule_place(state, SUCCESS_EVERY).is_err() {
                return Step::Stop("the weather lock was poisoned".into());
            }
            Step::Wait(SUCCESS_EVERY)
        }
        Ok(Err(why)) => {
            if lock_publish(state, Event::Failed(as_failure(why))).is_err()
                || schedule_place(state, RETRY_EVERY).is_err()
            {
                return Step::Stop("the weather lock was poisoned".into());
            }
            Step::Wait(RETRY_EVERY)
        }
        Err(payload) => Step::Stop(unwind_reason(payload)),
    }
}

/// One city. A miss is that city's miss: the place, the other cities, and
/// the clock keep what they already had. A panic is recorded on the row
/// rather than taken as the poller dying, because the next city is a
/// different request. The previous outcome stays up until this request
/// returns, so a refresh does not blank a glyph that is still the last
/// real one.
fn fetch_city(state: &Mutex<WeatherState>, activity: &tc::Refresh, target: &CityTarget) -> Step {
    let present = match state.lock() {
        Ok(guard) => guard.cities.iter().any(|slot| same_target(slot, target)),
        Err(_) => return Step::Stop("the weather lock was poisoned".into()),
    };
    if !present {
        return Step::Wait(Duration::ZERO);
    }
    let work = activity.start();
    let fetched = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        fetch_current(target.latitude, target.longitude)
    }));
    drop(work);
    let (outcome, wait) = match fetched {
        Ok(Ok(conditions)) => {
            // `parse_current_weather` refuses a body with no finite
            // temperature, so this glyph is a reading and not a stand-in.
            (CityOutcome::Ready { code: conditions.code }, SUCCESS_EVERY)
        }
        Ok(Err(why)) => (CityOutcome::Failed(as_failure(why)), RETRY_EVERY),
        Err(payload) => (
            CityOutcome::Failed(format!("weather unavailable: {}", unwind_reason(payload))),
            RETRY_EVERY,
        ),
    };
    match with_state(state, |st| set_city(st, target, outcome, wait)) {
        Ok(()) => Step::Wait(wait),
        Err(why) => Step::Stop(why),
    }
}

fn unwind_reason(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        return (*text).to_string();
    }
    if let Some(text) = payload.downcast_ref::<String>() {
        return text.clone();
    }
    "see the pane it was started from".to_string()
}

fn poll(
    state: Arc<Mutex<WeatherState>>,
    wake: Arc<(Mutex<bool>, Condvar)>,
    activity: tc::Refresh,
    force: Arc<AtomicBool>,
) {
    loop {
        let wait = match step(&state, &activity, &force) {
            Step::Wait(wait) => wait,
            Step::Stop(why) => {
                stop(&state, &why);
                return;
            }
        };
        let (lock, cond) = &*wake;
        let Ok(asked) = lock.lock() else {
            stop(&state, "the wake lock was poisoned");
            return;
        };
        let mut asked = if *asked {
            asked
        } else {
            match cond.wait_timeout(asked, wait) {
                Ok((guard, _)) => guard,
                Err(_) => {
                    stop(&state, "the wake lock was poisoned while waiting");
                    return;
                }
            }
        };
        *asked = false;
    }
}

/// The background request, and the view the frame reads.
pub struct Feed {
    state: Arc<Mutex<WeatherState>>,
    wake: Arc<(Mutex<bool>, Condvar)>,
    force: Arc<AtomicBool>,
    refresh: tc::Refresh,
}

impl Feed {
    pub fn start(cfg: &serde_json::Value) -> Self {
        let state = Arc::new(Mutex::new(WeatherState::from_config(cfg)));
        let wake = Arc::new((Mutex::new(false), Condvar::new()));
        let force = Arc::new(AtomicBool::new(false));
        let refresh = tc::Refresh::default();
        let poller = Arc::clone(&state);
        let poller_wake = Arc::clone(&wake);
        let poller_activity = refresh.clone();
        let poller_force = Arc::clone(&force);
        std::thread::spawn(move || poll(poller, poller_wake, poller_activity, poller_force));
        Self {
            state,
            wake,
            force,
            refresh,
        }
    }

    pub fn view(&self) -> View {
        match self.state.lock() {
            Ok(guard) => guard.view(),
            Err(_) => View::Failed("weather unavailable: the weather lock was poisoned".into()),
        }
    }

    /// Opted-in cities. Empty when none were configured, or when the lock
    /// was poisoned — the place row carries that failure, and a glyph is
    /// not drawn from a state we can no longer read.
    pub fn cities(&self) -> Vec<CityWeather> {
        match self.state.lock() {
            Ok(guard) => guard.cities(),
            Err(_) => Vec::new(),
        }
    }

    /// Ask for a read now. Settings have just been saved, and the next
    /// scheduled wait may be a quarter of an hour away.
    pub fn wake(&self) {
        self.force.store(true, Ordering::Relaxed);
        let (lock, cond) = &*self.wake;
        if let Ok(mut asked) = lock.lock() {
            *asked = true;
            cond.notify_one();
        }
    }

    pub fn refresh_tick(&self) -> Option<usize> {
        self.refresh.tick(tc::animation_tick())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_and_null_coordinates_fetch_nothing() {
        assert!(weather_place(&serde_json::json!({})).unwrap().is_none());
        let cfg = serde_json::json!({
            "weather_latitude": null,
            "weather_longitude": null,
            "weather_label": "unused"
        });
        assert!(weather_place(&cfg).unwrap().is_none());
    }

    #[test]
    fn an_explicit_zero_is_a_place() {
        let place = weather_place(&serde_json::json!({
            "weather_latitude": 0,
            "weather_longitude": 0
        }))
        .unwrap()
        .unwrap();
        assert_eq!(place.latitude, 0.0);
        assert_eq!(place.longitude, 0.0);
        assert!(place.label.is_empty());
    }

    #[test]
    fn a_string_coordinate_parses_and_a_word_does_not() {
        let place = weather_place(&serde_json::json!({
            "weather_latitude": "51.5",
            "weather_longitude": "-0.12",
            "weather_label": "  Bank  "
        }))
        .unwrap()
        .unwrap();
        assert_eq!(place.latitude, 51.5);
        assert_eq!(place.longitude, -0.12);
        assert_eq!(place.label, "Bank");
        let err = weather_place(&serde_json::json!({
            "weather_latitude": "here",
            "weather_longitude": 1
        }))
        .unwrap_err();
        assert!(err.contains("latitude is not a number"), "{err}");
    }

    #[test]
    fn one_coordinate_is_not_enough() {
        let err = weather_place(&serde_json::json!({"weather_latitude": 10})).unwrap_err();
        assert!(err.contains("longitude"), "{err}");
        let err = weather_place(&serde_json::json!({"weather_longitude": 10})).unwrap_err();
        assert!(err.contains("latitude"), "{err}");
    }

    #[test]
    fn coordinates_outside_the_globe_are_not_sent() {
        let err = weather_place(&serde_json::json!({
            "weather_latitude": 90.1,
            "weather_longitude": 0
        }))
        .unwrap_err();
        assert!(err.contains("latitude"), "{err}");
        let err = weather_place(&serde_json::json!({
            "weather_latitude": 0,
            "weather_longitude": 180.1
        }))
        .unwrap_err();
        assert!(err.contains("longitude"), "{err}");
        assert!(weather_place(&serde_json::json!({
            "weather_latitude": -90,
            "weather_longitude": 180
        }))
        .unwrap()
        .is_some());
    }

    #[test]
    fn the_url_is_the_coordinates_it_was_given() {
        let url = weather_url(1.5, -2.25);
        assert!(url.starts_with("https://api.open-meteo.com/v1/forecast?"));
        assert!(url.contains("latitude=1.500000"));
        assert!(url.contains("longitude=-2.250000"));
        assert!(url.contains("current=temperature_2m,weather_code"));
        assert!(url.contains("temperature_unit=celsius"));
        assert!(!url.contains("hourly="));
        assert!(!url.contains("daily="));
        assert_ne!(weather_url(3.0, 4.0), url);
    }

    #[test]
    fn a_failed_request_drops_the_reading_it_had() {
        let mut state = WeatherState::from_place(Ok(Some(Place {
            latitude: 1.0,
            longitude: 2.0,
            label: String::new(),
        })));
        publish(
            &mut state,
            Event::Ready(Reading {
                label: "Here".into(),
                celsius: 12.5,
                code: 0,
                observed: "09:00".into(),
            }),
        );
        assert!(matches!(state.view(), View::Ready(_)));
        publish(
            &mut state,
            Event::Failed("weather unavailable: offline".into()),
        );
        match state.view() {
            View::Failed(why) => {
                assert!(why.contains("offline"), "{why}");
                assert!(!why.contains("12.5"), "{why}");
                assert!(!why.contains('°'));
            }
            other => panic!("expected the failure, got {other:?}"),
        }
    }

    #[test]
    fn a_measured_zero_stays_on_the_view() {
        let mut state = WeatherState::from_place(Ok(None));
        publish(
            &mut state,
            Event::Ready(Reading {
                label: String::new(),
                celsius: 0.0,
                code: 0,
                observed: String::new(),
            }),
        );
        match state.view() {
            View::Ready(reading) => assert_eq!(reading.celsius, 0.0),
            other => panic!("a fetched zero is a reading, got {other:?}"),
        }
    }

    #[test]
    fn waiting_and_unset_hold_no_reading() {
        let waiting = WeatherState::from_place(Ok(Some(Place {
            latitude: 1.0,
            longitude: 2.0,
            label: String::new(),
        })));
        assert_eq!(waiting.view(), View::Waiting);
        assert!(waiting.reading.is_none());
        let unset = WeatherState::from_place(Ok(None));
        assert_eq!(unset.view(), View::Unset);
        assert!(unset.err.is_empty());
    }

    fn tokyo() -> CityTarget {
        CityTarget {
            name: "Tokyo".into(),
            zone: "Asia/Tokyo".into(),
            latitude: 35.68,
            longitude: 139.69,
        }
    }

    #[test]
    fn a_pair_is_not_a_place_and_a_label_is_not_a_coordinate() {
        let cfg = serde_json::json!({
            "cities": [
                ["Tokyo", "Asia/Tokyo"],
                ["San Francisco", "America/Los_Angeles", null, null]
            ]
        });
        let rows = read_cities(&cfg);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.place.is_none()));
        assert!(rows.iter().all(|row| row.coord_error.is_none()));
        assert!(city_targets(&cfg).is_empty());
    }

    #[test]
    fn four_fields_opt_that_city_in_and_nowhere_else() {
        let cfg = serde_json::json!({
            "cities": [
                ["Tokyo", "Asia/Tokyo", "35.68", 139.69],
                ["London", "Europe/London"],
                ["Nope", "Not/AZone", 1, 2]
            ]
        });
        let rows = read_cities(&cfg);
        assert_eq!(rows.len(), 2, "an unknown zone is dropped");
        assert_eq!(rows[0].place.unwrap().latitude, 35.68);
        assert_eq!(rows[0].place.unwrap().longitude, 139.69);
        assert!(rows[1].place.is_none());
        let targets = city_targets(&cfg);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].name, "Tokyo");
        assert_eq!(targets[0].zone, "Asia/Tokyo");
    }

    #[test]
    fn one_city_coordinate_is_a_clock_with_an_error() {
        let cfg = serde_json::json!({
            "cities": [["London", "Europe/London", 51.5]]
        });
        let rows = read_cities(&cfg);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].place.is_none());
        let why = rows[0].coord_error.as_deref().unwrap();
        assert!(why.contains("longitude"), "{why}");
        assert!(!why.contains('°'), "{why}");
        assert!(city_targets(&cfg).is_empty());

        let cfg = serde_json::json!({
            "cities": [["London", "Europe/London", 91, 0]]
        });
        let rows = read_cities(&cfg);
        assert!(rows[0].place.is_none());
        assert!(rows[0].coord_error.as_deref().unwrap().contains("latitude"));
    }

    #[test]
    fn an_explicit_zero_on_a_city_is_a_place() {
        let cfg = serde_json::json!({
            "cities": [["Here", "Etc/UTC", 0, 0]]
        });
        let place = read_cities(&cfg)[0].place.unwrap();
        assert_eq!(place.latitude, 0.0);
        assert_eq!(place.longitude, 0.0);
    }

    #[test]
    fn a_failed_city_fetch_drops_its_glyph_and_leaves_the_place() {
        let mut state = WeatherState::from_place(Ok(Some(Place {
            latitude: 1.0,
            longitude: 2.0,
            label: "Here".into(),
        })));
        publish(
            &mut state,
            Event::Ready(Reading {
                label: "Here".into(),
                celsius: 12.5,
                code: 0,
                observed: "09:00".into(),
            }),
        );
        let city = tokyo();
        apply_targets(&mut state, &[city.clone()]);
        set_city(
            &mut state,
            &city,
            CityOutcome::Ready { code: 61 },
            SUCCESS_EVERY,
        );
        set_city(
            &mut state,
            &city,
            CityOutcome::Failed("weather unavailable: offline".into()),
            RETRY_EVERY,
        );
        match &state.cities[0].outcome {
            CityOutcome::Failed(why) => {
                assert!(why.contains("offline"), "{why}");
                assert!(!why.contains('°'), "{why}");
                assert!(!why.contains("0.0"), "{why}");
            }
            other => panic!("expected the city failure, got {other:?}"),
        }
        match state.view() {
            View::Ready(reading) => assert_eq!(reading.celsius, 12.5),
            other => panic!("the place reading stays, got {other:?}"),
        }
    }

    #[test]
    fn a_place_failure_leaves_a_city_glyph() {
        let mut state = WeatherState::from_place(Ok(Some(Place {
            latitude: 1.0,
            longitude: 2.0,
            label: String::new(),
        })));
        let city = tokyo();
        apply_targets(&mut state, &[city.clone()]);
        set_city(&mut state, &city, CityOutcome::Ready { code: 3 }, SUCCESS_EVERY);
        publish(
            &mut state,
            Event::Failed("weather unavailable: offline".into()),
        );
        assert!(matches!(state.view(), View::Failed(_)));
        assert!(matches!(
            state.cities[0].outcome,
            CityOutcome::Ready { code: 3 }
        ));
    }

    #[test]
    fn a_changed_coordinate_drops_the_previous_glyph() {
        let mut state = WeatherState::from_place(Ok(None));
        let city = tokyo();
        apply_targets(&mut state, &[city.clone()]);
        set_city(&mut state, &city, CityOutcome::Ready { code: 1 }, SUCCESS_EVERY);
        let mut moved = city.clone();
        moved.latitude = 35.7;
        apply_targets(&mut state, &[moved]);
        assert!(matches!(state.cities[0].outcome, CityOutcome::Waiting));
        assert_eq!(state.cities[0].latitude, 35.7);
    }

    #[test]
    fn a_changed_place_drops_the_previous_temperature() {
        let mut state = WeatherState::from_place(Ok(Some(Place {
            latitude: 51.5,
            longitude: -0.1,
            label: "London".into(),
        })));
        publish(
            &mut state,
            Event::Ready(Reading {
                label: "London".into(),
                celsius: 10.5,
                code: 2,
                observed: "11:30".into(),
            }),
        );
        state.fetched_for = Some((51.5, -0.1));
        let moved = Place {
            latitude: 35.7,
            longitude: 139.7,
            label: "Tokyo".into(),
        };
        prepare_place_fetch(&mut state, &moved);
        assert!(state.reading.is_none(), "the old temperature stayed on the new place");
        assert!(matches!(state.view(), View::Waiting));
        prepare_place_fetch(
            &mut state,
            &Place {
                latitude: 51.5,
                longitude: -0.1,
                label: "London".into(),
            },
        );
        assert!(state.reading.is_none());
        publish(
            &mut state,
            Event::Ready(Reading {
                label: "London".into(),
                celsius: 10.5,
                code: 2,
                observed: "11:30".into(),
            }),
        );
        state.fetched_for = Some((51.5, -0.1));
        prepare_place_fetch(
            &mut state,
            &Place {
                latitude: 51.5,
                longitude: -0.1,
                label: "London".into(),
            },
        );
        assert_eq!(state.reading.as_ref().map(|r| r.celsius), Some(10.5));
    }

    #[test]
    fn a_removed_city_is_not_kept() {
        let mut state = WeatherState::from_place(Ok(None));
        apply_targets(&mut state, &[tokyo()]);
        apply_targets(&mut state, &[]);
        assert!(state.cities.is_empty());
    }

    fn overdue(city: &CityTarget) -> CitySlot {
        CitySlot {
            name: city.name.clone(),
            zone: city.zone.clone(),
            latitude: city.latitude,
            longitude: city.longitude,
            outcome: CityOutcome::Waiting,
            next: Instant::now(),
        }
    }

    #[test]
    fn a_duplicate_city_is_one_slot_and_one_backed_off_request() {
        let mut state = WeatherState::from_place(Ok(None));
        let city = tokyo();
        apply_targets(&mut state, &[city.clone(), city.clone()]);
        assert_eq!(state.cities.len(), 1, "two identical rows are one fetch");
        // The place is due at construction. Push it out so the assertion
        // is about the city, which is the slot that used to stay at now.
        state.place_next = Instant::now() + SUCCESS_EVERY;
        set_city(
            &mut state,
            &city,
            CityOutcome::Failed("weather unavailable: offline".into()),
            RETRY_EVERY,
        );
        let wait = soonest(&mut state);
        assert!(
            wait > Duration::from_secs(30),
            "a duplicate must wait out the failure backoff, waited {wait:?}"
        );

        // The shape apply_targets used to leave: two due slots, one result.
        state.cities = vec![overdue(&city), overdue(&city)];
        state.place_next = Instant::now() + SUCCESS_EVERY;
        let due = due_targets(&state, Instant::now());
        assert_eq!(due.len(), 1, "two due copies are still one request");
        set_city(
            &mut state,
            &due[0],
            CityOutcome::Failed("weather unavailable: offline".into()),
            RETRY_EVERY,
        );
        assert!(
            state
                .cities
                .iter()
                .all(|slot| matches!(slot.outcome, CityOutcome::Failed(_))),
            "both copies take the result"
        );
        let wait = soonest(&mut state);
        assert!(
            wait > Duration::from_secs(30),
            "a twin left due would fetch again immediately, waited {wait:?}"
        );
    }

    #[test]
    fn two_different_cities_stay_two_requests() {
        let mut state = WeatherState::from_place(Ok(None));
        let tokyo = tokyo();
        let london = CityTarget {
            name: "London".into(),
            zone: "Europe/London".into(),
            latitude: 51.5,
            longitude: -0.12,
        };
        apply_targets(&mut state, &[tokyo.clone(), london.clone()]);
        assert_eq!(state.cities.len(), 2);
        state.place_next = Instant::now() + SUCCESS_EVERY;
        let due = due_targets(&state, Instant::now());
        assert_eq!(due.len(), 2);
        set_city(
            &mut state,
            &tokyo,
            CityOutcome::Ready { code: 1 },
            SUCCESS_EVERY,
        );
        let still = due_targets(&state, Instant::now());
        assert_eq!(still.len(), 1);
        assert_eq!(still[0].name, "London");
    }
}
