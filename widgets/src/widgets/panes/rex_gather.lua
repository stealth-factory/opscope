-- panes, Rex backend: one read of the whole server, on one connection.
--
-- Run as `rex --autostart=false do -e <this>`, locally in the CLI, and the
-- table it returns is printed as JSON. Read-only by construction: it calls
-- list, view, attach, status and the three reading block methods, and
-- check.rs (`every_rex_script_is_read_only`) fails the build if anything
-- else appears here. A method that fails is carried as text beside the
-- block it failed for, never raised, so one bad pane cannot blank the rest.
local out = { sessions = {}, errors = {} }
local list, err = rex.call("session.list")
if err ~= nil then error(err, 0) end
for _, s in ipairs(list.sessions or {}) do
  -- session.* methods answer only on a connection attached to that session.
  local _, aerr = rex.call("session.attach", { session_id = s.session_id })
  if aerr ~= nil then
    table.insert(out.errors, { session_id = s.session_id, attach_error = tostring(aerr) })
  end
  local view, verr = rex.call("session.view", { session_id = s.session_id })
  local entry = { session_id = s.session_id, label = s.label, view = view, blocks = {} }
  if verr ~= nil then entry.view_error = tostring(verr) end
  local blocks, berr = rex.call("session.list_blocks", { session_id = s.session_id })
  if berr ~= nil then entry.blocks_error = tostring(berr) end
  for _, b in ipairs((blocks and blocks.blocks) or {}) do
    local row = { block = b }
    if b.creator_name == "com.superlogical.terminal" then
      for _, m in ipairs({ "process", "title", "program_status" }) do
        local r, merr = rex.call("block.method", { session_id = s.session_id, block_id = b.block_id, method = m })
        if merr ~= nil then row[m .. "_error"] = tostring(merr) else row[m] = r end
      end
    end
    table.insert(entry.blocks, row)
  end
  table.insert(out.sessions, entry)
end
out.server = rex.call("server.status")
out.version = rex.call("server.version")
out.clients = rex.call("client.list")
return out
