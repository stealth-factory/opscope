# Changelog

Every release, and what changed in it. Generated from the commit subjects,
which is why they are worth writing carefully.

## [0.18.0] - 2026-09-13

### Features

- **launcher**: The version on the title row, and a pane that scrolls whole (#224)

## [0.17.0] - 2026-09-13

### Features

- **luvus**: The launcher on right-click, so the module has a way in (#221)

## [0.16.1] - 2026-09-13

### Bug Fixes

- **luvus**: The manifest at the repo root, so owner/repo installs (#216)

### CI

- Publish npm under `next` and move `latest` only once it installs (#219)

## [0.16.0] - 2026-09-13

### Bug Fixes

- **github**: Spend PR FLOW's width on the chart, not on wording (#210)
- **github-prs**: Colour each figure like the bars it stands beside (#214)

### Features

- **github**: Draw opened and merged in the last 24h beside PR FLOW (#208)
- **luvus**: The widgets as a Luvus module, with its panes generated (#206)

## [0.15.2] - 2026-09-13

### Bug Fixes

- **widgets**: Three toggles say what the next press does, and tailnet names its filter (#202)

## [0.15.1] - 2026-09-12

### Bug Fixes

- **widgets**: Hints name the next press, and the pane names its filters (#199)

### Documentation

- Take the plumbing out of the widget pages (#198)

## [0.15.0] - 2026-09-12

### Bug Fixes

- **github-prs**: Stop the search from timing out, and scroll the whole widget (#174)
- **github**: Say whose contributions the calendar counts (#178)
- **github**: Colour the merge rate the way round it is read (#186)
- **github-prs**: Count what was opened, and say whose age AGE is (#185)
- **core**: Lift the ramp's hot end where it lands on a tinted row (#192)

### Features

- **github-prs**: Draw merged-per-day beside opened, and the last 24 hours large (#168)
- **agent-usage**: Lanes on [+] for Cursor extra usage and Grok on-demand allowance (#169)
- **luvus-panes**: Read a Luvus session over UHP 1.0 (#183)
- **core**: Let a widget write a command's stdin (#191)
- **luvus-panes**: Draw resumable sessions, what is claimable, and the checkout (#190)
- **luvus-panes**: Say what a blocked agent is blocked on (#194)

### Miscellaneous

- **core**: Start OPS-101 — a stdin-capable run helper

### Performance

- **github-prs**: Put the two count requests in flight at once (#195)

### Tests

- Derive the widget count instead of restating it (#193)

## [0.14.0] - 2026-09-11

### Bug Fixes

- **agent-usage**: Renew Grok's token after it lapses, and keep the last live reading across the lapse (#162)

### Features

- **agent-usage**: Ask x.ai for Grok's quota by default (#160)

## [0.13.1] - 2026-09-09

### Bug Fixes

- **github-prs**: Keep the pane readable when GitHub search is slow (#157)

## [0.13.0] - 2026-09-05

### Features

- **agent-usage**: Price gpt-6-astra (#154)

## [0.12.0] - 2026-09-03

### Features

- **agent-usage**: Price gemini-3.8-flash (#152)

## [0.11.1] - 2026-09-02

### Bug Fixes

- **agent-usage**: Price claude-fable-5-1 on its own row (#150)

### Documentation

- Drop the Windows sentence from the platforms note

## [0.11.0] - 2026-09-01

### Features

- Add widget dependency declarations and doctor (#145)

## [0.10.0] - 2026-09-01

### CI

- Gate pull requests on macos (#143)

### Documentation

- **wiki**: Explain platform-specific widget code (#141)
- **wiki**: Complete widget contributor onboarding (#144)

### Features

- **ports**: Add macOS listener and traffic sources (#137)
- **link**: Add macOS TCP metrics (#139)

## [0.9.0] - 2026-08-31

### Features

- **netwatch**: Add macOS traffic monitoring (#132)
- **latency**: Add macOS ping support (#135)

## [0.8.1] - 2026-08-31

### Bug Fixes

- Point missing-config messages at the settings screen (#131)

## [0.8.0] - 2026-08-31

### Bug Fixes

- **vercel-deployments**: Scroll the complete list body (#127)
- **core**: Measure clipped text in terminal columns (#129)

### Features

- **ports**: Compile parsers on every target, acquire per OS (#115)

## [0.7.0] - 2026-08-31

### Features

- **months**: A month grid you can page through, with today in context (#98)

## [0.6.0] - 2026-08-31

### Bug Fixes

- **clocks**: Sort world-clock rows by this frame's offset (#113)

### Documentation

- Mark the Linux-only widgets, and a new compressed demo (#105)
- Say what is published, not what is not (#106)
- Drop the "Building the wall" section (#108)
- Npx is the official way, and it says so at the top (#109)

### Features

- Move settings into standalone widget packages (#76)

## [0.5.0] - 2026-08-28

### Bug Fixes

- **agent-usage**: Name Cursor lanes for what they show, not for its API fields (#93)

### Features

- The mouse wheel scrolls every widget, and the keys keep the selection (#95)

## [0.4.0] - 2026-08-28

### CI

- Open the release PR over REST so a repo-scoped token works (#85)

### Documentation

- Teach opscope <widget> instead of PATH-installing every binary (#86)

### Features

- Add github-actions, and standardise the three GitHub widgets (#78) — **breaking**

## [0.3.0] - 2026-08-27

### Bug Fixes

- Explain missing live quota per agent on the usage summary (#74)

### CI

- Open the release PR with GH_TOKEN when it is set (#83)

### Documentation

- Offer commercial licenses alongside AGPL (#81)

### Features

- Rename the usage widget to agent-usage (#75) — **breaking**

### Refactor

- **core**: Centralize shared widget helpers (#71)

## [0.2.2] - 2026-08-27

### Bug Fixes

- Publish packed dirs as local paths, not github shorthand (#68)

## [0.2.1] - 2026-08-27

### Bug Fixes

- Read packed package.json as a file, not a module (#66)

## [0.2.0] - 2026-08-27

### Features

- Rename the project, crates and launcher to opscope (#62) — **breaking**
- Ship the widgets through npm (#59)

## [0.1.3] - 2026-08-26

### Bug Fixes

- **ci**: Never cancel a run on main (#57)

### Documentation

- Record what the release automation cost to get right (#55)
- Make the download instructions actually lead somewhere (#56)

## [0.1.2] - 2026-08-26

### Bug Fixes

- **ci**: Do not offer a release for documentation alone (#53)

### Documentation

- Say that prebuilt binaries exist, and how a release is cut (#51)

## [0.1.1] - 2026-08-26

### Bug Fixes

- **ci**: Fetch git-cliff directly instead of through an installer action (#45)
- **ci**: Tidy what the release PR writes (#47)
- Ignore config backups, which the existing rules did not cover (#49)
- **ci**: Put the blank line back between the release notes and the rule (#50)

### CI

- Build and test every pull request (#48)

## [0.1.0] - 2026-08-26

The first tagged release, and the baseline the version numbers count from.

Everything before this point is the project finding its shape: ten fabricated
panels, then the slow replacement of each one with something wired to a real
source, then a port of the lot from Python to Rust. Roughly two hundred commits
of that, written before there was a changelog to write them for, and summarising
them line by line here would say less than this paragraph does.

What the release contains:

- **Fourteen widgets**, one binary each: `clocks`, `deployments`, `github`,
  `herdr-panes`, `latency`, `linear`, `link`, `matrix`, `netwatch`, `ports`,
  `pr`, `start`, `tailnet` and `usage`.
- **Self-contained binaries.** SQLite is compiled in; nothing is linked beyond
  the platform's own C runtime, and the build fails if that stops being true.
- **Three targets**: `x86_64-unknown-linux-gnu` (built against glibc 2.35),
  `aarch64-apple-darwin` and `x86_64-apple-darwin`.
- **Every number on screen is real.** `matrix` is the one deliberate exception
  and computes nothing on purpose.

From here the version is computed from conventional commit subjects, and this
file is written by the release machinery rather than by hand.
