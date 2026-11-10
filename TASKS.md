# Tasks

Order = build order. Each task ends with the checks in `AGENTS.md` and an update of the skills. Delete a task
when done.

| # | Task | Done when |
|---|---|---|
| 10 | `dsc --resume` to start in the last conversation; delete a session from the list | the chat opens where it was left |
| 13 | Stream command output live while it runs | long commands show progress |
| 14 | Verify `--search` and `--expert` live; map their fragment types | both give an answer |
| 11 | `dsc login` over CDP (own Chrome profile) | fresh login without FreeDeepseekAPI |
| 12 | Fully static binary (musl) and an aarch64 build in the release workflow | the download runs on any Linux, also ARM |

## Open questions

- Binary name `dsc` ok?
