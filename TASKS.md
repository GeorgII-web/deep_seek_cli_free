# Tasks

Order = build order. Each task ends with the checks in `AGENTS.md` and an update of the skills. Delete a task
when done.

| # | Task | Done when |
|---|---|---|
| 10 | `dsc chat` REPL: several tasks in one conversation; transcript in `<home>/sessions/<id>.jsonl` (prompts, actions, results, session id, last message id) and `--resume` | a second task sees the first; resume continues after a restart |
| 13 | Display: hide the raw `dsc` block while streaming (the action line shows it already); stream command output live | no JSON noise in a run |
| 14 | Verify `--search` and `--expert` live; map their fragment types | both give an answer |
| 11 | `dsc login` over CDP (own Chrome profile) | fresh login without FreeDeepseekAPI |
| 12 | Release build (musl static), GitHub release workflow | `cargo build --release --target x86_64-unknown-linux-musl` gives one binary |

## Open questions

- Binary name `dsc` ok?
