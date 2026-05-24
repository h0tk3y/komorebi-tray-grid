# komorebi state fixtures

Hand-crafted `komorebic state`–shaped JSON snapshots used by
`tests/state_mapping.rs`. They model the subset of fields documented in
`src/komorebi/types.rs`.

The fixtures aim to cover the cases listed in the plan:

| File                  | Scenario                                                          |
| --------------------- | ----------------------------------------------------------------- |
| `single_monitor.json` | One monitor with three workspaces (focused, gray, empty).         |
| `multi_monitor.json`  | Two monitors, each with their own focused workspace.              |
| `full_screen.json`    | Maximized window in one workspace, monocle container in another.  |
| `empty_trailing.json` | Two workspaces only — cells 2..8 must render as empty.            |

To regenerate fixtures from a live komorebi, run `komorebic state` and trim
the JSON to the fields used by the parser; unknown fields are ignored.
