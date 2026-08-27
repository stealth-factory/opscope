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

'use strict';

// Optional dependencies that do not match `os` / `cpu` / `libc` are
// skipped, and a failed optional fetch is a warning. Either way npm
// reports the install as successful and leaves a package that cannot
// run. Failing here is what turns that into a sentence at install
// time, which is when somebody is still looking.

const platform = require('./platform');

try {
  platform.requireInstalled();
} catch (err) {
  console.error(err.message);
  process.exit(1);
}
