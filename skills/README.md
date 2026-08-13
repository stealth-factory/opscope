# Bundled skills

## `herdr/`

The Herdr control skill, so an agent working in this repo can drive panes, tabs
and workspaces without hunting for it. Drop it into `~/.claude/skills/herdr/`
to install.

It is a verbatim copy of what the installed Herdr binary emits, and is the work
of Herdr's authors rather than part of this project — the repository's AGPL
does not extend to it.

Bundled from **herdr 0.8.0**. It tracks the version it came from, and that version is the authority for
command syntax. Refresh it after upgrading Herdr:

```sh
herdr --skill > skills/herdr/SKILL.md
```

For what the skill does *not* cover — resize semantics, focus, detecting what a
pane runs, notification gating — see
[`../docs/building-herdr-panels.md`](../docs/building-herdr-panels.md).
