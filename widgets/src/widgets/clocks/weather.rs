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

//! Current weather for one configured place.
//!
//! Open-Meteo, no key. The coordinates come from config on every pass.
//! A failed request clears the last reading before the pane draws again:
//! a number this request did not fetch is not the current temperature,
//! and a zero is not standing in for one.

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use chrono_tz::Tz;
use opscope_core as tc;

use crate::parse::{self, CityCoords, CurrentConditions};

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

/// What the pane can draw. `Ready` is the only arm with a temperature.
#[derive(Debug, Clone, PartialEq)]
pub enum View {
    Unset,
    Waiting,
    Ready(Reading),
    Failed(String),
}

/// A world-clock row's own conditions. `Glyph` is the only arm that came
/// back from a request. A miss is `Failed` and carries no temperature.
#[derive(Debug, Clone, PartialEq)]
pub enum CityFace {
    Pending,
    Glyph(i64),
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
struct WeatherState {
    err: String,
    reading: Option<Reading>,
    waiting: bool,
    cities: Vec<CityMark>,
}

#[derive(Debug, Clone, PartialEq)]
struct CityMark {
    key: String,
    outcome: CityOutcome,
}

#[derive(Debug, Clone, PartialEq)]
enum CityOutcome {
    Pending,
    Glyph(i64),
    Failed(String),
}

struct Target {
    key: String,
    action: TargetAction,
}

enum TargetAction {
    Fetch(f64, f64),
    Invalid(String),
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
            waiting: false,
            cities: Vec::new(),
        };
        publish(&mut state, event_for_place(place));
        state
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

    fn city_face(&self, key: &str) -> CityFace {
        match self.cities.iter().find(|mark| mark.key == key) {
            Some(mark) => match &mark.outcome {
                CityOutcome::Pending => CityFace::Pending,
                CityOutcome::Glyph(code) => CityFace::Glyph(*code),
                CityOutcome::Failed(why) => CityFace::Failed(why.clone()),
            },
            None => CityFace::Pending,
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
    let latitude = coordinate(&latitude, "latitude")?;
    let longitude = coordinate(&longitude, "longitude")?;
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
            Ok(Some(Place {
                latitude,
                longitude,
                label,
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

fn stop(state: &Mutex<WeatherState>, why: &str) {
    if let Ok(mut guard) = state.lock() {
        guard.reading = None;
        guard.waiting = false;
        guard.cities.clear();
        guard.err = format!("poller stopped: {why}");
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

/// Identity of one opted-in city. The label and the zone are not
/// coordinates; they only keep two rows that share a point distinct.
pub fn city_key(label: &str, zone: &str, latitude: f64, longitude: f64) -> String {
    format!("{label}\n{zone}\n{latitude:.6}\n{longitude:.6}")
}

/// Cities that opted into weather, in file order, duplicates skipped.
///
/// A `[label, zone]` pair is not here. An unknown zone is not here either:
/// the world clock drops it, and a request for a row that is not drawn
/// would be a reading with nowhere to put it.
fn city_targets(cfg: &serde_json::Value) -> Vec<Target> {
    let items = cfg
        .get("cities")
        .and_then(|v| v.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in items {
        let Ok(text) = serde_json::to_string(item) else {
            continue;
        };
        let Ok(spec) = parse::parse_city_entry(&text) else {
            continue;
        };
        if spec.zone.parse::<Tz>().is_err() {
            continue;
        }
        let (key, action) = match spec.coords {
            CityCoords::Absent => continue,
            CityCoords::At {
                latitude,
                longitude,
            } => (
                city_key(&spec.label, &spec.zone, latitude, longitude),
                TargetAction::Fetch(latitude, longitude),
            ),
            CityCoords::Invalid(why) => (
                format!("invalid\n{}\n{}\n{why}", spec.label, spec.zone),
                TargetAction::Invalid(why),
            ),
        };
        if seen.insert(key.clone()) {
            out.push(Target { key, action });
        }
    }
    out
}

fn remember_cities(state: &mut WeatherState, targets: &[Target]) {
    let keys: Vec<&str> = targets.iter().map(|t| t.key.as_str()).collect();
    state
        .cities
        .retain(|mark| keys.contains(&mark.key.as_str()));
    for target in targets {
        if state.cities.iter().any(|mark| mark.key == target.key) {
            continue;
        }
        let outcome = match &target.action {
            TargetAction::Invalid(why) => CityOutcome::Failed(why.clone()),
            TargetAction::Fetch(_, _) => CityOutcome::Pending,
        };
        state.cities.push(CityMark {
            key: target.key.clone(),
            outcome,
        });
    }
}

fn set_city(state: &mut WeatherState, key: &str, outcome: CityOutcome) {
    if let Some(mark) = state.cities.iter_mut().find(|mark| mark.key == key) {
        mark.outcome = outcome;
    } else {
        state.cities.push(CityMark {
            key: key.to_string(),
            outcome,
        });
    }
}

enum Attempt {
    Ready(CurrentConditions),
    Failed(String),
    Panic(String),
}

fn attempt(latitude: f64, longitude: f64) -> Attempt {
    let fetched = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        fetch_current(latitude, longitude)
    }));
    match fetched {
        Ok(Ok(conditions)) => Attempt::Ready(conditions),
        Ok(Err(why)) => Attempt::Failed(as_failure(why)),
        Err(payload) => Attempt::Panic(unwind_reason(payload)),
    }
}

fn step(state: &Mutex<WeatherState>, activity: &tc::Refresh) -> Step {
    let cfg = tc::load_config("clocks");
    let home = weather_place(&cfg);
    let targets = city_targets(&cfg);
    let keep_reading = match state.lock() {
        Ok(guard) => guard.reading.is_some(),
        Err(_) => return Step::Stop("the weather lock was poisoned".into()),
    };
    // A refresh keeps the last success on screen until this request
    // returns. Waiting is only the first request, when there is no reading.
    let home_event = match (&home, keep_reading) {
        (Ok(Some(_)), true) => None,
        (Ok(Some(_)), false) => Some(Event::Waiting),
        (Ok(None), _) => Some(Event::Unset),
        (Err(why), _) => Some(Event::Failed(why.clone())),
    };
    if let Some(event) = home_event {
        if lock_publish(state, event).is_err() {
            return Step::Stop("the weather lock was poisoned".into());
        }
    }
    if state
        .lock()
        .map(|mut guard| remember_cities(&mut guard, &targets))
        .is_err()
    {
        return Step::Stop("the weather lock was poisoned".into());
    }

    let mut fetches: Vec<(String, f64, f64)> = Vec::new();
    if let Ok(Some(place)) = &home {
        fetches.push(("home".into(), place.latitude, place.longitude));
    }
    for target in &targets {
        if let TargetAction::Fetch(latitude, longitude) = target.action {
            fetches.push((target.key.clone(), latitude, longitude));
        }
    }
    let mut attempts = Vec::new();
    if !fetches.is_empty() {
        let work = activity.start();
        for (key, latitude, longitude) in &fetches {
            attempts.push((key.clone(), attempt(*latitude, *longitude)));
        }
        drop(work);
    }

    let mut retry = matches!(home, Err(_))
        || targets
            .iter()
            .any(|target| matches!(target.action, TargetAction::Invalid(_)))
        || fetches.is_empty();
    let Ok(mut guard) = state.lock() else {
        return Step::Stop("the weather lock was poisoned".into());
    };
    for (key, attempt) in attempts {
        match attempt {
            Attempt::Panic(why) => return Step::Stop(why),
            Attempt::Failed(why) => {
                retry = true;
                if key == "home" {
                    publish(&mut guard, Event::Failed(why));
                } else {
                    set_city(&mut guard, &key, CityOutcome::Failed(why));
                }
            }
            Attempt::Ready(conditions) => {
                if key == "home" {
                    let label = match &home {
                        Ok(Some(place)) => place.label.clone(),
                        _ => String::new(),
                    };
                    publish(&mut guard, Event::Ready(reading_from(&label, conditions)));
                } else {
                    // The code is the glyph. The temperature was required
                    // to accept the body and is not stored: a city row has
                    // no number to invent a zero into.
                    set_city(&mut guard, &key, CityOutcome::Glyph(conditions.code));
                }
            }
        }
    }
    if retry {
        Step::Wait(RETRY_EVERY)
    } else {
        Step::Wait(SUCCESS_EVERY)
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

fn poll(state: Arc<Mutex<WeatherState>>, wake: Arc<(Mutex<bool>, Condvar)>, activity: tc::Refresh) {
    loop {
        let wait = match step(&state, &activity) {
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
    refresh: tc::Refresh,
}

impl Feed {
    pub fn start(cfg: &serde_json::Value) -> Self {
        let state = Arc::new(Mutex::new(WeatherState::from_place(weather_place(cfg))));
        let wake = Arc::new((Mutex::new(false), Condvar::new()));
        let refresh = tc::Refresh::default();
        let poller = Arc::clone(&state);
        let poller_wake = Arc::clone(&wake);
        let poller_activity = refresh.clone();
        std::thread::spawn(move || poll(poller, poller_wake, poller_activity));
        Self {
            state,
            wake,
            refresh,
        }
    }

    pub fn view(&self) -> View {
        match self.state.lock() {
            Ok(guard) => guard.view(),
            Err(_) => View::Failed("weather unavailable: the weather lock was poisoned".into()),
        }
    }

    /// The glyph for one opted-in city, or why that city's request failed.
    ///
    /// A key this pass does not know is `Pending`: no glyph, and no
    /// invented reading. The weather row, not this one, carries a poller
    /// that stopped.
    pub fn city_face(&self, key: &str) -> CityFace {
        match self.state.lock() {
            Ok(guard) => guard.city_face(key),
            Err(_) => CityFace::Failed("weather unavailable: the weather lock was poisoned".into()),
        }
    }

    /// Ask for a read now. Settings have just been saved, and the next
    /// scheduled wait may be a quarter of an hour away.
    pub fn wake(&self) {
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
    fn a_pair_is_not_a_weather_target() {
        let cfg = serde_json::json!({
            "cities": [
                ["Tokyo", "Asia/Tokyo"],
                ["Lisbon", "Europe/Lisbon", 38.72, -9.14],
                ["Gone", "Not/AZone", 1, 2],
                ["Origin", "Etc/UTC", 0, 0]
            ]
        });
        let keys: Vec<_> = city_targets(&cfg).into_iter().map(|t| t.key).collect();
        assert!(
            !keys.iter().any(|k| k.contains("Tokyo")),
            "a label and a zone are not a place: {keys:?}"
        );
        assert!(
            !keys.iter().any(|k| k.contains("Not/AZone")),
            "an unknown zone is not fetched: {keys:?}"
        );
        assert!(keys.contains(&city_key("Lisbon", "Europe/Lisbon", 38.72, -9.14)));
        assert!(keys.contains(&city_key("Origin", "Etc/UTC", 0.0, 0.0)));
    }

    #[test]
    fn a_failed_city_request_drops_its_glyph() {
        let mut state = WeatherState::from_place(Ok(None));
        let key = city_key("Lisbon", "Europe/Lisbon", 38.72, -9.14);
        set_city(&mut state, &key, CityOutcome::Glyph(2));
        assert_eq!(state.city_face(&key), CityFace::Glyph(2));
        set_city(
            &mut state,
            &key,
            CityOutcome::Failed("weather unavailable: offline".into()),
        );
        match state.city_face(&key) {
            CityFace::Failed(why) => {
                assert!(why.contains("offline"), "{why}");
                assert!(!why.contains('°'), "{why}");
                assert!(!why.contains("0.0"), "{why}");
            }
            other => panic!("expected the miss, got {other:?}"),
        }
        assert_eq!(
            state.city_face("other"),
            CityFace::Pending,
            "a city with no request has no glyph"
        );
    }

    #[test]
    fn one_city_failing_leaves_another_city_glyph() {
        let mut state = WeatherState::from_place(Ok(None));
        let lisbon = city_key("Lisbon", "Europe/Lisbon", 38.72, -9.14);
        let tokyo = city_key("Tokyo", "Asia/Tokyo", 35.68, 139.69);
        set_city(&mut state, &lisbon, CityOutcome::Glyph(1));
        set_city(&mut state, &tokyo, CityOutcome::Glyph(3));
        set_city(
            &mut state,
            &tokyo,
            CityOutcome::Failed("weather unavailable: offline".into()),
        );
        assert_eq!(state.city_face(&lisbon), CityFace::Glyph(1));
        assert!(matches!(state.city_face(&tokyo), CityFace::Failed(_)));
    }

    #[test]
    fn a_poller_stopping_clears_city_glyphs() {
        let state = Mutex::new(WeatherState::from_place(Ok(Some(Place {
            latitude: 1.0,
            longitude: 2.0,
            label: String::new(),
        }))));
        let key = city_key("Lisbon", "Europe/Lisbon", 38.72, -9.14);
        state.lock().unwrap().cities.push(CityMark {
            key: key.clone(),
            outcome: CityOutcome::Glyph(0),
        });
        stop(&state, "the request panicked");
        let guard = state.lock().unwrap();
        assert!(guard.city_face(&key) != CityFace::Glyph(0));
        assert!(guard.err.contains("poller stopped"));
        assert!(guard.reading.is_none());
        assert!(!guard.err.contains('°'));
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
}
