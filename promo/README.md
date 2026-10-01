# Promo video

`gpui-mcp-promo.mp4` is a recording of a real session, not a mockup. An agent
script drives the [Ember showcase](../examples/runtime-showcase) through the
real `gpui-mcp` server over stdio. Every line in the video's console is an MCP
call that was actually made, with its real result and latency.

| Chapter | What the agent does | Tools |
| --- | --- | --- |
| See | discovers the app, reads the 151-node semantic tree, outlines every control in-app | `list_apps`, `get_ui_tree`, `highlight_elements` |
| Drive | opens a file, toggles the terminal dock, types into the editor, asserts on live state | `find_elements`, `click_element`, `wait_for_state`, `type_text`, `get_text_info` |
| Validate | audits for controls with no accessible name and buttons with no handler | `save_ui_snapshot`, `get_ui_tree`, `highlight_elements` |
| Edit live | adds the missing names and restyles the status bar with one atomic HTML/CSS edit, then re-audits | `get_live_document`, `preview_live_document`, `compare_screenshots`, `diff_current_ui` |
| Guard | tries an `onclick` injection and a stale-revision edit; both are rejected | `preview_live_document` |
| Measure | hovers the file tree and reads the frame report | `mark_frames`, `pointer_move`, `get_frame_report` |

The session also runs `start_video_recording` and `stop_video_recording`.

## Reproduce

Linux with X11 tooling (`Xvfb`, `xdotool`, `xwininfo`, `ffmpeg`), the CI
native dependencies, Python 3, and Node:

```console
promo/record.sh    # build, launch Ember under Xvfb, run promo/director.py, capture footage
promo/compose.sh   # composite promo/out/take into promo/out/gpui-mcp-promo.mp4
```

`record.sh` captures the window with `x11grab` and logs wall-clock times for
each call. `compose/prep.py` lines the log up with the footage using the
x11grab start time. Pointer glides are sped up (each `pointer_move` waits for a
software-rendered frame), and everything else plays in real time. The cursor
and click ripples are drawn from the logged `pointer_move` and `click_element`
coordinates, because injected GPUI input does not move the X cursor. Set
`CHROMIUM_PATH` if Playwright's bundled Chromium is not installed.

Frame times in the Measure chapter come from Mesa's CPU renderer on a headless
machine. With a real GPU, draw times are far lower.
