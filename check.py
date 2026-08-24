#!/usr/bin/env python3
# terminal-toys - small dependency-free terminal widgets
# Copyright (C) 2026 William Li
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU Affero General Public License as published
# by the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
# GNU Affero General Public License for more details.
#
# You should have received a copy of the GNU Affero General Public License
# along with this program.  If not, see <https://www.gnu.org/licenses/>.
"""Checks for the failure modes these widgets actually had.

    python3 check.py

Every check here exists because something shipped broken. `compile()` catches
none of them: each is a runtime or presentation fault that looks, on screen,
exactly like "there is no data".
"""
import ast
import builtins
import glob
import json
import os
import re
import sys

os.chdir(os.path.dirname(os.path.abspath(__file__)))
# The library, the checker, the launcher and the directory's entry point are
# not widgets: none of them draws a panel, and none has a doc page of the
# shape these checks look for.
WIDGETS = [f for f in sorted(glob.glob("*.py"))
           if f not in ("common.py", "check.py", "start.py", "__main__.py")]
PROBLEMS = []


def fail(check, where, detail):
    PROBLEMS.append((check, where, detail))


def bound_names(tree):
    out = {"__file__", "__name__", "__doc__"}
    for n in ast.walk(tree):
        if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            out.add(n.name)
            args = getattr(n, "args", None)
            if args:
                for grp in (args.posonlyargs, args.args, args.kwonlyargs):
                    out |= {a.arg for a in grp}
                for a in (args.vararg, args.kwarg):
                    if a:
                        out.add(a.arg)
        elif isinstance(n, ast.Lambda):
            for grp in (n.args.posonlyargs, n.args.args, n.args.kwonlyargs):
                out |= {a.arg for a in grp}
        elif isinstance(n, (ast.Import, ast.ImportFrom)):
            out |= {(a.asname or a.name).split(".")[0] for a in n.names}
        elif isinstance(n, ast.Name) and isinstance(n.ctx, (ast.Store, ast.Del)):
            out.add(n.id)
        elif isinstance(n, ast.ExceptHandler) and n.name:
            out.add(n.name)
        elif isinstance(n, ast.Global):
            out |= set(n.names)
    return out


def check_unbound():
    """A name used but never bound raises NameError only when reached.

    deployments.py lost `config_token_warning` from its imports and its poll
    thread died on the first iteration for a day, showing an empty board.
    """
    for f in WIDGETS + ["common.py"]:
        tree = ast.parse(open(f).read())
        used = {n.id for n in ast.walk(tree)
                if isinstance(n, ast.Name) and isinstance(n.ctx, ast.Load)}
        for name in sorted(used - bound_names(tree) - set(dir(builtins))):
            fail("unbound name", f, name)


def check_poller_guarded():
    """A daemon thread that raises simply stops.

    The widget then shows no data and no error, which is indistinguishable
    from a source that genuinely has none.
    """
    for f in WIDGETS:
        src = open(f).read()
        if "threading.Thread" not in src:
            continue
        tree = ast.parse(src)
        ok = False
        for n in ast.walk(tree):
            if (isinstance(n, ast.FunctionDef) and n.name in ("run", "reader")
                    and any(isinstance(b, ast.Try) for b in n.body)):
                ok = True
        if not ok:
            fail("unguarded poller", f, "run()/reader() may die silently")


def check_config_keys():
    """A key in the example the widget never reads is a lie in a sample file."""
    try:
        example = json.load(open("config.example.json"))
    except (OSError, ValueError) as e:
        return fail("config.example.json", "-", str(e)[:60])
    # a section can be read by more than one widget - pr.py borrows github's
    # token - so a key is only dead if *nothing* reads it
    known = {}
    for f in WIDGETS:
        for m in re.finditer(r'load_config\(\s*"(\w+)"\s*,\s*\{(.*?)\n\}\)',
                             open(f).read(), re.S):
            known.setdefault(m.group(1), set())
            known[m.group(1)] |= set(re.findall(r'"(\w+)":', m.group(2)))
    # The port reads the same file, and has keys of its own. A setting the
    # Rust reads is not dead because the Python has not caught up - it would
    # only be dead if nothing at all read it, and this script can only see
    # half the tree. Read as text: a key counts as read if the widget
    # mentions it, which is the same rule the Rust check settled on after
    # two attempts that guessed at the variable name and got it wrong.
    ported = {}
    for section in known:
        found = set()
        for path in glob.glob("rust/widgets/src/bin/%s.rs" % section) + glob.glob(
            "rust/widgets/src/bin/%s/*.rs" % section
        ):
            try:
                found.add(open(path).read())
            except OSError:
                pass
        ported[section] = "\n".join(found)
    for section, keys in known.items():
        shipped = {k for k in example.get(section, {}) if not k.startswith("_")}
        for dead in sorted(shipped - keys):
            if '"%s"' % dead in ported.get(section, ""):
                continue
            fail("dead config key", section, dead)


def check_docs():
    """Every widget carries a doc page and a README row, or says why not."""
    readme = open("README.md").read()
    for f in WIDGETS:
        if f == "matrix.py":            # decorative, deliberately undocumented
            continue
        if not os.path.exists("docs/%s.md" % f[:-3]):
            fail("missing doc", f, "docs/%s.md" % f[:-3])
        if "`%s`" % f not in readme:
            fail("missing README row", f, "not in the widget table")


def check_keys_documented():
    """A documented key that does not exist teaches a lie; so does the reverse."""
    for f in WIDGETS:
        doc = "docs/%s.md" % f[:-3]
        if not os.path.exists(doc):
            continue
        src, text = open(f).read(), open(doc).read()
        handled = set(re.findall(r'key\s*(?:==|in\s*\()\s*[("]([a-z0-9])["\)]',
                                 src))
        # a footer hint is "[w]indow" or "[r]efresh": the bracket is followed
        # straight away by the rest of the word. Indexing like rows[0] is not.
        hinted = set(re.findall(r'\[([a-z0-9])\](?=[a-z])',
                                " ".join(re.findall(r'"([^"\n]*)"', src))))
        for k in sorted(hinted - set(re.findall(r'`([a-z0-9])`', text))):
            fail("undocumented key", f, "[%s] in the footer, not in %s" % (k, doc))


for fn in (check_unbound, check_poller_guarded, check_config_keys,
           check_docs, check_keys_documented):
    fn()

if not PROBLEMS:
    print("all checks pass across %d widgets" % len(WIDGETS))
    raise SystemExit(0)
width = max(len(p[0]) for p in PROBLEMS)
for check, where, detail in PROBLEMS:
    print("%-*s  %-18s %s" % (width, check, where, detail))
print("\n%d problem(s)" % len(PROBLEMS))
raise SystemExit(1)
