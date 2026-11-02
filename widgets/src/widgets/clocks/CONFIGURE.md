# Configure `clocks`

This is configuration guidance for people and AI assistants. It is documentation, not an executable skill and not permission to change files or external services.

## Real source

The machine clock, IANA timezone data, and timers maintained by this process. Current weather, when latitude and longitude are set, comes from one Open-Meteo request for those coordinates. No API key.

## Settings owned here

The owned section is `clocks`.

Declared fields: `cities`, `weather_latitude`, `weather_longitude`, `weather_label`, `work_start_hour`, `work_end_hour`, `show_hints`, `work_days`, `pomodoro_enabled`, `pomodoro_focus_minutes`, `pomodoro_short_break_minutes`, `pomodoro_long_break_minutes`, `pomodoro_sessions_before_long_break`, `pomodoro_bell`, `pomodoro_notify`, `pomodoro_flash`, `pomodoro_flash_count`, `pomodoro_flash_gap`, `pomodoro_flash_rgb`

The field types, defaults, order, and inline help come from `settings.json` in this folder. Use the widget's settings screen (press `,`) instead of constructing JSON by hand.

## Safe configuration process

1. Read the machine timezone if useful. Ask which cities, work hours, and work days belong to the user; do not infer them from IP location. Ask whether current weather should be shown and, if so, for the latitude and longitude. Do not infer coordinates from IP, from a city name, or from a timezone. Leave both fields null when weather should stay off. A label is optional text and is not used to choose the place.
2. Read the resolved path shown by the settings screen and the current values before proposing changes.
3. Pomodoro lengths and notification choices are preferences, so ask rather than guessing.
4. Change only this widget's declared section. Keep secrets out of chat, logs, shell history, source files, and screenshots.
5. Save through the settings screen and leave it — the widget reloads itself on the way out, so no restart is needed. Then verify that the real source answers, or that the pane gives a specific reason why it cannot.

## Boundaries

Do not fabricate hostnames, account names, team names, repositories, prices, ports, credentials, or successful readings. If a value cannot be established from the local environment or the user's explicit instruction, ask.
