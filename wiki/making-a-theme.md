# Making a theme

[← wiki](README.md)

A theme is 16 colours, one for each **role** a widget asks for. Widgets never
name a colour themselves; they ask for `fg`, `muted`, `red` and so on, and the
theme in force answers.

There are two ways to use a theme you made:

| | Your own theme | A bundled theme |
|---|---|---|
| Where it lives | a file in `~/.config/opscope/themes/` | an entry in [`core/themes.json`](../core/themes.json) |
| Who sees it | you | everyone who installs opscope |
| What it needs | `fg` and `bg`; the rest is worked out | all 16 roles, a name and a group |
| Review | none | a pull request |

Start with your own file. It is the quickest way to try colours, and an entry
for the bundled list is the same colours with every role written out.

## Your own theme

1. Make the folder beside your `config.json`, if it is not there:
   `~/.config/opscope/themes/`. With `XDG_CONFIG_HOME` set, it is
   `$XDG_CONFIG_HOME/opscope/themes/`.
2. Add a file to that folder. The file's name is the theme's id, so
   `mine.json` is the theme `mine`:

   ```json
   { "name": "Mine", "fg": "#e6e1cf", "bg": "#1b1d24", "red": "#f07178", "green": "#aad94c" }
   ```

3. Press `ctrl-t` in any widget — the theme picker opens straight to it
   (or `ctrl-k`, then `↵` on **Theme…**). The picker lists your themes in
   a section of
   their own, **your themes**, between `auto`/`terminal` and the bundled
   themes. Move onto yours to preview it, and press `↵` to keep it. `esc`
   backs out of the picker. Opened from the palette, `esc` returns to the
   palette's commands; `esc` again closes it.

Only `fg` and `bg` are required. Any role you leave out is worked out from
those and from whichever hues you did give. A file whose id matches a bundled
theme replaces that theme for you.

Widgets read the folder when they start. Restart a widget, or the launcher,
to see a file you just added or changed.

A file that cannot be read is named in a notice (`!`) and left out. It never
stops a widget.

## The roles

| Role | Used for |
|---|---|
| `bg` | the pane's background |
| `dark` | title and status bars |
| `slot` | the selected row's tint |
| `fg` | main text |
| `muted` | secondary text, labels, hints |
| `dim` | decoration only: bar tracks, rules. Never text |
| `border` | frames and dividers |
| `accent` | the theme's highlight: titles, the cursor, the picker's frame |
| `onAccent` | text drawn on `accent` |
| `green` `blue` `yellow` `red` `cyan` `magenta` `orange` | meaning: ok, info, warning, failure, and the series in charts |

Two more fields are optional:

- **`light`:** `true` for a light background. In your own file it is
  worked out from `bg` when left out. In `core/themes.json` it defaults to
  whether the group is `light`, so write it out there.
- **`paintsBackground`:** `false` leaves the terminal's own background showing
  instead of painting `bg`. It defaults to `true`, so a light theme reads on a
  dark terminal.

### Contrast is handled for you

Every text role is moved, just far enough, to clear WCAG AA 4.5 on `bg`, on
`dark` and on the selected row. A colour that already reads is left exactly
as written. So you choose the hues; opscope keeps them readable. If a colour
of yours looks different in opscope than in your file, that is why: it did
not clear AA as written.

## Offering a theme to everyone

1. Try it as your own theme first, as above.
2. Fork the repo, and add an entry to the `themes` list in
   [`core/themes.json`](../core/themes.json). Write out all 16 roles, and
   add:
   - **`id`:** lowercase letters, digits and dashes. Not `auto` or
     `terminal`, which are settings, and not an id already in the file.
   - **`name`:** shown in the picker.
   - **`group`:** the picker section it is listed under (see below).
   - **`film`:** optional, for the hacker-films group: the film it is from.
   - **`light`**, and **`paintsBackground`** only when it should be `false`.
3. Run `cargo test` from the root.
4. Open a pull request with the checklist below in its description.

The picker reads this file, so an entry needs no code change to appear
there. The website is to read the same file. Until it does, a new theme
appears on the site only when the site's own list is updated.

### Groups

| `group` | Section | Themes now |
|---|---|---|
| `opscope` | opscope | 2 |
| `hacker` | hacker films | 9 |
| `soft` | soft & muted | 11 |
| `pastel` | pastel | 7 |
| `classic` | editor classics | 8 |
| `neon` | neon | 4 |
| `light` | light | 10 |

A theme that fits none of them can bring a new group. Add an entry to
`groups` (an `id` and the `label` the picker shows) in the same pull request,
and say why in it.

### What the tests check

- **`the_bundled_themes_file_is_well_formed`**, in `core/src/theme.rs`. It
  fails with one line per problem, naming the theme and the field: a missing
  role, a value that is not `#rrggbb`, a field name with a typo, a group not
  in `groups`, a duplicate or badly formed id. Without this check, a theme
  that did not parse would be left out quietly and never appear.
- **The contrast tests**, in the same file. Every text role, the lifted form
  used on tinted rows, and every step of the heat ramp are measured in every
  bundled theme. A colour that fails gets moved as described above, so these
  tests fail only if no move could make it read.

### Checklist for the pull request

```markdown
- [ ] Tried as my own theme with `ctrl-t`, in at least two widgets
- [ ] All 16 roles written out in `core/themes.json`
- [ ] `id` is new, lowercase, and not `auto` or `terminal`
- [ ] `group` is one of the groups, or the PR adds a new one and says why
- [ ] `light` set to match `bg`
- [ ] `cargo test` passes
- [ ] A screenshot of a widget in the theme
```
