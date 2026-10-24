# Configure `google-trends`

This is configuration guidance for people and AI assistants. It is documentation, not an executable skill and not permission to change files or external services.

## Real source

Google's public Trending Now RSS feed, one request per region per poll:
`https://trends.google.com/trending/rss?geo=<GEO>`. No account, cookie or API
key is involved, and nothing else is fetched - the Explore link the widget
copies is only ever copied.

What the widget adds itself - SEEN counts, `new since`, LEFT THE FEED - is kept
in `$XDG_STATE_HOME/opscope/google-trends/<GEO>.json` (or
`~/.local/state/opscope/google-trends/<GEO>.json`). Deleting a file there only
resets that region's counts; the next fetch is a first check.

## Settings owned here

The owned section is `google_trends`.

Declared fields: `geos`, `refresh`

The field types, defaults, order, and inline help come from `settings.json` in this folder. Use the widget's settings screen (press `,`) instead of constructing JSON by hand.

## Safe configuration process

1. Confirm `curl` is on `PATH`.
2. Ask which regions the user wants. Region codes are the ones Google's feed takes in `?geo=` - two-letter country codes such as `US`, `GB` or `JP`. Do not guess a region from a timezone or locale.
3. To check a code before saving it, fetch `https://trends.google.com/trending/rss?geo=<GEO>` and look at the status: `200` with an `<rss>` document is a code Google knows; `400` is one it rejects, and the widget will say so on screen.
4. Keep `refresh` at 60 seconds or more. Google refreshes the feed every few minutes; asking more often adds load and shows nothing new.
5. Save through the settings screen and leave it — the widget reloads itself on the way out, so no restart is needed. Then verify that the feed answers, or that the pane names a specific reason why it cannot.

## Boundaries

There are no secrets in this section. Do not fabricate trends, traffic figures, region codes or successful readings. `approx_traffic` is Google's bucket (`200+`, `5000+`), not a count, and must never be presented as one. If a value cannot be established from the user's explicit instruction, ask.
