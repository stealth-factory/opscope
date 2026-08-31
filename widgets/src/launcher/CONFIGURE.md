# Configure the `opscope` launcher

This is plain documentation for a person or AI assistant. It is not an
executable skill and not permission to change files.

The launcher owns only settings shared by every widget. Today that is
`terminal.mouse`: whether opscope asks the terminal to report mouse-wheel
events.

## Safe process

1. Ask whether the user values wheel scrolling or drag-to-select terminal
   text more. Mouse reporting enables the first and prevents the second.
2. Press `,` in the launcher, review the resolved config path and current
   value, then toggle `mouse`.
3. Restart the launcher and widgets so terminal setup uses the new value.
4. Verify either that the wheel scrolls, or that dragging selects text when
   mouse reporting is disabled.

Do not change a widget's section here. Each widget owns its settings and its
own `CONFIGURE.md`.
