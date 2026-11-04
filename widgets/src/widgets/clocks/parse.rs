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

//! The Open-Meteo current-conditions response, always compiled.
//!
//! Hourly, daily, and anything else in the body is ignored. This widget
//! draws the current observation or it draws nothing: a missing temperature
//! is an error, not a zero.

/// One current observation. `observed` is `HH:MM` from the response, or
/// empty when the timestamp is absent or not a clock time.
#[derive(Debug, Clone, PartialEq)]
pub struct CurrentConditions {
    pub celsius: f64,
    pub code: i64,
    pub observed: String,
}

/// Current conditions from an Open-Meteo forecast body.
///
/// `temperature_2m` has to be a finite number in the body. It is never
/// filled in: a missing field, a null, or a string is a refusal, including
/// when the rest of the object would have made a plausible zero.
pub fn parse_current_weather(body: &str) -> Result<CurrentConditions, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("response was not JSON ({e})"))?;
    if let Some(unit) = value
        .pointer("/current_units/temperature_2m")
        .and_then(|v| v.as_str())
    {
        if unit != "°C" && unit != "celsius" {
            return Err(format!("response temperature unit was {unit}, not °C"));
        }
    }
    let Some(current) = value.get("current") else {
        if let Some(reason) = value.get("reason").and_then(|v| v.as_str()) {
            let reason = reason.trim();
            if !reason.is_empty() {
                return Err(format!("source refused the request: {reason}"));
            }
        }
        return Err("response has no current conditions".into());
    };
    let Some(celsius) = current.get("temperature_2m").and_then(|v| v.as_f64()) else {
        return Err("response has no temperature".into());
    };
    if !celsius.is_finite() {
        return Err("temperature was not a finite number".into());
    }
    let Some(code) = current.get("weather_code").and_then(json_code) else {
        return Err("response has no weather code".into());
    };
    let observed = current
        .get("time")
        .and_then(|v| v.as_str())
        .map(observed_hhmm)
        .unwrap_or_default();
    Ok(CurrentConditions {
        celsius,
        code,
        observed,
    })
}

/// A WMO code, including a whole-number float. A fractional code is not
/// rounded into a nearby condition.
fn json_code(value: &serde_json::Value) -> Option<i64> {
    if let Some(n) = value.as_i64() {
        return Some(n);
    }
    let n = value.as_f64()?;
    if n.is_finite() && n.fract() == 0.0 && (-1_000_000.0..1_000_000.0).contains(&n) {
        Some(n as i64)
    } else {
        None
    }
}

/// `HH:MM` from an ISO local time, or empty when it is not one.
fn observed_hhmm(time: &str) -> String {
    let Some((_, rest)) = time.split_once('T') else {
        return String::new();
    };
    let bytes = rest.as_bytes();
    if bytes.len() >= 5
        && bytes[2] == b':'
        && bytes[..2].iter().all(u8::is_ascii_digit)
        && bytes[3..5].iter().all(u8::is_ascii_digit)
    {
        rest[..5].to_string()
    } else {
        String::new()
    }
}

/// The glyph for a WMO weather code. An unlisted code is a dot: inventing
/// a sky for a code the table does not name would be a fake condition
/// standing next to a real temperature.
pub fn condition_glyph(code: i64) -> &'static str {
    match code {
        0 | 1 => "☀",
        2 => "◑",
        3 => "☁",
        45 | 48 => "≡",
        51 | 53 | 55 | 61 | 63 | 65 | 80 | 81 | 82 => "☂",
        56 | 57 | 66 | 67 | 71 | 73 | 75 | 77 | 85 | 86 => "❄",
        95 | 96 | 99 => "⚡",
        _ => "·",
    }
}

/// The condition in words. An unlisted code is named by its number.
pub fn condition_name(code: i64) -> String {
    match code {
        0 => "clear",
        1 => "mainly clear",
        2 => "partly cloudy",
        3 => "overcast",
        45 => "fog",
        48 => "rime fog",
        51 => "light drizzle",
        53 => "drizzle",
        55 => "dense drizzle",
        56 => "light freezing drizzle",
        57 => "freezing drizzle",
        61 => "light rain",
        63 => "rain",
        65 => "heavy rain",
        66 => "light freezing rain",
        67 => "freezing rain",
        71 => "light snow",
        73 => "snow",
        75 => "heavy snow",
        77 => "snow grains",
        80 => "light showers",
        81 => "showers",
        82 => "violent showers",
        85 => "light snow showers",
        86 => "snow showers",
        95 => "thunderstorm",
        96 => "thunderstorm, slight hail",
        99 => "thunderstorm, heavy hail",
        _ => return format!("code {code}"),
    }
    .to_string()
}

/// One decimal and the unit, so a measured zero reads as `0.0°C` and a
/// temperature is not left to be guessed as Celsius or not.
pub fn format_celsius(celsius: f64) -> String {
    format!("{celsius:.1}°C")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn current(body: &str) -> CurrentConditions {
        parse_current_weather(body).unwrap_or_else(|e| panic!("parse failed: {e}"))
    }

    #[test]
    fn a_current_observation_keeps_its_temperature_code_and_clock() {
        let got = current(
            r#"{"current_units":{"temperature_2m":"°C"},"hourly":{"temperature_2m":[1,2]},"current":{"time":"2026-11-02T21:45","interval":900,"temperature_2m":7.4,"weather_code":2}}"#,
        );
        assert_eq!(got.celsius, 7.4);
        assert_eq!(got.code, 2);
        assert_eq!(got.observed, "21:45");
    }

    #[test]
    fn a_measured_zero_is_kept() {
        let got = current(
            r#"{"current":{"time":"2026-01-01T00:00:00Z","temperature_2m":0,"weather_code":0}}"#,
        );
        assert_eq!(got.celsius, 0.0);
        assert_eq!(got.code, 0);
        assert_eq!(got.observed, "00:00");
        assert_eq!(format_celsius(got.celsius), "0.0°C");
    }

    #[test]
    fn a_missing_temperature_is_not_read_as_zero() {
        let err = parse_current_weather(r#"{"current":{"weather_code":0}}"#).unwrap_err();
        assert!(err.contains("no temperature"), "{err}");
        assert!(!err.contains('0'));
    }

    #[test]
    fn a_null_or_string_temperature_is_refused() {
        for body in [
            r#"{"current":{"temperature_2m":null,"weather_code":1}}"#,
            r#"{"current":{"temperature_2m":"7","weather_code":1}}"#,
        ] {
            let err = parse_current_weather(body).unwrap_err();
            assert!(err.contains("no temperature"), "{err}");
        }
    }

    #[test]
    fn a_missing_code_is_refused() {
        let err = parse_current_weather(r#"{"current":{"temperature_2m":3.5}}"#).unwrap_err();
        assert!(err.contains("no weather code"), "{err}");
        assert!(!err.contains("3.5"));
    }

    #[test]
    fn a_fractional_code_is_not_rounded() {
        let err = parse_current_weather(r#"{"current":{"temperature_2m":3,"weather_code":1.5}}"#)
            .unwrap_err();
        assert!(err.contains("no weather code"), "{err}");
    }

    #[test]
    fn a_whole_float_code_is_a_code() {
        let got = current(r#"{"current":{"temperature_2m":3,"weather_code":2.0}}"#);
        assert_eq!(got.code, 2);
        assert!(got.observed.is_empty());
    }

    #[test]
    fn a_refusal_names_the_reason_and_not_json() {
        let err = parse_current_weather(
            r#"{"error":true,"reason":"Latitude must be between -90 and 90"}"#,
        )
        .unwrap_err();
        assert!(err.contains("Latitude must be between -90 and 90"), "{err}");
    }

    #[test]
    fn prose_is_not_a_forecast() {
        let err = parse_current_weather("nope").unwrap_err();
        assert!(err.contains("not JSON"), "{err}");
    }

    #[test]
    fn a_unit_other_than_celsius_is_refused() {
        let err = parse_current_weather(
            r#"{"current_units":{"temperature_2m":"°F"},"current":{"temperature_2m":32,"weather_code":0}}"#,
        )
        .unwrap_err();
        assert!(err.contains("°F"), "{err}");
    }

    #[test]
    fn a_time_without_a_clock_is_left_off() {
        let got = current(r#"{"current":{"time":"noon","temperature_2m":1,"weather_code":0}}"#);
        assert!(got.observed.is_empty());
    }

    #[test]
    fn glyphs_follow_the_code_and_an_unlisted_code_stays_unnamed() {
        assert_eq!(condition_glyph(0), "☀");
        assert_eq!(condition_name(0), "clear");
        assert_eq!(condition_glyph(2), "◑");
        assert_eq!(condition_name(2), "partly cloudy");
        assert_eq!(condition_glyph(3), "☁");
        assert_eq!(condition_glyph(45), "≡");
        assert_eq!(condition_glyph(61), "☂");
        assert_eq!(condition_glyph(71), "❄");
        assert_eq!(condition_glyph(95), "⚡");
        assert_eq!(condition_glyph(17), "·");
        assert_eq!(condition_name(17), "code 17");
    }

    #[test]
    fn below_zero_keeps_its_sign() {
        assert_eq!(format_celsius(-0.4), "-0.4°C");
    }
}
