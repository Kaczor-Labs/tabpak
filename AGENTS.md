# Working on Tabpak

Read the README first: what the app does, the map of the repo and the checks.

- **Timing is the product.** With Songsterr sound, a note must land where Songsterr's own player plays it. The rules in `src/chart.rs` (tempo list, fermatas, swing, grace notes, pickup measures, repeats) copy that player's behaviour, also where it looks odd: they were measured, not designed. Change one only with a tab on which Songsterr's player does otherwise.
- **Expected times come from Songsterr's player.** The numbers in `tests/golden/timing.jsonl` and in the timing tests of `src/chart.rs` are what the player gave the same tab. When one fails, fix the converter; replace a number only with a new measurement from the player.
- **A timing change is done when three checks agree:** the tests here, Songsterr's player on real songs, and the sound itself (a conversion with Songsterr sound prints at how many places the notes line up). The second needs the maintainers' private test kit; a PR made without it says that check did not run.
- **This repo is public.** Tabs and recordings belong to other people: tests use made-up tabs, and Songsterr's code, real tabs, audio and converted songs stay out of it. So do the names, paths and machines of the people working here, in code, commits, PRs and pictures.
- **Check the window by drawing it:** `examples/shots.rs` writes every state to a PNG file without opening a window. The README's `docs/song.png` is its `5-done` state, 1600 pixels wide.
- **Words in the window** are short and plain, as quickRS's are; an error says what failed and what to do next.
